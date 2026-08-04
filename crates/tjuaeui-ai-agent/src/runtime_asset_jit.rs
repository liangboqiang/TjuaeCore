//! Per-session resolution of managed runtime-asset configuration.
//!
//! This module is deliberately the only bridge from a persisted, non-secret
//! Engine/MCP projection to the owning user's encrypted Overlay. Returned values
//! stay in the launch call stack: none of these types implement `Debug` or
//! `Serialize`, and callers must never write them back to legacy tables.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use tjuaeui_api_types::{
    AssetConfigurationBindingTarget, AssetConfigurationFieldDefinition, AssetConfigurationSchemaDefinition,
    AssetConfigurationValue, AssetConfigurationValueType, AssetKeyedSecretSlot, AssetNamedSecretSlot,
    AssetPrimitiveValue, AssetPublicConfiguration, EngineAdapterAssetConfiguration, McpAssetConfiguration,
    McpAssetTransport,
};
use tjuaeui_asset::RuntimeAssetConfigurationResolver;
use tjuaeui_db::models::McpServerRow;

use crate::error::AgentError;

pub(crate) struct EngineJitConfiguration {
    pub environment: Vec<tjuaeui_common::EnvVar>,
    pub working_directory: Option<String>,
}

pub(crate) struct McpJitConfiguration {
    pub transport: McpAssetTransport,
    pub environment: HashMap<String, String>,
    pub headers: HashMap<String, String>,
}

