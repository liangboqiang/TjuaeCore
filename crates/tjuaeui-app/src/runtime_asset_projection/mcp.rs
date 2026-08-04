use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use tjuaeui_api_types::{
    AssetConfigurationValue, AssetPrimitiveValue, AssetPublicConfiguration, McpAssetConfiguration, McpAssetTransport,
    McpDefinition, McpTransportDefinition, PortablePackageRunner,
};
use tjuaeui_asset::{AssetError, RuntimeAssetDefinition, parse_mcp_definition};
use tjuaeui_db::models::McpServerRow;
use tjuaeui_db::{CreateMcpServerParams, IMcpServerRepository, UpdateMcpServerParams};
use tjuaeui_mcp::{McpConnectionTestService, McpServerTransport};

use super::{ProjectionMode, stable_identity, validate_runtime_name};

pub(super) struct McpProjection {
    repo: Arc<dyn IMcpServerRepository>,
    mode: ProjectionMode,
    runtime_id: String,
    previous: Option<McpServerRow>,
    replacement: Option<McpReplacement>,
    created_id: Option<String>,
    applied: bool,
}

struct McpReplacement {
    description: Option<String>,
    transport_type: &'static str,
    transport_configuration: String,
    owner_marker: String,
    enabled: bool,
}

pub(super) async fn prepare(
    asset: RuntimeAssetDefinition,
    mode: ProjectionMode,
    repo: Arc<dyn IMcpServerRepository>,
) -> Result<McpProjection, AssetError> {
    let (definition, persisted_transport, _) = validate_and_resolve(&asset)?;
    let previous = repo.find_by_name_any(&asset.projection_runtime_id).await?;
    let marker = owner_marker(&asset.projection_runtime_id, &asset.local_asset_id);
    if let Some(row) = previous.as_ref()
        && !is_owned_projection(row, &marker)
    {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_MCP_ID_COLLISION",
            message: format!("MCP {} 的内部运行时投影已由其他来源占用", asset.portable_runtime_id),
        });
    }
    if mode == ProjectionMode::Remove && previous.is_none() {
        return Err(AssetError::RuntimeProjectionFailed {
            code: "RUNTIME_MCP_PROJECTION_MISSING",
            message: format!("MCP {} 的运行时投影不存在", asset.portable_runtime_id),
        });
    }
    let replacement = (mode == ProjectionMode::Replace)
        .then(|| {
            Ok::<_, AssetError>(McpReplacement {
                description: definition.description,
                transport_type: persisted_transport.transport_type(),
                transport_configuration: persisted_transport
                    .to_config_json()
                    .map_err(|_| configuration_error("无法编码 MCP 公开 transport 配置"))?,
                owner_marker: marker,
                enabled: previous
                    .as_ref()
                    .is_none_or(|row| row.deleted_at.is_some() || row.enabled),
            })
        })
        .transpose()?;
    Ok(McpProjection {
        repo,
        mode,
        runtime_id: asset.projection_runtime_id,
        previous,
        replacement,
        created_id: None,
        applied: false,
    })
}

pub(super) fn validate(asset: &RuntimeAssetDefinition) -> Result<(), AssetError> {
    validate_and_resolve(asset).map(|_| ())
}

pub(super) async fn try_run(
    asset: &RuntimeAssetDefinition,
    connection_test: &McpConnectionTestService,
) -> Result<(), AssetError> {
    let (_, _, transient_transport) = validate_and_resolve(asset)?;
    let result = connection_test
        .test_connection_with_runtime_scope(
            &asset.portable_runtime_id,
            &transient_transport,
            Some(&asset.local_asset_id),
        )
        .await;
    if result.success {
        return Ok(());
    }
    let code = result.code.map_or("UNKNOWN", |code| code.as_str());
    Err(AssetError::RuntimeProjectionFailed {
        code: "RUNTIME_MCP_PROBE_FAILED",
        // 不回传底层 error/details，避免第三方服务把请求头或密钥反射到错误中。
        message: format!(
            "MCP {} 未通过 initialize/tools-list 探测（{code}）",
            asset.portable_runtime_id
        ),
    })
}

impl McpProjection {
    pub(super) async fn apply(&mut self) -> Result<(), AssetError> {
        match self.mode {
            ProjectionMode::Replace => self.apply_replace().await?,
            ProjectionMode::Remove => self.apply_remove().await?,
        }
        self.applied = true;
        Ok(())
    }

