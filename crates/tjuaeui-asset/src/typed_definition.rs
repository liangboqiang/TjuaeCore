use std::collections::HashSet;

use semver::Version;
use tjuaeui_api_types::{
    AssetConfigurationBindingTarget, AssetConfigurationSchemaDefinition, AssetKind, EngineAdapterDefinition,
    McpDefinition, McpTransportDefinition, PortableNpmPackageDefinition,
};

use crate::AssetError;
use crate::definition::{AssetDefinitionFile, normalize_relative_path};

pub const ENGINE_ADAPTER_DEFINITION_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/engine-adapter-definition.v1.schema.json";
pub const MCP_DEFINITION_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/mcp-definition.v1.schema.json";

/// 解析并验证可共享的引擎适配器 Definition。
///
/// Definition 只能描述可移植能力。账号、凭据、环境变量、本机路径和实例地址必须留在用户 Overlay 中。
pub fn parse_engine_adapter_definition(bytes: &[u8]) -> Result<EngineAdapterDefinition, AssetError> {
    let definition = serde_json::from_slice::<EngineAdapterDefinition>(bytes)?;
    validate_engine_adapter_definition(&definition)?;
    Ok(definition)
}

/// 解析并验证可共享的 MCP Definition。
///
/// 私密或设备相关字段不在强类型 DTO 中，`deny_unknown_fields` 会在反序列化阶段拒绝它们。
pub fn parse_mcp_definition(bytes: &[u8]) -> Result<McpDefinition, AssetError> {
    let definition = serde_json::from_slice::<McpDefinition>(bytes)?;
    validate_mcp_definition(&definition)?;
    Ok(definition)
}

pub(crate) fn validate_typed_definition(
    kind: AssetKind,
    entry_file: Option<&str>,
    runtime_id: Option<&str>,
    files: &[AssetDefinitionFile],
) -> Result<(), AssetError> {
    let expected_entry = match kind {
        AssetKind::EngineAdapter => "engine-adapter.json",
        AssetKind::Mcp => "mcp.json",
        AssetKind::Assistant | AssetKind::Skill => return Ok(()),
    };
    if entry_file != Some(expected_entry) {
        return Err(AssetError::InvalidMetadata(format!(
            "{kind:?} 资产入口必须是 {expected_entry}"
        )));
    }
    let content = files
        .iter()
        .find(|file| file.path == expected_entry)
        .map(|file| file.content.as_slice())
        .ok_or_else(|| AssetError::InvalidMetadata(format!("入口文件不存在：{expected_entry}")))?;
    let definition_runtime_id = match kind {
        AssetKind::EngineAdapter => parse_engine_adapter_definition(content)?.runtime_id,
        AssetKind::Mcp => parse_mcp_definition(content)?.runtime_id,
        AssetKind::Assistant | AssetKind::Skill => unreachable!("前置分支已经返回"),
    };
    if runtime_id != Some(definition_runtime_id.as_str()) {
        return Err(AssetError::InvalidMetadata(format!(
            "目录 runtimeId 与 {expected_entry} 中的 runtimeId 不一致"
        )));
    }
    Ok(())
}

fn validate_engine_adapter_definition(definition: &EngineAdapterDefinition) -> Result<(), AssetError> {
    validate_header(
        &definition.schema,
        definition.schema_version,
        definition.kind,
        AssetKind::EngineAdapter,
        ENGINE_ADAPTER_DEFINITION_SCHEMA_URL,
        &definition.id,
        &definition.runtime_id,
        &definition.display_name,
        definition.description.as_deref(),
    )?;
    if let Some(icon) = definition.icon.as_deref() {
        validate_safe_relative_path(icon, "icon")?;
    }
    validate_portable_arguments(&definition.protocol.arguments, "protocol.arguments")?;
    validate_command_name(&definition.runtime.command_name)?;
    if let Some(capabilities) = definition.capabilities.as_ref() {
        if capabilities.skills_directories.len() > 32 {
            return invalid("capabilities.skillsDirectories 最多允许 32 项");
        }
        let mut unique = HashSet::new();
        for path in &capabilities.skills_directories {
            validate_safe_relative_path(path, "capabilities.skillsDirectories")?;
            if !unique.insert(path) {
                return invalid("capabilities.skillsDirectories 不允许重复路径");
            }
        }
    }
    if let Some(schema) = definition.configuration_schema.as_ref() {
        validate_configuration_schema(schema, AssetConfigurationBindingTarget::Environment)?;
    }
    Ok(())
}

