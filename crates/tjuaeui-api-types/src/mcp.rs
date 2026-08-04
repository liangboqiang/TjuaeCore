use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tjuaeui_common::{McpServerStatus, McpSource, TimestampMs};

// ---------------------------------------------------------------------------
// A. Transport types
// ---------------------------------------------------------------------------

/// MCP server transport configuration (tagged union).
///
/// `http` represents Streamable HTTP (the MCP standard); `sse` is legacy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpTransport {
    #[serde(rename = "stdio")]
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
    },
    #[serde(rename = "sse")]
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
    #[serde(rename = "http")]
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
}

// ---------------------------------------------------------------------------
// B. Tool description
// ---------------------------------------------------------------------------

/// MCP tool description returned from connection tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResponse {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// D. Server CRUD — Response DTOs
// ---------------------------------------------------------------------------

/// Full MCP server configuration response.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerResponse {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub enabled: bool,
    pub transport: McpTransport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<McpToolResponse>>,
    pub last_test_status: McpServerStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<TimestampMs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_json: Option<String>,
    pub builtin: bool,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

/// Detected MCP server entry for one-click import.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedMcpServerEntry {
    #[serde(flatten)]
    pub server: McpServerResponse,
    pub importable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_skip_reason: Option<String>,
}

/// Detected MCP servers for a single agent.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedMcpServerResponse {
    pub source: McpSource,
    pub servers: Vec<DetectedMcpServerEntry>,
}

// ---------------------------------------------------------------------------
// E. Connection test
// ---------------------------------------------------------------------------

/// Authentication method detected during connection test.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpAuthMethod {
    Oauth,
    Basic,
}

/// Machine-readable error code for MCP connection test failures.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum McpConnectionTestErrorCode {
    CommandNotFound,
    CommandPermissionDenied,
    CommandStartFailed,
    ConnectionFailed,
    HttpError,
    Timeout,
    RpcError,
    ProtocolError,
}

impl McpConnectionTestErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommandNotFound => "MCP_COMMAND_NOT_FOUND",
            Self::CommandPermissionDenied => "MCP_COMMAND_PERMISSION_DENIED",
            Self::CommandStartFailed => "MCP_COMMAND_START_FAILED",
            Self::ConnectionFailed => "MCP_CONNECTION_FAILED",
            Self::HttpError => "MCP_HTTP_ERROR",
            Self::Timeout => "MCP_TIMEOUT",
            Self::RpcError => "MCP_RPC_ERROR",
            Self::ProtocolError => "MCP_PROTOCOL_ERROR",
        }
    }
}

/// Result of an MCP server connection test.
#[derive(Debug, Clone, Serialize)]
pub struct McpConnectionTestResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<McpToolResponse>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<McpConnectionTestErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_auth: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<McpAuthMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub www_authenticate: Option<String>,
}

// ---------------------------------------------------------------------------
