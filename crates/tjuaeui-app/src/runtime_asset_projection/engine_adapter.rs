use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tjuaeui_ai_agent::{AgentRegistry, probe_engine_adapter_in_directory};
use tjuaeui_api_types::{
    AssetConfigurationValue, AssetKind, AssetPrimitiveValue, AssetPublicConfiguration, EngineAdapterAssetConfiguration,
    EngineAdapterDefinition, EngineAdapterProbeResponse,
};
use tjuaeui_asset::{AssetError, RuntimeAssetDefinition, parse_engine_adapter_definition};
use tjuaeui_db::IAgentMetadataRepository;
use tjuaeui_db::models::{AgentMetadataRow, UpsertAgentMetadataParams};

use super::{ProjectionMode, stable_identity, validate_runtime_name};

const ENGINE_SORT_ORDER: i64 = 3_500;

pub(super) struct EngineProjection {
    repo: Arc<dyn IAgentMetadataRepository>,
    registry: Arc<AgentRegistry>,
    mode: ProjectionMode,
    runtime_id: String,
    previous: Option<AgentMetadataRow>,
    replacement: Option<EngineReplacement>,
    applied: bool,
}

struct EngineReplacement {
    icon: Option<String>,
    name: String,
    description: Option<String>,
    source_info: String,
    command: String,
    arguments: String,
    native_skills_dirs: Option<String>,
    enabled: bool,
    sort_order: i64,
}

pub(super) struct EngineLaunchSpec {
    command: String,
    arguments: Vec<String>,
    environment: HashMap<String, String>,
    working_directory: Option<PathBuf>,
}

pub(super) async fn prepare(
    asset: RuntimeAssetDefinition,
    mode: ProjectionMode,
    repo: Arc<dyn IAgentMetadataRepository>,
    registry: Arc<AgentRegistry>,
) -> Result<EngineProjection, AssetError> {
    let (definition, spec) = validate_and_resolve(&asset)?;
    let previous = repo.get(&asset.projection_runtime_id).await?;
    let owner = owner_marker(&asset.projection_runtime_id);
    if let Some(row) = previous.as_ref()
        && !is_owned_projection(row, &owner)
    {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ENGINE_ID_COLLISION",
            message: format!("引擎 {} 的内部运行时投影已由其他来源占用", asset.portable_runtime_id),
        });
    }
    if mode == ProjectionMode::Remove && previous.is_none() {
        return Err(AssetError::RuntimeProjectionFailed {
            code: "RUNTIME_ENGINE_PROJECTION_MISSING",
            message: format!("引擎 {} 的运行时投影不存在", asset.portable_runtime_id),
        });
    }

    let replacement = (mode == ProjectionMode::Replace)
        .then(|| {
            let source_info = serde_json::json!({
                "binary_name": &spec.command,
                "hub_package_id": owner,
                "tjuaeLocalAssetId": &asset.local_asset_id,
            })
            .to_string();
            let native_skills_dirs = definition
                .capabilities
                .as_ref()
                .map(|capabilities| serde_json::to_string(&capabilities.skills_directories))
                .transpose()?;
            Ok::<_, AssetError>(EngineReplacement {
                icon: definition.icon,
                name: definition.display_name,
                description: definition.description,
                source_info,
                command: spec.command,
                arguments: serde_json::to_string(&spec.arguments)?,
                native_skills_dirs,
                enabled: previous.as_ref().is_none_or(|row| row.enabled),
                sort_order: previous.as_ref().map_or(ENGINE_SORT_ORDER, |row| row.sort_order),
            })
        })
        .transpose()?;

    Ok(EngineProjection {
        repo,
        registry,
        mode,
        runtime_id: asset.projection_runtime_id,
        previous,
        replacement,
        applied: false,
    })
}

pub(super) fn validate(asset: &RuntimeAssetDefinition) -> Result<(), AssetError> {
    validate_and_resolve(asset).map(|_| ())
}