    async fn apply_replace(&mut self) -> Result<(), AssetError> {
        let replacement = self
            .replacement
            .as_ref()
            .ok_or_else(|| AssetError::InvalidState("缺少 MCP 替换投影".into()))?;
        match self.previous.as_ref() {
            Some(previous) => {
                self.repo
                    .update(
                        &previous.id,
                        UpdateMcpServerParams {
                            name: Some(&self.runtime_id),
                            description: Some(replacement.description.as_deref()),
                            enabled: Some(replacement.enabled),
                            transport_type: Some(replacement.transport_type),
                            transport_config: Some(&replacement.transport_configuration),
                            tools: Some(None),
                            original_json: Some(Some(&replacement.owner_marker)),
                            builtin: Some(false),
                            deleted_at: Some(None),
                        },
                    )
                    .await?;
            }
            None => {
                let created = self
                    .repo
                    .create(CreateMcpServerParams {
                        name: &self.runtime_id,
                        description: replacement.description.as_deref(),
                        enabled: replacement.enabled,
                        transport_type: replacement.transport_type,
                        transport_config: &replacement.transport_configuration,
                        tools: None,
                        original_json: Some(&replacement.owner_marker),
                        builtin: false,
                    })
                    .await?;
                self.created_id = Some(created.id);
            }
        }
        Ok(())
    }

    async fn apply_remove(&self) -> Result<(), AssetError> {
        let previous = self
            .previous
            .as_ref()
            .ok_or_else(|| AssetError::InvalidState("缺少 MCP 运行投影".into()))?;
        self.repo.delete(&previous.id).await?;
        Ok(())
    }

    pub(super) async fn rollback(&mut self) -> Result<(), AssetError> {
        if !self.applied {
            return Ok(());
        }
        match self.previous.as_ref() {
            Some(previous) => self.repo.restore_projection_row(previous).await?,
            None => {
                let created_id = self
                    .created_id
                    .as_deref()
                    .ok_or_else(|| AssetError::InvalidState("MCP 补偿事务缺少新建投影 ID".into()))?;
                self.repo.purge_projection_row(created_id).await?;
            }
        }
        self.applied = false;
        Ok(())
    }

    pub(super) fn finalize(&mut self) {
        self.created_id = None;
        self.applied = false;
    }
}

fn validate_and_resolve(
    asset: &RuntimeAssetDefinition,
) -> Result<(McpDefinition, McpServerTransport, McpServerTransport), AssetError> {
    validate_runtime_name(&asset.portable_runtime_id, "MCP runtimeId")?;
    let bytes = asset
        .files
        .iter()
        .find(|file| file.path == asset.entry_file)
        .map(|file| file.content.as_slice())
        .ok_or_else(|| AssetError::InvalidMetadata("MCP 入口文件不存在".into()))?;
    let definition = parse_mcp_definition(bytes)?;
    if definition.runtime_id != asset.portable_runtime_id {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_MCP_ID_MISMATCH",
            message: "mcp.json 的 runtimeId 与资产运行时身份不一致".into(),
        });
    }
    let (configuration, secrets) = match asset.runtime_configuration.as_ref() {
        Some(resolved) => match &resolved.configuration {
            AssetPublicConfiguration::Mcp(configuration) => (configuration.clone(), resolved.secrets.clone()),
            _ => return Err(configuration_kind_error()),
        },
        None => (default_configuration(&definition), BTreeMap::new()),
    };
    validate_configuration_schema(&definition, &configuration, &secrets)?;
    validate_transport_match(&definition.transport, configuration.transport)?;
    let persisted = build_transport(&definition, &configuration, &BTreeMap::new(), false)?;
    let transient = build_transport(&definition, &configuration, &secrets, true)?;
    Ok((definition, persisted, transient))
}

fn default_configuration(definition: &McpDefinition) -> McpAssetConfiguration {
    McpAssetConfiguration {
        transport: match &definition.transport {
            McpTransportDefinition::Stdio { .. } => McpAssetTransport::Stdio,
            McpTransportDefinition::Sse {} => McpAssetTransport::Sse,
            McpTransportDefinition::StreamableHttp {} => McpAssetTransport::StreamableHttp,
        },
        executable_path: None,
        arguments: Vec::new(),
        instance_url: None,
        environment: Vec::new(),
        headers: Vec::new(),
        values: Vec::new(),
        secrets: Vec::new(),
    }
}