fn validate_mcp_definition(definition: &McpDefinition) -> Result<(), AssetError> {
    validate_header(
        &definition.schema,
        definition.schema_version,
        definition.kind,
        AssetKind::Mcp,
        MCP_DEFINITION_SCHEMA_URL,
        &definition.id,
        &definition.runtime_id,
        &definition.display_name,
        definition.description.as_deref(),
    )?;
    if let McpTransportDefinition::Stdio { package, arguments } = &definition.transport {
        validate_npm_package(package)?;
        validate_portable_arguments(arguments, "transport.arguments")?;
    }
    if let Some(schema) = definition.configuration_schema.as_ref() {
        let target = match &definition.transport {
            McpTransportDefinition::Stdio { .. } => AssetConfigurationBindingTarget::Environment,
            McpTransportDefinition::Sse {} | McpTransportDefinition::StreamableHttp {} => {
                AssetConfigurationBindingTarget::Header
            }
        };
        validate_configuration_schema(schema, target)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_header(
    schema: &str,
    schema_version: u32,
    kind: AssetKind,
    expected_kind: AssetKind,
    expected_schema: &str,
    id: &str,
    runtime_id: &str,
    display_name: &str,
    description: Option<&str>,
) -> Result<(), AssetError> {
    if schema != expected_schema {
        return invalid(format!("$schema 必须是 {expected_schema}"));
    }
    if schema_version != 1 {
        return invalid("schemaVersion 必须是 1");
    }
    if kind != expected_kind {
        return invalid(format!("kind 必须是 {expected_kind:?}"));
    }
    validate_local_id(id, "id")?;
    validate_local_id(runtime_id, "runtimeId")?;
    validate_text(display_name, 1, 128, "displayName")?;
    if let Some(description) = description {
        validate_text(description, 0, 2_048, "description")?;
    }
    Ok(())
}

fn validate_local_id(value: &str, field: &str) -> Result<(), AssetError> {
    if value.is_empty() || value.len() > 128 {
        return invalid(format!("{field} 长度无效"));
    }
    let mut requires_alphanumeric = true;
    for byte in value.bytes() {
        let alphanumeric = byte.is_ascii_lowercase() || byte.is_ascii_digit();
        if requires_alphanumeric {
            if !alphanumeric {
                return invalid(format!("{field} 必须使用小写可移植标识符"));
            }
            requires_alphanumeric = false;
        } else if matches!(byte, b'.' | b'_' | b':' | b'-') {
            requires_alphanumeric = true;
        } else if !alphanumeric {
            return invalid(format!("{field} 必须使用小写可移植标识符"));
        }
    }
    if requires_alphanumeric {
        return invalid(format!("{field} 不能以分隔符结尾"));
    }
    Ok(())
}

fn validate_safe_relative_path(value: &str, field: &str) -> Result<(), AssetError> {
    if value.chars().count() > 512 || normalize_relative_path(value)? != value {
        return invalid(format!("{field} 必须是规范化的安全相对路径"));
    }
    Ok(())
}

fn validate_portable_arguments(arguments: &[String], field: &str) -> Result<(), AssetError> {
    if arguments.len() > 64 {
        return invalid(format!("{field} 最多允许 64 项"));
    }
    for argument in arguments {
        let length = argument.chars().count();
        let looks_absolute = argument.starts_with('/')
            || argument.starts_with("\\\\")
            || argument
                .as_bytes()
                .get(1)
                .is_some_and(|byte| *byte == b':' && argument.as_bytes()[0].is_ascii_alphabetic());
        let portable = argument.replace('\\', "/").split(['=', '/']).all(|part| {
            !matches!(
                part.to_ascii_lowercase().as_str(),
                "users" | "home" | "documents and settings"
            )
        });
        if !(1..=512).contains(&length) || argument.chars().any(char::is_control) || looks_absolute || !portable {
            return invalid(format!("{field} 包含本机路径或不可移植参数"));
        }
    }
    Ok(())
}

fn validate_command_name(value: &str) -> Result<(), AssetError> {
    if value.is_empty()
        || value.len() > 128
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return invalid("runtime.commandName 无效");
    }
    Ok(())
}

fn validate_npm_package(package: &PortableNpmPackageDefinition) -> Result<(), AssetError> {
    if package.name.len() > 214 || !valid_npm_package_name(&package.name) {
        return invalid("package.name 不是可移植的 npm 包名");
    }
    Version::parse(&package.version)
        .map_err(|_| AssetError::InvalidMetadata("package.version 必须是精确 SemVer".into()))?;
    Ok(())
}

fn valid_npm_package_name(value: &str) -> bool {
    fn is_lowercase_or_digit(byte: u8) -> bool {
        byte.is_ascii_lowercase() || byte.is_ascii_digit()
    }

    fn valid_segment(value: &str) -> bool {
        !value.is_empty()
            && is_lowercase_or_digit(value.as_bytes()[0])
            && value
                .bytes()
                .all(|byte| is_lowercase_or_digit(byte) || matches!(byte, b'.' | b'_' | b'-'))
    }

    if let Some(scoped) = value.strip_prefix('@') {
        let Some((scope, name)) = scoped.split_once('/') else {
            return false;
        };
        !name.contains('/') && valid_segment(scope) && valid_segment(name)
    } else {
        !value.contains('/') && valid_segment(value)
    }
}

fn validate_configuration_schema(
    schema: &AssetConfigurationSchemaDefinition,
    allowed_target: AssetConfigurationBindingTarget,
) -> Result<(), AssetError> {
    if schema.fields.len() > 64 {
        return invalid("configurationSchema.fields 最多允许 64 项");
    }
    let mut keys = HashSet::new();
    let mut binding_names = HashSet::new();
    for field in &schema.fields {
        if field.key.is_empty()
            || field.key.len() > 128
            || !field.key.as_bytes()[0].is_ascii_alphabetic()
            || !field
                .key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return invalid("configurationSchema.fields.key 无效");
        }
        if !keys.insert(field.key.as_str()) {
            return invalid("configurationSchema.fields.key 不允许重复");
        }
        validate_text(&field.label, 1, 128, "configurationSchema.fields.label")?;
        if let Some(description) = field.description.as_deref() {
            validate_text(description, 0, 1_024, "configurationSchema.fields.description")?;
        }
        if field.binding.target != allowed_target {
            return invalid("configurationSchema.fields.binding.target 与运行 transport 不兼容");
        }
        let binding_name = field.binding.name.as_str();
        let valid_name = match allowed_target {
            AssetConfigurationBindingTarget::Environment => valid_environment_name(binding_name),
            AssetConfigurationBindingTarget::Header => valid_header_name(binding_name),
        };
        if !valid_name {
            return invalid("configurationSchema.fields.binding.name 无效");
        }
        if !binding_names.insert(binding_name.to_ascii_lowercase()) {
            return invalid("configurationSchema.fields.binding.name 不允许重复");
        }
    }
    Ok(())
}