pub(super) async fn try_run(asset: &RuntimeAssetDefinition) -> Result<(), AssetError> {
    let (_, spec) = validate_and_resolve(asset)?;
    match probe_engine_adapter_in_directory(
        &spec.command,
        &spec.arguments,
        &spec.environment,
        spec.working_directory.as_deref(),
        None,
    )
    .await
    {
        EngineAdapterProbeResponse::Success => Ok(()),
        EngineAdapterProbeResponse::FailCli { .. } => Err(AssetError::RuntimeProjectionFailed {
            code: "RUNTIME_ENGINE_COMMAND_UNAVAILABLE",
            message: format!("引擎 {} 的启动命令不可用", asset.portable_runtime_id),
        }),
        EngineAdapterProbeResponse::FailAcp { .. } => Err(AssetError::RuntimeProjectionFailed {
            code: "RUNTIME_ENGINE_ACP_PROBE_FAILED",
            message: format!(
                "引擎 {} 未通过 ACP initialize/session-new 探测",
                asset.portable_runtime_id
            ),
        }),
        EngineAdapterProbeResponse::FailAuth { .. } => Err(AssetError::RuntimeProjectionFailed {
            code: "RUNTIME_ENGINE_AUTH_REQUIRED",
            message: format!("引擎 {} 可连接，但需要先完成认证", asset.portable_runtime_id),
        }),
    }
}

impl EngineProjection {
    pub(super) async fn apply(&mut self) -> Result<(), AssetError> {
        let result = match self.mode {
            ProjectionMode::Replace => self.apply_replace().await,
            ProjectionMode::Remove => self.apply_remove().await,
        };
        if let Err(error) = result {
            self.restore_previous().await?;
            return Err(error);
        }
        self.applied = true;
        Ok(())
    }

    async fn apply_replace(&self) -> Result<(), AssetError> {
        let replacement = self
            .replacement
            .as_ref()
            .ok_or_else(|| AssetError::InvalidState("缺少引擎替换投影".into()))?;
        self.repo
            .upsert(&UpsertAgentMetadataParams {
                id: &self.runtime_id,
                icon: replacement.icon.as_deref(),
                name: &replacement.name,
                name_i18n: None,
                description: replacement.description.as_deref(),
                description_i18n: None,
                backend: None,
                agent_type: "acp",
                agent_source: "asset",
                agent_source_info: Some(&replacement.source_info),
                enabled: replacement.enabled,
                command: Some(&replacement.command),
                args: Some(&replacement.arguments),
                // 明文凭据只用于 probe；旧表只保存一个可公开的空环境。
                env: Some("[]"),
                native_skills_dirs: replacement.native_skills_dirs.as_deref(),
                behavior_policy: None,
                yolo_id: None,
                agent_capabilities: None,
                auth_methods: None,
                config_options: None,
                available_modes: None,
                available_models: None,
                available_commands: None,
                sort_order: replacement.sort_order,
            })
            .await?;
        self.reload_required().await
    }

    async fn apply_remove(&self) -> Result<(), AssetError> {
        if !self.repo.delete(&self.runtime_id).await? {
            return Err(AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_ENGINE_PROJECTION_MISSING",
                message: format!("引擎 {} 的运行时投影已不存在", self.runtime_id),
            });
        }
        self.registry
            .reload_one(&self.runtime_id)
            .await
            .map_err(|_| registry_error(&self.runtime_id))?;
        Ok(())
    }

    pub(super) async fn rollback(&mut self) -> Result<(), AssetError> {
        if !self.applied {
            return Ok(());
        }
        self.restore_previous().await?;
        self.applied = false;
        Ok(())
    }

    pub(super) fn finalize(&mut self) {
        self.applied = false;
    }

    async fn restore_previous(&self) -> Result<(), AssetError> {
        match self.previous.as_ref() {
            Some(previous) => self.repo.restore_projection_row(previous).await?,
            None => {
                self.repo.delete(&self.runtime_id).await?;
            }
        }
        self.registry
            .reload_one(&self.runtime_id)
            .await
            .map_err(|_| registry_error(&self.runtime_id))?;
        Ok(())
    }

    async fn reload_required(&self) -> Result<(), AssetError> {
        self.registry
            .reload_one(&self.runtime_id)
            .await
            .map_err(|_| registry_error(&self.runtime_id))?
            .ok_or_else(|| registry_error(&self.runtime_id))?;
        Ok(())
    }
}

