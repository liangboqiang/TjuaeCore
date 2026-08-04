use serde::{Deserialize, Serialize};

use crate::AssetKind;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PortablePackageEcosystem {
    Npm,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PortablePackageRunner {
    Bunx,
    Npx,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableNpmPackageDefinition {
    pub ecosystem: PortablePackageEcosystem,
    pub name: String,
    pub version: String,
    pub runner: PortablePackageRunner,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EngineAdapterProtocolType {
    Acp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EngineAdapterTransport {
    Stdio,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineAdapterProtocolDefinition {
    pub r#type: EngineAdapterProtocolType,
    pub transport: EngineAdapterTransport,
    #[serde(default)]
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableRuntimeDefinition {
    pub command_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineAdapterCapabilitiesDefinition {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub authentication_required: bool,
    #[serde(default)]
    pub skills_directories: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetConfigurationValueType {
    String,
    Number,
    Boolean,
}

/// Definition 配置字段在会话启动时注入运行时的位置。
///
/// 这里只声明目标，不保存任何用户值或凭据。Core 会根据资产类型和 MCP
/// transport 进一步收紧允许的目标，并在每次启动时即时解析 Overlay。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetConfigurationBindingTarget {
    Environment,
    Header,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetConfigurationFieldBindingDefinition {
    pub target: AssetConfigurationBindingTarget,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetConfigurationFieldDefinition {
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub value_type: AssetConfigurationValueType,
    pub required: bool,
    pub secret: bool,
    pub binding: AssetConfigurationFieldBindingDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssetConfigurationSchemaDefinition {
    #[serde(default)]
    pub fields: Vec<AssetConfigurationFieldDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineAdapterDefinition {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: u32,
    pub kind: AssetKind,
    pub id: String,
    pub runtime_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub protocol: EngineAdapterProtocolDefinition,
    pub runtime: PortableRuntimeDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<EngineAdapterCapabilitiesDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_schema: Option<AssetConfigurationSchemaDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum McpTransportDefinition {
    Stdio {
        package: PortableNpmPackageDefinition,
        #[serde(default)]
        arguments: Vec<String>,
    },
    Sse {},
    StreamableHttp {},
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCapabilitiesDefinition {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub resources: bool,
    #[serde(default)]
    pub prompts: bool,
    #[serde(default)]
    pub sampling: bool,
    #[serde(default)]
    pub logging: bool,
    #[serde(default)]
    pub completions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpDefinition {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: u32,
    pub kind: AssetKind,
    pub id: String,
    pub runtime_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub transport: McpTransportDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<McpCapabilitiesDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_schema: Option<AssetConfigurationSchemaDefinition>,
}