fn valid_environment_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn valid_header_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn validate_text(value: &str, min: usize, max: usize, field: &str) -> Result<(), AssetError> {
    if !(min..=max).contains(&value.chars().count()) {
        return invalid(format!("{field} 长度无效"));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, AssetError> {
    Err(AssetError::InvalidMetadata(message.into()))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tjuaeui_api_types::{AssetKind, McpTransportDefinition};

    use super::*;

    const ENGINE_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/engine-adapter-definition.v1.complete.json");
    const MCP_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/mcp-definition.v1.complete.json");

    #[test]
    fn hub_engine_definition_fixture_round_trips_semantically() {
        let definition = parse_engine_adapter_definition(ENGINE_FIXTURE).unwrap();

        assert_eq!(definition.kind, AssetKind::EngineAdapter);
        assert_eq!(definition.id, "contract-acp");
        assert_eq!(definition.runtime_id, "contract-acp");
        assert_eq!(
            serde_json::to_value(definition).unwrap(),
            serde_json::from_slice::<Value>(ENGINE_FIXTURE).unwrap()
        );
    }

    #[test]
    fn hub_mcp_definition_fixture_round_trips_semantically() {
        let definition = parse_mcp_definition(MCP_FIXTURE).unwrap();

        assert_eq!(definition.kind, AssetKind::Mcp);
        assert_eq!(definition.id, "contract-mcp");
        assert!(matches!(definition.transport, McpTransportDefinition::Stdio { .. }));
        assert_eq!(
            serde_json::to_value(definition).unwrap(),
            serde_json::from_slice::<Value>(MCP_FIXTURE).unwrap()
        );
    }

    #[test]
    fn definitions_reject_overlay_secrets_paths_and_endpoints() {
        let mut engine = serde_json::from_slice::<Value>(ENGINE_FIXTURE).unwrap();
        engine["runtime"]["environment"] = json!({"OPENAI_API_KEY": "secret"});
        assert!(parse_engine_adapter_definition(&serde_json::to_vec(&engine).unwrap()).is_err());

        let mut mcp = serde_json::from_slice::<Value>(MCP_FIXTURE).unwrap();
        mcp["transport"]["headers"] = json!({"Authorization": "Bearer secret"});
        assert!(parse_mcp_definition(&serde_json::to_vec(&mcp).unwrap()).is_err());

        let mut mcp = serde_json::from_slice::<Value>(MCP_FIXTURE).unwrap();
        mcp["transport"]["instanceUrl"] = json!("https://private.example.test");
        assert!(parse_mcp_definition(&serde_json::to_vec(&mcp).unwrap()).is_err());

        let mut mcp = serde_json::from_slice::<Value>(MCP_FIXTURE).unwrap();
        mcp["transport"]["executablePath"] = json!("C:\\private\\server.exe");
        assert!(parse_mcp_definition(&serde_json::to_vec(&mcp).unwrap()).is_err());
    }

    #[test]
    fn definitions_reject_wrong_schema_kind_and_runtime_identifier() {
        let mut engine = serde_json::from_slice::<Value>(ENGINE_FIXTURE).unwrap();
        engine["$schema"] = json!(MCP_DEFINITION_SCHEMA_URL);
        assert!(parse_engine_adapter_definition(&serde_json::to_vec(&engine).unwrap()).is_err());

        let mut engine = serde_json::from_slice::<Value>(ENGINE_FIXTURE).unwrap();
        engine["kind"] = json!("mcp");
        assert!(parse_engine_adapter_definition(&serde_json::to_vec(&engine).unwrap()).is_err());

        let mut mcp = serde_json::from_slice::<Value>(MCP_FIXTURE).unwrap();
        mcp["runtimeId"] = json!("Contract MCP");
        assert!(parse_mcp_definition(&serde_json::to_vec(&mcp).unwrap()).is_err());
    }
}