fn validate_and_resolve(
    asset: &RuntimeAssetDefinition,
) -> Result<(EngineAdapterDefinition, EngineLaunchSpec), AssetError> {
    validate_runtime_name(&asset.portable_runtime_id, "引擎 runtimeId")?;
    let bytes = asset
        .files
        .iter()
        .find(|file| file.path == asset.entry_file)
        .map(|file| file.content.as_slice())
        .ok_or_else(|| AssetError::InvalidMetadata("引擎入口文件不存在".into()))?;
    let definition = parse_engine_adapter_definition(bytes)?;
    if definition.runtime_id != asset.portable_runtime_id {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ENGINE_ID_MISMATCH",
            message: "engine-adapter.json 的 runtimeId 与资产运行时身份不一致".into(),
        });
    }
    let (configuration, secrets) = match asset.runtime_configuration.as_ref() {
        Some(resolved) => match &resolved.configuration {
            AssetPublicConfiguration::EngineAdapter(configuration) => (configuration.clone(), resolved.secrets.clone()),
            _ => return Err(configuration_kind_error(AssetKind::EngineAdapter)),
        },
        None => (EngineAdapterAssetConfiguration::default(), BTreeMap::new()),
    };
    validate_configuration_schema(&definition, &configuration, &secrets)?;
    let environment = resolve_environment(&configuration, &secrets)?;
    let (command, mut arguments) = resolve_command(&definition, &configuration)?;
    arguments.extend(definition.protocol.arguments.iter().cloned());
    arguments.extend(configuration.arguments.iter().cloned());
    Ok((
        definition,
        EngineLaunchSpec {
            command,
            arguments,
            environment,
            working_directory: configuration.working_directory.map(PathBuf::from),
        },
    ))
}

fn resolve_command(
    definition: &EngineAdapterDefinition,
    configuration: &EngineAdapterAssetConfiguration,
) -> Result<(String, Vec<String>), AssetError> {
    if let Some(path) = configuration.executable_path.as_deref() {
        validate_command(path)?;
        return Ok((path.to_owned(), Vec::new()));
    }
    if let Some(command) = configuration.command.as_deref() {
        validate_command(command)?;
        return Ok((command.to_owned(), Vec::new()));
    }
    validate_command(&definition.runtime.command_name)?;
    Ok((definition.runtime.command_name.clone(), Vec::new()))
}

fn resolve_environment(
    configuration: &EngineAdapterAssetConfiguration,
    secrets: &BTreeMap<String, String>,
) -> Result<HashMap<String, String>, AssetError> {
    let mut names = BTreeSet::new();
    let mut environment = HashMap::new();
    for binding in &configuration.environment {
        validate_environment_name(&binding.name)?;
        if !names.insert(binding.name.clone()) {
            return Err(configuration_error("引擎环境变量名称不能重复"));
        }
        let value = secrets
            .get(&binding.secret_slot)
            .ok_or_else(|| AssetError::RuntimeProjectionFailed {
                code: "RUNTIME_ENGINE_SECRET_MISSING",
                message: format!("引擎环境变量 {} 引用的凭据槽尚未配置", binding.name),
            })?;
        environment.insert(binding.name.clone(), value.clone());
    }
    Ok(environment)
}

fn validate_configuration_schema(
    definition: &EngineAdapterDefinition,
    configuration: &EngineAdapterAssetConfiguration,
    resolved_secrets: &BTreeMap<String, String>,
) -> Result<(), AssetError> {
    if !configuration.secrets.is_empty() {
        return Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_ENGINE_KEYED_SECRET_ADAPTER_REQUIRED",
            message: "通用 ACP adapter 不推断 Definition 密钥字段的注入协议".into(),
        });
    }
    if let Some(directory) = configuration.working_directory.as_deref()
        && (!Path::new(directory).is_absolute() || !Path::new(directory).is_dir())
    {
        return Err(configuration_error("引擎 workingDirectory 必须是已存在的绝对目录"));
    }
    let values = configuration
        .values
        .iter()
        .map(|entry| (entry.key.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    if values.len() != configuration.values.len() {
        return Err(configuration_error("引擎配置值 key 不能重复"));
    }
    if let Some(schema) = definition.configuration_schema.as_ref() {
        let known_fields = schema
            .fields
            .iter()
            .map(|field| field.key.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(unknown) = values.keys().find(|key| !known_fields.contains(*key)) {
            return Err(configuration_error(&format!(
                "引擎配置包含 Definition 未声明的字段 {unknown}"
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
                        code: "RUNTIME_ENGINE_SECRET_MISSING",
                        message: format!("引擎必填凭据字段 {} 尚未配置", field.key),
                    });
                }
                continue;
            }
            match values.get(field.key.as_str()) {
                Some(value) => validate_value_type(value, field.value_type)?,
                None if field.required => {
                    return Err(configuration_error(&format!("引擎必填配置字段 {} 缺失", field.key)));
                }
                None => {}
            }
        }
    } else if !values.is_empty() {
        return Err(configuration_error("引擎 Definition 没有声明可配置字段"));
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

fn owner_marker(local_asset_id: &str) -> String {
    format!("asset:{}", stable_identity(local_asset_id))
}

fn is_owned_projection(row: &AgentMetadataRow, owner: &str) -> bool {
    row.agent_source == "asset"
        && row
            .agent_source_info
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| {
                value
                    .get("hub_package_id")
                    .and_then(|marker| marker.as_str())
                    .map(str::to_owned)
            })
            .as_deref()
            == Some(owner)
}

fn validate_command(value: &str) -> Result<(), AssetError> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(configuration_error("引擎启动命令不能为空或包含 NUL"));
    }
    Ok(())
}