/// Return the original Core-local asset ID only for a row carrying our managed
/// marker. Ordinary legacy `original_json` remains unmanaged. Once the managed
/// marker is present, every required field is strict and failures are fatal.
pub(crate) fn managed_mcp_local_asset_id(row: &McpServerRow) -> Result<Option<String>, AgentError> {
    let Some(raw) = row.original_json.as_deref() else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    let Some(marker) = value.get("$tjuaeAsset") else {
        return Ok(None);
    };
    let marker = marker
        .as_object()
        .ok_or_else(|| jit_error("MANAGED_MCP_MARKER_INVALID"))?;
    if marker.get("kind").and_then(serde_json::Value::as_str) != Some("mcp") {
        return Err(jit_error("MANAGED_MCP_MARKER_INVALID"));
    }
    let local_asset_id = marker
        .get("tjuaeLocalAssetId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| jit_error("MANAGED_MCP_LOCAL_ASSET_ID_MISSING"))?;
    Ok(Some(local_asset_id.to_owned()))
}

pub(crate) async fn resolve_engine_configuration(
    resolver: Option<&dyn RuntimeAssetConfigurationResolver>,
    user_id: &str,
    local_asset_id: &str,
) -> Result<EngineJitConfiguration, AgentError> {
    validate_resolution_identity(user_id, local_asset_id, "ENGINE")?;
    let resolved = resolve_required(resolver, user_id, local_asset_id, "ENGINE").await?;
    let configuration = match resolved.configuration {
        AssetPublicConfiguration::EngineAdapter(configuration) => configuration,
        _ => return Err(jit_error("ENGINE_CONFIGURATION_KIND_MISMATCH")),
    };
    validate_engine_configuration(&configuration)?;
    let mut environment = resolve_environment(&configuration.environment, &resolved.secrets, "ENGINE")?;
    let declared = resolve_configuration_bindings(
        &resolved.configuration_schema,
        &configuration.values,
        &configuration.secrets,
        &resolved.secrets,
        AssetConfigurationBindingTarget::Environment,
        "ENGINE",
    )?;
    merge_bound_values(&mut environment, declared, true, "ENGINE_ENVIRONMENT")?;
    let environment = environment
        .into_iter()
        .map(|(name, value)| tjuaeui_common::EnvVar { name, value })
        .collect();
    Ok(EngineJitConfiguration {
        environment,
        working_directory: configuration.working_directory,
    })
}

pub(crate) async fn resolve_mcp_configuration(
    resolver: Option<&dyn RuntimeAssetConfigurationResolver>,
    user_id: &str,
    local_asset_id: &str,
) -> Result<McpJitConfiguration, AgentError> {
    validate_resolution_identity(user_id, local_asset_id, "MCP")?;
    let resolved = resolve_required(resolver, user_id, local_asset_id, "MCP").await?;
    let configuration = match resolved.configuration {
        AssetPublicConfiguration::Mcp(configuration) => configuration,
        _ => return Err(jit_error("MCP_CONFIGURATION_KIND_MISMATCH")),
    };
    validate_mcp_configuration(&configuration)?;
    let (environment, headers) = match configuration.transport {
        McpAssetTransport::Stdio => {
            let mut environment = resolve_environment(&configuration.environment, &resolved.secrets, "MCP")?;
            let declared = resolve_configuration_bindings(
                &resolved.configuration_schema,
                &configuration.values,
                &configuration.secrets,
                &resolved.secrets,
                AssetConfigurationBindingTarget::Environment,
                "MCP",
            )?;
            merge_bound_values(&mut environment, declared, true, "MCP_ENVIRONMENT")?;
            (environment.into_iter().collect(), HashMap::new())
        }
        McpAssetTransport::Sse | McpAssetTransport::StreamableHttp => {
            let mut headers = resolve_headers(&configuration.headers, &resolved.secrets)?;
            let declared = resolve_configuration_bindings(
                &resolved.configuration_schema,
                &configuration.values,
                &configuration.secrets,
                &resolved.secrets,
                AssetConfigurationBindingTarget::Header,
                "MCP",
            )?;
            merge_bound_values(&mut headers, declared.into_iter().collect(), false, "MCP_HEADER")?;
            (HashMap::new(), headers)
        }
    };
    Ok(McpJitConfiguration {
        transport: configuration.transport,
        environment,
        headers,
    })
}

/// Verify that the persisted projection is the public, credential-free half of
/// the same transport selected by the resolved Overlay. This runs before any
/// adapter converts the row into its own SDK type.
pub(crate) fn verify_managed_mcp_projection(
    row: &McpServerRow,
    configuration: &McpJitConfiguration,
) -> Result<(), AgentError> {
    let transport_matches = matches!(
        (row.transport_type.as_str(), configuration.transport),
        ("stdio", McpAssetTransport::Stdio)
            | ("sse", McpAssetTransport::Sse)
            | ("http" | "streamable_http", McpAssetTransport::StreamableHttp)
    );
    if !transport_matches {
        return Err(jit_error("MCP_TRANSPORT_MISMATCH"));
    }
    let value = serde_json::from_str::<serde_json::Value>(&row.transport_config)
        .map_err(|_| jit_error("MCP_PROJECTION_INVALID"))?;
    let credential_field = match configuration.transport {
        McpAssetTransport::Stdio => "env",
        McpAssetTransport::Sse | McpAssetTransport::StreamableHttp => "headers",
    };
    if value
        .get(credential_field)
        .and_then(serde_json::Value::as_object)
        .is_some_and(|values| !values.is_empty())
    {
        return Err(jit_error("MCP_PERSISTED_CREDENTIALS_REJECTED"));
    }
    Ok(())
}

async fn resolve_required(
    resolver: Option<&dyn RuntimeAssetConfigurationResolver>,
    user_id: &str,
    local_asset_id: &str,
    kind: &str,
) -> Result<tjuaeui_asset::RuntimeResolvedConfiguration, AgentError> {
    let resolver = resolver.ok_or_else(|| jit_error(&format!("{kind}_CONFIGURATION_RESOLVER_MISSING")))?;
    // Deliberately discard the source error: catalog/storage/crypto diagnostics
    // can contain implementation details and must not become a session DTO/log.
    resolver
        .resolve(user_id, local_asset_id)
        .await
        .map_err(|_| jit_error(&format!("{kind}_CONFIGURATION_RESOLUTION_FAILED")))?
        .ok_or_else(|| jit_error(&format!("{kind}_CONFIGURATION_MISSING")))
}

fn validate_resolution_identity(user_id: &str, local_asset_id: &str, kind: &str) -> Result<(), AgentError> {
    if user_id.trim().is_empty() {
        return Err(jit_error(&format!("{kind}_SESSION_USER_MISSING")));
    }
    if local_asset_id.trim().is_empty() {
        return Err(jit_error(&format!("{kind}_LOCAL_ASSET_ID_MISSING")));
    }
    Ok(())
}

fn validate_engine_configuration(configuration: &EngineAdapterAssetConfiguration) -> Result<(), AgentError> {
    if let Some(directory) = configuration.working_directory.as_deref()
        && (!Path::new(directory).is_absolute() || !Path::new(directory).is_dir())
    {
        return Err(jit_error("ENGINE_WORKING_DIRECTORY_INVALID"));
    }
    Ok(())
}

fn validate_mcp_configuration(configuration: &McpAssetConfiguration) -> Result<(), AgentError> {
    match configuration.transport {
        McpAssetTransport::Stdio if !configuration.headers.is_empty() || configuration.instance_url.is_some() => {
            Err(jit_error("MCP_STDIO_CONFIGURATION_INVALID"))
        }
        McpAssetTransport::Sse | McpAssetTransport::StreamableHttp
            if !configuration.environment.is_empty()
                || configuration.executable_path.is_some()
                || !configuration.arguments.is_empty() =>
        {
            Err(jit_error("MCP_REMOTE_CONFIGURATION_INVALID"))
        }
        _ => Ok(()),
    }
}

fn resolve_configuration_bindings(
    schema: &AssetConfigurationSchemaDefinition,
    values: &[AssetConfigurationValue],
    keyed_secrets: &[AssetKeyedSecretSlot],
    secrets: &BTreeMap<String, String>,
    allowed_target: AssetConfigurationBindingTarget,
    kind: &str,
) -> Result<BTreeMap<String, String>, AgentError> {
    let fields = unique_by_key(&schema.fields, |field| field.key.as_str())
        .ok_or_else(|| jit_error(&format!("{kind}_SCHEMA_FIELD_DUPLICATE")))?;
    let values = unique_by_key(values, |value| value.key.as_str())
        .ok_or_else(|| jit_error(&format!("{kind}_CONFIGURATION_KEY_DUPLICATE")))?;
    let keyed_secrets = unique_by_key(keyed_secrets, |value| value.key.as_str())
        .ok_or_else(|| jit_error(&format!("{kind}_SECRET_KEY_DUPLICATE")))?;

    for key in values.keys().chain(keyed_secrets.keys()) {
        if !fields.contains_key(key) {
            return Err(jit_error(&format!("{kind}_CONFIGURATION_FIELD_UNKNOWN")));
        }
    }

    let mut result = BTreeMap::new();
    let mut target_names = BTreeSet::new();
    for field in &schema.fields {
        if field.binding.target != allowed_target {
            return Err(jit_error(&format!("{kind}_CONFIGURATION_BINDING_TARGET_INVALID")));
        }
        validate_bound_name(&field.binding.name, allowed_target, kind)?;
        let folded_name = field.binding.name.to_ascii_lowercase();
        if !target_names.insert(folded_name) {
            return Err(jit_error(&format!("{kind}_CONFIGURATION_BINDING_DUPLICATE")));
        }

        let rendered = if field.secret {
            if values.contains_key(field.key.as_str()) {
                return Err(jit_error(&format!("{kind}_SECRET_EXPOSED_AS_VALUE")));
            }
            keyed_secrets
                .get(field.key.as_str())
                .map(|slot| required_secret(secrets, &slot.secret_slot, kind))
                .transpose()?
                .map(|value| render_secret_value(&value, field, kind))
                .transpose()?
        } else {
            if keyed_secrets.contains_key(field.key.as_str()) {
                return Err(jit_error(&format!("{kind}_PUBLIC_VALUE_USES_SECRET_SLOT")));
            }
            values
                .get(field.key.as_str())
                .map(|value| render_public_value(&value.value, field, kind))
                .transpose()?
        };
        let Some(rendered) = rendered else {
            if field.required {
                return Err(jit_error(&format!("{kind}_CONFIGURATION_REQUIRED")));
            }
            continue;
        };
        validate_bound_value(&rendered, allowed_target, kind)?;
        result.insert(field.binding.name.clone(), rendered);
    }
    Ok(result)
}

fn unique_by_key<'a, T>(values: &'a [T], key: impl Fn(&'a T) -> &'a str) -> Option<BTreeMap<&'a str, &'a T>> {
    let mut result = BTreeMap::new();
    for value in values {
        if result.insert(key(value), value).is_some() {
            return None;
        }
    }
    Some(result)
}

fn render_public_value(
    value: &AssetPrimitiveValue,
    field: &AssetConfigurationFieldDefinition,
    kind: &str,
) -> Result<String, AgentError> {
    match (value, field.value_type) {
        (AssetPrimitiveValue::String(value), AssetConfigurationValueType::String) => Ok(value.clone()),
        (AssetPrimitiveValue::Number(value), AssetConfigurationValueType::Number) => Ok(value.to_string()),
        (AssetPrimitiveValue::Boolean(value), AssetConfigurationValueType::Boolean) => Ok(value.to_string()),
        _ => Err(jit_error(&format!("{kind}_CONFIGURATION_VALUE_TYPE_INVALID"))),
    }
}

fn render_secret_value(
    value: &str,
    field: &AssetConfigurationFieldDefinition,
    kind: &str,
) -> Result<String, AgentError> {
    match field.value_type {
        AssetConfigurationValueType::String => Ok(value.to_owned()),
        AssetConfigurationValueType::Number => serde_json::from_str::<serde_json::Number>(value)
            .map(|number| number.to_string())
            .map_err(|_| jit_error(&format!("{kind}_SECRET_VALUE_TYPE_INVALID"))),
        AssetConfigurationValueType::Boolean => value
            .parse::<bool>()
            .map(|value| value.to_string())
            .map_err(|_| jit_error(&format!("{kind}_SECRET_VALUE_TYPE_INVALID"))),
    }
}

fn validate_bound_name(name: &str, target: AssetConfigurationBindingTarget, kind: &str) -> Result<(), AgentError> {
    let valid = match target {
        AssetConfigurationBindingTarget::Environment => {
            valid_environment_name(name) && !crate::registry::is_blocked_override_env_key(name)
        }
        AssetConfigurationBindingTarget::Header => valid_header_name(name),
    };
    if valid {
        Ok(())
    } else {
        Err(jit_error(&format!("{kind}_CONFIGURATION_BINDING_NAME_INVALID")))
    }
}

fn validate_bound_value(value: &str, target: AssetConfigurationBindingTarget, kind: &str) -> Result<(), AgentError> {
    if value.contains(['\r', '\n', '\0']) {
        let target = match target {
            AssetConfigurationBindingTarget::Environment => "ENVIRONMENT",
            AssetConfigurationBindingTarget::Header => "HEADER",
        };
        return Err(jit_error(&format!("{kind}_{target}_VALUE_INVALID")));
    }
    Ok(())
}

fn merge_bound_values<M>(existing: &mut M, additions: M, environment: bool, code_prefix: &str) -> Result<(), AgentError>
where
    M: RuntimeBindingMap,
{
    for (name, value) in additions.into_entries() {
        if existing.contains_case_insensitive(&name) {
            return Err(jit_error(&format!("{code_prefix}_DUPLICATE")));
        }
        if environment && crate::registry::is_blocked_override_env_key(&name) {
            return Err(jit_error(&format!("{code_prefix}_NAME_INVALID")));
        }
        existing.insert_entry(name, value);
    }
    Ok(())
}

trait RuntimeBindingMap: Sized {
    fn into_entries(self) -> Vec<(String, String)>;
    fn contains_case_insensitive(&self, name: &str) -> bool;
    fn insert_entry(&mut self, name: String, value: String);
}

impl RuntimeBindingMap for BTreeMap<String, String> {
    fn into_entries(self) -> Vec<(String, String)> {
        self.into_iter().collect()
    }

    fn contains_case_insensitive(&self, name: &str) -> bool {
        self.keys().any(|existing| existing.eq_ignore_ascii_case(name))
    }

    fn insert_entry(&mut self, name: String, value: String) {
        self.insert(name, value);
    }
}

impl RuntimeBindingMap for HashMap<String, String> {
    fn into_entries(self) -> Vec<(String, String)> {
        self.into_iter().collect()
    }

    fn contains_case_insensitive(&self, name: &str) -> bool {
        self.keys().any(|existing| existing.eq_ignore_ascii_case(name))
    }

    fn insert_entry(&mut self, name: String, value: String) {
        self.insert(name, value);
    }
}

fn resolve_environment(
    bindings: &[AssetNamedSecretSlot],
    secrets: &BTreeMap<String, String>,
    kind: &str,
) -> Result<BTreeMap<String, String>, AgentError> {
    let mut names = BTreeSet::new();
    let mut result = BTreeMap::new();
    for binding in bindings {
        if !valid_environment_name(&binding.name) || crate::registry::is_blocked_override_env_key(&binding.name) {
            return Err(jit_error(&format!("{kind}_ENVIRONMENT_NAME_INVALID")));
        }
        if !names.insert(binding.name.to_ascii_uppercase()) {
            return Err(jit_error(&format!("{kind}_ENVIRONMENT_NAME_DUPLICATE")));
        }
        let value = required_secret(secrets, &binding.secret_slot, kind)?;
        if value.contains(['\r', '\n', '\0']) {
            return Err(jit_error(&format!("{kind}_ENVIRONMENT_VALUE_INVALID")));
        }
        result.insert(binding.name.clone(), value);
    }
    Ok(result)
}

fn resolve_headers(
    bindings: &[AssetNamedSecretSlot],
    secrets: &BTreeMap<String, String>,
) -> Result<HashMap<String, String>, AgentError> {
    let mut names = BTreeSet::new();
    let mut result = HashMap::new();
    for binding in bindings {
        if !valid_header_name(&binding.name) {
            return Err(jit_error("MCP_HEADER_NAME_INVALID"));
        }
        if !names.insert(binding.name.to_ascii_lowercase()) {
            return Err(jit_error("MCP_HEADER_NAME_DUPLICATE"));
        }
        let value = required_secret(secrets, &binding.secret_slot, "MCP")?;
        if value.contains(['\r', '\n', '\0']) {
            return Err(jit_error("MCP_HEADER_VALUE_INVALID"));
        }
        result.insert(binding.name.clone(), value);
    }
    Ok(result)
}

fn required_secret(secrets: &BTreeMap<String, String>, slot: &str, kind: &str) -> Result<String, AgentError> {
    if slot.trim().is_empty() {
        return Err(jit_error(&format!("{kind}_SECRET_SLOT_INVALID")));
    }
    secrets
        .get(slot)
        .cloned()
        .ok_or_else(|| jit_error(&format!("{kind}_SECRET_MISSING")))
}

fn valid_environment_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn valid_header_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn jit_error(code: &str) -> AgentError {
    AgentError::bad_request(format!("RUNTIME_ASSET_JIT_{code}：托管运行资产的会话配置无法安全解析"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use tjuaeui_api_types::{AssetKind, McpAssetTransport};
    use tjuaeui_asset::{AssetError, RuntimeResolvedConfiguration};
    use tjuaeui_common::now_ms;

    use super::*;

    struct Resolver {
        expected_user_id: String,
        configuration: AssetPublicConfiguration,
        secrets: BTreeMap<String, String>,
    }

    #[async_trait]
    impl RuntimeAssetConfigurationResolver for Resolver {
        async fn resolve(
            &self,
            user_id: &str,
            _local_asset_id: &str,
        ) -> Result<Option<RuntimeResolvedConfiguration>, AssetError> {
            if user_id != self.expected_user_id {
                return Ok(None);
            }
            Ok(Some(RuntimeResolvedConfiguration {
                configuration: self.configuration.clone(),
                configuration_schema: AssetConfigurationSchemaDefinition::default(),
                secrets: self.secrets.clone(),
            }))
        }
    }

    fn mcp_row(original_json: Option<String>) -> McpServerRow {
        McpServerRow {
            id: "mcp-row".into(),
            name: "managed".into(),
            description: None,
            enabled: true,
            transport_type: "http".into(),
            transport_config: r#"{"url":"https://example.invalid/mcp","headers":{}}"#.into(),
            tools: None,
            last_test_status: "disconnected".into(),
            last_connected: None,
            original_json,
            builtin: false,
            deleted_at: None,
            created_at: now_ms(),
            updated_at: now_ms(),
        }
    }

    #[tokio::test]
    async fn engine_credentials_resolve_only_for_the_session_user() {
        let mut configuration = EngineAdapterAssetConfiguration::default();
        configuration.environment.push(AssetNamedSecretSlot {
            name: "API_TOKEN".into(),
            secret_slot: "token".into(),
        });
        let resolver: Arc<dyn RuntimeAssetConfigurationResolver> = Arc::new(Resolver {
            expected_user_id: "user-1".into(),
            configuration: AssetPublicConfiguration::EngineAdapter(configuration),
            secrets: BTreeMap::from([("token".into(), "session-secret".into())]),
        });

        let resolved = resolve_engine_configuration(Some(resolver.as_ref()), "user-1", "engine:managed")
            .await
            .expect("resolve for owner");
        assert_eq!(resolved.environment.len(), 1);
        assert_eq!(resolved.environment[0].name, "API_TOKEN");
        assert_eq!(resolved.environment[0].value, "session-secret");
        let error = match resolve_engine_configuration(Some(resolver.as_ref()), "other-user", "engine:managed").await {
            Ok(_) => panic!("cross-user resolution must fail closed"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("RUNTIME_ASSET_JIT_ENGINE_CONFIGURATION_MISSING")
        );
    }

    #[tokio::test]
    async fn mcp_header_is_resolved_in_memory_and_missing_secret_fails_closed() {
        let configuration = McpAssetConfiguration {
            transport: McpAssetTransport::StreamableHttp,
            executable_path: None,
            arguments: Vec::new(),
            instance_url: Some("https://example.invalid/mcp".into()),
            environment: Vec::new(),
            headers: vec![AssetNamedSecretSlot {
                name: "Authorization".into(),
                secret_slot: "authorization".into(),
            }],
            values: Vec::new(),
            secrets: Vec::new(),
        };
        let resolver: Arc<dyn RuntimeAssetConfigurationResolver> = Arc::new(Resolver {
            expected_user_id: "user-1".into(),
            configuration: AssetPublicConfiguration::Mcp(configuration.clone()),
            secrets: BTreeMap::from([("authorization".into(), "Bearer session-secret".into())]),
        });
        let resolved = resolve_mcp_configuration(Some(resolver.as_ref()), "user-1", "mcp:managed")
            .await
            .expect("resolve header");
        assert_eq!(
            resolved.headers.get("Authorization").map(String::as_str),
            Some("Bearer session-secret")
        );

        let missing: Arc<dyn RuntimeAssetConfigurationResolver> = Arc::new(Resolver {
            expected_user_id: "user-1".into(),
            configuration: AssetPublicConfiguration::Mcp(configuration),
            secrets: BTreeMap::new(),
        });
        let error = match resolve_mcp_configuration(Some(missing.as_ref()), "user-1", "mcp:managed").await {
            Ok(_) => panic!("missing secret must fail closed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("RUNTIME_ASSET_JIT_MCP_SECRET_MISSING"));
    }

    #[test]
    fn managed_marker_is_strict_but_ordinary_original_json_is_legacy() {
        let ordinary = mcp_row(Some(r#"{"command":"legacy"}"#.into()));
        assert_eq!(managed_mcp_local_asset_id(&ordinary).unwrap(), None);

        let managed = mcp_row(Some(
            serde_json::json!({
                "$tjuaeAsset": {
                    "id": "digest",
                    "kind": "mcp",
                    "tjuaeLocalAssetId": "mcp:managed"
                }
            })
            .to_string(),
        ));
        assert_eq!(
            managed_mcp_local_asset_id(&managed).unwrap().as_deref(),
            Some("mcp:managed")
        );

        let stale = mcp_row(Some(
            serde_json::json!({"$tjuaeAsset": {"id": "digest", "kind": "mcp"}}).to_string(),
        ));
        assert!(managed_mcp_local_asset_id(&stale).is_err());
    }

    #[test]
    fn dangerous_environment_names_are_rejected() {
        let mut configuration = EngineAdapterAssetConfiguration::default();
        configuration.environment.push(AssetNamedSecretSlot {
            name: "TJUAE_RUNTIME_TOKEN".into(),
            secret_slot: "token".into(),
        });
        assert!(validate_engine_configuration(&configuration).is_ok());
        let error = resolve_environment(
            &configuration.environment,
            &BTreeMap::from([("token".into(), "secret".into())]),
            "ENGINE",
        )
        .expect_err("reserved key must fail");
        assert!(error.to_string().contains("ENVIRONMENT_NAME_INVALID"));
    }

    #[tokio::test]
    async fn definition_public_and_secret_fields_are_bound_only_at_session_start() {
        let configuration = EngineAdapterAssetConfiguration {
            values: vec![AssetConfigurationValue {
                key: "baseUrl".into(),
                value: AssetPrimitiveValue::String("https://api.example.invalid".into()),
            }],
            secrets: vec![AssetKeyedSecretSlot {
                key: "apiKey".into(),
                secret_slot: "engine-api-key".into(),
            }],
            ..Default::default()
        };
        let schema = AssetConfigurationSchemaDefinition {
            fields: vec![
                AssetConfigurationFieldDefinition {
                    key: "baseUrl".into(),
                    label: "接口地址".into(),
                    description: None,
                    value_type: AssetConfigurationValueType::String,
                    required: true,
                    secret: false,
                    binding: tjuaeui_api_types::AssetConfigurationFieldBindingDefinition {
                        target: AssetConfigurationBindingTarget::Environment,
                        name: "OPENAI_BASE_URL".into(),
                    },
                },
                AssetConfigurationFieldDefinition {
                    key: "apiKey".into(),
                    label: "API 密钥".into(),
                    description: None,
                    value_type: AssetConfigurationValueType::String,
                    required: true,
                    secret: true,
                    binding: tjuaeui_api_types::AssetConfigurationFieldBindingDefinition {
                        target: AssetConfigurationBindingTarget::Environment,
                        name: "OPENAI_API_KEY".into(),
                    },
                },
            ],
        };
        struct BoundResolver {
            configuration: AssetPublicConfiguration,
            schema: AssetConfigurationSchemaDefinition,
        }
        #[async_trait]
        impl RuntimeAssetConfigurationResolver for BoundResolver {
            async fn resolve(
                &self,
                _user_id: &str,
                _local_asset_id: &str,
            ) -> Result<Option<RuntimeResolvedConfiguration>, AssetError> {
                Ok(Some(RuntimeResolvedConfiguration {
                    configuration: self.configuration.clone(),
                    configuration_schema: self.schema.clone(),
                    secrets: BTreeMap::from([("engine-api-key".into(), "session-secret".into())]),
                }))
            }
        }
        let resolver = BoundResolver {
            configuration: AssetPublicConfiguration::EngineAdapter(configuration),
            schema,
        };
        let resolved = resolve_engine_configuration(Some(&resolver), "user-1", "engine:bound")
            .await
            .expect("schema bindings resolve");
        let environment = resolved
            .environment
            .into_iter()
            .map(|value| (value.name, value.value))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            environment.get("OPENAI_BASE_URL").map(String::as_str),
            Some("https://api.example.invalid")
        );
        assert_eq!(
            environment.get("OPENAI_API_KEY").map(String::as_str),
            Some("session-secret")
        );
    }

    #[test]
    fn newlines_and_duplicate_binding_targets_fail_closed() {
        let schema = AssetConfigurationSchemaDefinition {
            fields: vec![
                AssetConfigurationFieldDefinition {
                    key: "first".into(),
                    label: "第一项".into(),
                    description: None,
                    value_type: AssetConfigurationValueType::String,
                    required: true,
                    secret: false,
                    binding: tjuaeui_api_types::AssetConfigurationFieldBindingDefinition {
                        target: AssetConfigurationBindingTarget::Environment,
                        name: "DEMO_VALUE".into(),
                    },
                },
                AssetConfigurationFieldDefinition {
                    key: "second".into(),
                    label: "第二项".into(),
                    description: None,
                    value_type: AssetConfigurationValueType::String,
                    required: true,
                    secret: false,
                    binding: tjuaeui_api_types::AssetConfigurationFieldBindingDefinition {
                        target: AssetConfigurationBindingTarget::Environment,
                        name: "demo_value".into(),
                    },
                },
            ],
        };
        let values = vec![
            AssetConfigurationValue {
                key: "first".into(),
                value: AssetPrimitiveValue::String("safe".into()),
            },
            AssetConfigurationValue {
                key: "second".into(),
                value: AssetPrimitiveValue::String("unsafe\nvalue".into()),
            },
        ];
        let error = resolve_configuration_bindings(
            &schema,
            &values,
            &[],
            &BTreeMap::new(),
            AssetConfigurationBindingTarget::Environment,
            "ENGINE",
        )
        .expect_err("case-insensitive duplicate targets must fail");
        assert!(error.to_string().contains("CONFIGURATION_BINDING_DUPLICATE"));
    }

    #[test]
    fn public_kind_names_remain_distinct() {
        assert_eq!(
            AssetPublicConfiguration::EngineAdapter(Default::default()).kind(),
            AssetKind::EngineAdapter
        );
    }
}