fn build_transport(
    definition: &McpDefinition,
    configuration: &McpAssetConfiguration,
    secrets: &BTreeMap<String, String>,
    include_secrets: bool,
) -> Result<McpServerTransport, AssetError> {
    match &definition.transport {
        McpTransportDefinition::Stdio { package, arguments } => {
            let (command, mut args) = match configuration.executable_path.as_deref() {
                Some(path) => {
                    validate_command(path)?;
                    (path.to_owned(), Vec::new())
                }
                None => package_command(package),
            };
            args.extend(arguments.iter().cloned());
            args.extend(configuration.arguments.iter().cloned());
            let env = resolve_named_secrets(&configuration.environment, secrets, "环境变量", include_secrets)?;
            Ok(McpServerTransport::Stdio { command, args, env })
        }
        McpTransportDefinition::Sse {} => {
            let url = required_instance_url(configuration)?;
            let headers = resolve_named_secrets(&configuration.headers, secrets, "请求头", include_secrets)?;
            Ok(McpServerTransport::Sse { url, headers })
        }
        McpTransportDefinition::StreamableHttp {} => {
            let url = required_instance_url(configuration)?;
            let headers = resolve_named_secrets(&configuration.headers, secrets, "请求头", include_secrets)?;
            Ok(McpServerTransport::Http { url, headers })
        }
    }
}

fn package_command(package: &tjuaeui_api_types::PortableNpmPackageDefinition) -> (String, Vec<String>) {
    let pinned = format!("{}@{}", package.name, package.version);
    match package.runner {
        PortablePackageRunner::Bunx => ("bunx".into(), vec![pinned]),
        PortablePackageRunner::Npx => ("npx".into(), vec!["-y".into(), pinned]),
    }
}

fn resolve_named_secrets(
    bindings: &[tjuaeui_api_types::AssetNamedSecretSlot],
    secrets: &BTreeMap<String, String>,
    field: &str,
    include_secrets: bool,
) -> Result<HashMap<String, String>, AssetError> {
    let mut names = BTreeSet::new();
    let mut result = HashMap::new();
    for binding in bindings {
        if binding.name.trim().is_empty() || binding.name.contains(['\r', '\n', '\0']) {
            return Err(configuration_error(&format!("MCP {field}名称不合法")));
        }
        if field == "环境变量"
            && (!valid_environment_name(&binding.name) || is_blocked_runtime_environment_name(&binding.name))
        {
            return Err(configuration_error("MCP 环境变量名称不合法或属于运行时保留字段"));
        }
        if field == "请求头" && !valid_header_name(&binding.name) {
            return Err(configuration_error("MCP 请求头名称不合法"));
        }
        if !names.insert(binding.name.to_ascii_lowercase()) {
            return Err(configuration_error(&format!("MCP {field}名称不能重复")));
        }
        if !include_secrets {
            // 持久化 transport 刻意写入空 env/headers，不解析凭据。
            continue;
        }
        let value = secrets
            .get(&binding.secret_slot)
            .ok_or_else(|| AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_MCP_SECRET_MISSING",
                message: format!("MCP {field} {} 引用的凭据槽尚未配置", binding.name),
            })?;
        if field == "请求头" && value.contains(['\r', '\n', '\0']) {
            return Err(configuration_error("MCP 请求头值不合法"));
        }
        result.insert(binding.name.clone(), value.clone());
    }
    Ok(result)
}

fn valid_environment_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_blocked_runtime_environment_name(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    upper.starts_with("TJUAE_")
        || upper.starts_with("TJUAEUI_")
        || matches!(
            upper.as_str(),
            "HOME" | "PATH" | "USER" | "SHELL" | "TERM" | "CODEX_HOME"
        )
}