fn validate_environment_name(value: &str) -> Result<(), AssetError> {
    let mut chars = value.chars();
    if !chars.next().is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(configuration_error("引擎环境变量名称不合法"));
    }
    if is_blocked_runtime_environment_name(value) {
        return Err(configuration_error("引擎环境变量名称属于运行时保留字段"));
    }
    Ok(())
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

fn configuration_kind_error(kind: AssetKind) -> AssetError {
    AssetError::RuntimeProjectionUnsupported {
        code: "RUNTIME_CONFIGURATION_KIND_MISMATCH",
        message: format!("本机配置类型与 {kind:?} 资产不匹配"),
    }
}

fn configuration_error(message: &str) -> AssetError {
    AssetError::RuntimeProjectionUnsupported {
        code: "RUNTIME_ENGINE_CONFIGURATION_INVALID",
        message: message.into(),
    }
}

fn registry_error(runtime_id: &str) -> AssetError {
    AssetError::RuntimeProjectionFailed {
        code: "RUNTIME_ENGINE_REGISTRY_RELOAD_FAILED",
        message: format!("引擎 {runtime_id} 的 AgentRegistry 重载失败"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjuaeui_asset::{AssetDefinitionFile, RuntimeResolvedConfiguration};

    fn fixture_asset(configuration: EngineAdapterAssetConfiguration) -> RuntimeAssetDefinition {
        RuntimeAssetDefinition {
            local_asset_id: "engine:contract-acp".into(),
            kind: AssetKind::EngineAdapter,
            portable_runtime_id: "contract-acp".into(),
            projection_runtime_id: format!("tjuae-proj-v1-{}", "1".repeat(64)),
            entry_file: "engine-adapter.json".into(),
            workspace_path: "engine-adapters/contract-acp".into(),
            files: vec![AssetDefinitionFile::text(
                "engine-adapter.json",
                include_str!("../../../tjuaeui-asset/tests/fixtures/engine-adapter-definition.v1.complete.json"),
            )],
            dependency_portable_runtime_ids: BTreeMap::new(),
            dependency_projection_runtime_ids: BTreeMap::new(),
            runtime_configuration: Some(RuntimeResolvedConfiguration {
                configuration: AssetPublicConfiguration::EngineAdapter(configuration),
                configuration_schema: parse_engine_adapter_definition(include_bytes!(
                    "../../../tjuaeui-asset/tests/fixtures/engine-adapter-definition.v1.complete.json"
                ))
                .unwrap()
                .configuration_schema
                .unwrap_or_default(),
                secrets: BTreeMap::new(),
            }),
        }
    }

    #[test]
    fn external_command_keeps_protocol_arguments_without_installing_a_package() {
        let mut configuration = EngineAdapterAssetConfiguration::default();
        configuration.values.push(AssetConfigurationValue {
            key: "profile".into(),
            value: AssetPrimitiveValue::String("test".into()),
        });
        let (_, spec) = validate_and_resolve(&fixture_asset(configuration)).unwrap();
        assert_eq!(spec.command, "contract-acp");
        assert_eq!(spec.arguments, ["--acp"]);
    }

    #[test]
    fn persisted_environment_is_never_built_from_plaintext_secret() {
        let mut configuration = EngineAdapterAssetConfiguration::default();
        configuration.values.push(AssetConfigurationValue {
            key: "profile".into(),
            value: AssetPrimitiveValue::String("test".into()),
        });
        configuration.environment.push(tjuaeui_api_types::AssetNamedSecretSlot {
            name: "API_TOKEN".into(),
            secret_slot: "token".into(),
        });
        let mut asset = fixture_asset(configuration);
        asset
            .runtime_configuration
            .as_mut()
            .unwrap()
            .secrets
            .insert("token".into(), "plain-secret".into());
        let (_, spec) = validate_and_resolve(&asset).unwrap();
        assert_eq!(
            spec.environment.get("API_TOKEN").map(String::as_str),
            Some("plain-secret")
        );
    }
}