fn valid_header_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn validate_configuration_schema(
    definition: &McpDefinition,
    configuration: &McpAssetConfiguration,
    resolved_secrets: &BTreeMap<String, String>,
) -> Result<(), AssetError> {
    if !configuration.secrets.is_empty() {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_MCP_KEYED_SECRET_ADAPTER_REQUIRED",
            message: "通用 MCP adapter 不推断 Definition 密钥字段的注入协议".into(),
        });
    }
    match configuration.transport {
        McpAssetTransport::Stdio if !configuration.headers.is_empty() || configuration.instance_url.is_some() => {
            return Err(configuration_error("stdio MCP 不能配置 URL 或请求头"));
        }
        McpAssetTransport::Sse | McpAssetTransport::StreamableHttp
            if !configuration.environment.is_empty()
                || configuration.executable_path.is_some()
                || !configuration.arguments.is_empty() =>
        {
            return Err(configuration_error("远程 MCP 不能配置可执行文件、参数或环境变量"));
        }
        _ => {}
    }
    let values = configuration
        .values
        .iter()
        .map(|entry| (entry.key.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    if values.len() != configuration.values.len() {
        return Err(configuration_error("MCP 配置值 key 不能重复"));
    }
    if let Some(schema) = definition.configuration_schema.as_ref() {
        let known_fields = schema
            .fields
            .iter()
            .map(|field| field.key.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(unknown) = values.keys().find(|key| !known_fields.contains(*key)) {
            return Err(configuration_error(&format!(
                "MCP 配置包含 Definition 未声明的字段 {unknown}"
            )));
        }
        for field in &schema.fields {
            if field.secret {
                if field.required
                    && !configuration
                        .secrets
                        .iter()
                        .any(|binding| binding.key == field.key && resolved_secrets.contains_key(&binding.secret_slot))
                {
                    return Err(AssetError::RuntimeProjectionFailed {
                        code: "RUNTIME_MCP_SECRET_MISSING",
                        message: format!("MCP 必填凭据字段 {} 尚未配置", field.key),
                    });
                }
                continue;
            }
            match values.get(field.key.as_str()) {
                Some(value) => validate_value_type(value, field.value_type)?,
                None if field.required => {
                    return Err(configuration_error(&format!("MCP 必填配置字段 {} 缺失", field.key)));
                }
                None => {}
            }
        }
    } else if !values.is_empty() {
        return Err(configuration_error("MCP Definition 没有声明可配置字段"));
    }
    Ok(())
}

fn validate_value_type(
    value: &AssetConfigurationValue,
    expected: tjuaeui_api_types::AssetConfigurationValueType,
) -> Result<(), AssetError> {
    let matches = matches!(
        (&value.value, expected),
        (
            AssetPrimitiveValue::String(_),
            tjuaeui_api_types::AssetConfigurationValueType::String
        ) | (
            AssetPrimitiveValue::Number(_),
            tjuaeui_api_types::AssetConfigurationValueType::Number
        ) | (
            AssetPrimitiveValue::Boolean(_),
            tjuaeui_api_types::AssetConfigurationValueType::Boolean
        )
    );
    matches
        .then_some(())
        .ok_or_else(|| configuration_error(&format!("配置字段 {} 的类型不匹配", value.key)))
}

fn validate_transport_match(
    definition: &McpTransportDefinition,
    configured: McpAssetTransport,
) -> Result<(), AssetError> {
    let matches = matches!(
        (definition, configured),
        (McpTransportDefinition::Stdio { .. }, McpAssetTransport::Stdio)
            | (McpTransportDefinition::Sse {}, McpAssetTransport::Sse)
            | (
                McpTransportDefinition::StreamableHttp {},
                McpAssetTransport::StreamableHttp
            )
    );
    matches
        .then_some(())
        .ok_or_else(|| configuration_error("MCP Overlay transport 与 Definition 不一致"))
}

fn required_instance_url(configuration: &McpAssetConfiguration) -> Result<String, AssetError> {
    let value = configuration
        .instance_url
        .as_deref()
        .ok_or_else(|| configuration_error("远程 MCP 必须配置 instanceUrl"))?;
    let url = reqwest::Url::parse(value).map_err(|_| configuration_error("MCP instanceUrl 不是有效 URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(configuration_error("MCP instanceUrl 只允许 HTTP(S)"));
    }
    Ok(url.to_string())
}

fn validate_command(value: &str) -> Result<(), AssetError> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(configuration_error("MCP 启动命令不能为空或包含 NUL"));
    }
    Ok(())
}

fn owner_marker(projection_runtime_id: &str, local_asset_id: &str) -> String {
    serde_json::json!({
        "$tjuaeAsset": {
            "id": stable_identity(projection_runtime_id),
            "kind": "mcp",
            "tjuaeLocalAssetId": local_asset_id
        }
    })
    .to_string()
}

fn is_owned_projection(row: &McpServerRow, marker: &str) -> bool {
    row.original_json.as_deref() == Some(marker) && !row.builtin
}

fn configuration_kind_error() -> AssetError {
    AssetError::RuntimeProjectionUnsupported {
        code: "RUNTIME_CONFIGURATION_KIND_MISMATCH",
        message: "本机配置类型与 MCP 资产不匹配".into(),
    }
}

fn configuration_error(message: &str) -> AssetError {
    AssetError::RuntimeProjectionUnsupported {
        code: "RUNTIME_MCP_CONFIGURATION_INVALID",
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjuaeui_api_types::AssetKind;
    use tjuaeui_asset::{AssetDefinitionFile, RuntimeResolvedConfiguration};

    fn fixture_asset() -> RuntimeAssetDefinition {
        RuntimeAssetDefinition {
            local_asset_id: "mcp:contract-mcp".into(),
            kind: AssetKind::Mcp,
            portable_runtime_id: "contract-mcp".into(),
            projection_runtime_id: format!("tjuae-proj-v1-{}", "2".repeat(64)),
            entry_file: "mcp.json".into(),
            workspace_path: "mcps/contract-mcp".into(),
            files: vec![AssetDefinitionFile::text(
                "mcp.json",
                include_str!("../../../tjuaeui-asset/tests/fixtures/mcp-definition.v1.complete.json"),
            )],
            dependency_portable_runtime_ids: BTreeMap::new(),
            dependency_projection_runtime_ids: BTreeMap::new(),
            runtime_configuration: Some(RuntimeResolvedConfiguration {
                configuration: AssetPublicConfiguration::Mcp(McpAssetConfiguration {
                    transport: McpAssetTransport::Stdio,
                    executable_path: None,
                    arguments: Vec::new(),
                    instance_url: None,
                    environment: Vec::new(),
                    headers: Vec::new(),
                    values: vec![AssetConfigurationValue {
                        key: "workspace".into(),
                        value: AssetPrimitiveValue::String("demo".into()),
                    }],
                    secrets: Vec::new(),
                }),
                configuration_schema: parse_mcp_definition(include_bytes!(
                    "../../../tjuaeui-asset/tests/fixtures/mcp-definition.v1.complete.json"
                ))
                .unwrap()
                .configuration_schema
                .unwrap_or_default(),
                secrets: BTreeMap::new(),
            }),
        }
    }

    #[test]
    fn persisted_stdio_transport_has_empty_environment() {
        let (_, persisted, _) = validate_and_resolve(&fixture_asset()).unwrap();
        assert_eq!(
            persisted,
            McpServerTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@tjuae/contract-mcp@1.3.0".into(), "--stdio".into()],
                env: HashMap::new(),
            }
        );
    }

    #[test]
    fn secret_header_is_only_present_in_transient_transport() {
        let mut asset = fixture_asset();
        let resolved = asset.runtime_configuration.as_mut().unwrap();
        let AssetPublicConfiguration::Mcp(configuration) = &mut resolved.configuration else {
            unreachable!()
        };
        configuration.transport = McpAssetTransport::StreamableHttp;
        configuration.instance_url = Some("https://example.invalid/mcp".into());
        configuration.values.clear();
        configuration.headers.push(tjuaeui_api_types::AssetNamedSecretSlot {
            name: "Authorization".into(),
            secret_slot: "auth".into(),
        });
        resolved.secrets.insert("auth".into(), "Bearer secret".into());
        let raw = serde_json::json!({
            "$schema": "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/mcp-definition.v1.schema.json",
            "schemaVersion": 1,
            "kind": "mcp",
            "id": "contract-mcp",
            "runtimeId": "contract-mcp",
            "displayName": "Contract MCP",
            "transport": {"type": "streamableHttp"}
        });
        asset.files = vec![AssetDefinitionFile::text("mcp.json", raw.to_string())];
        let (_, persisted, transient) = validate_and_resolve(&asset).unwrap();
        assert!(matches!(persisted, McpServerTransport::Http { ref headers, .. } if headers.is_empty()));
        assert!(
            matches!(transient, McpServerTransport::Http { ref headers, .. } if headers.get("Authorization").map(String::as_str) == Some("Bearer secret"))
        );
    }
}
