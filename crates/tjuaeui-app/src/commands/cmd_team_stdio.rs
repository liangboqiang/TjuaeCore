//! `tjuaecore mcp-team-stdio` subcommand: MCP stdio server for team tools.
//!
//! Uses the `rmcp` crate (Rust MCP SDK) for protocol handling. Tool calls are
//! forwarded to the TeamMcpServer TCP listener via 4-byte big-endian
//! length-prefixed JSON frames — the same wire protocol used by `mcp-bridge`,
//! but with proper tool registration via rmcp instead of transparent proxying.
//!
//! Each tool call opens a fresh TCP connection, sends an `initialize` frame
//! (injecting auth_token + slot_id), then sends the `tools/call` frame, reads
//! the response, and closes the connection (one-shot mode).

use std::process::ExitCode;

use crate::commands::error::{CliBoundaryCode, CliBoundaryError, missing_env, parse_required_port};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ListToolsResult, Tool};
use rmcp::{schemars, service::ServiceExt, tool, tool_router, transport};
use serde::Deserialize;
use tjuaeui_api_types::TeamMcpStdioConfig;
use tjuaeui_team::mcp::protocol::{read_frame, write_frame};
use tokio::net::TcpStream;

const SUBCOMMAND: &str = "mcp-team-stdio";
const CONNECT_HOST: &str = "127.0.0.1";
const ERR_JSON_SERIALIZE: &str = "failed to serialize MCP JSON frame";
const ERR_TCP_CONNECT: &str = "failed to connect to local MCP TCP listener";
const ERR_TCP_WRITE: &str = "failed to write MCP frame to TCP listener";
const ERR_TCP_READ: &str = "failed to read MCP frame from TCP listener";
const ERR_TOOL_REMOTE: &str = "local team tool returned an error";
const ERR_TOOL_RESPONSE_UNEXPECTED: &str = "unexpected local team tool response";

pub async fn run_team_stdio() -> ExitCode {
    let env = match TeamStdioEnv::from_env() {
        Ok(env) => env,
        Err(err) => {
            eprintln!("{}", err.stderr_line());
            return err.exit_code();
        }
    };

    let server = TeamStdioServer {
        port: env.port,
        token: env.token,
        slot_id: env.slot_id,
    };

    let transport = transport::io::stdio();
    match server.serve(transport).await {
        Ok(peer) => {
            if let Err(_e) = peer.waiting().await {
                let err = CliBoundaryError::new(
                    CliBoundaryCode::McpSessionEndedWithError,
                    SUBCOMMAND,
                    "MCP stdio session ended with an error",
                );
                eprintln!("{}", err.stderr_line());
                err.exit_code()
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(_e) => {
            let err = CliBoundaryError::new(
                CliBoundaryCode::McpStdioServeFailed,
                SUBCOMMAND,
                "failed to start MCP stdio server",
            );
            eprintln!("{}", err.stderr_line());
            err.exit_code()
        }
    }
}

#[derive(Clone, Debug)]
struct TeamStdioEnv {
    port: u16,
    token: String,
    slot_id: String,
}

impl TeamStdioEnv {
    fn from_env() -> Result<Self, CliBoundaryError> {
        let port_raw = std::env::var(TeamMcpStdioConfig::ENV_PORT)
            .map_err(|_| missing_env(SUBCOMMAND, TeamMcpStdioConfig::ENV_PORT))?;
        let token = std::env::var(TeamMcpStdioConfig::ENV_TOKEN)
            .map_err(|_| missing_env(SUBCOMMAND, TeamMcpStdioConfig::ENV_TOKEN))?;
        let slot_id = std::env::var(TeamMcpStdioConfig::ENV_SLOT_ID)
            .map_err(|_| missing_env(SUBCOMMAND, TeamMcpStdioConfig::ENV_SLOT_ID))?;
        Self::from_values(&port_raw, token, slot_id)
    }

    fn from_values(
        port_raw: &str,
        token: impl Into<String>,
        slot_id: impl Into<String>,
    ) -> Result<Self, CliBoundaryError> {
        Ok(Self {
            port: parse_required_port(SUBCOMMAND, TeamMcpStdioConfig::ENV_PORT, port_raw)?,
            token: token.into(),
            slot_id: slot_id.into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TeamStdioServer {
    port: u16,
    token: String,
    slot_id: String,
}

// ---------------------------------------------------------------------------
// Parameter types
// ---------------------------------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema)]
struct SendMessageParams {
    /// 目标智能体 slot_id；广播时使用 "*"。
    to: String,
    /// 消息内容。
    message: String,
    /// 要转发给目标智能体的附件绝对路径。
    #[serde(default)]
    files: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpawnAgentParams {
    /// 智能体显示名称。
    name: String,
    /// 可用助手目录中的助手标识。
    #[serde(default)]
    assistant_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TaskCreateParams {
    /// 任务主题。
    subject: String,
    /// 任务说明。
    #[serde(default)]
    description: Option<String>,
    /// 负责智能体的 slot_id。
    #[serde(default)]
    owner: Option<String>,
    /// 本任务依赖的任务 ID。
    #[serde(default)]
    blocked_by: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TaskUpdateParams {
    /// 要更新的任务 ID。
    task_id: String,
    /// 新状态：pending、in_progress、completed 或 deleted。
    #[serde(default)]
    status: Option<String>,
    /// 新说明。
    #[serde(default)]
    description: Option<String>,
    /// 新的负责智能体 slot_id。
    #[serde(default)]
    owner: Option<String>,
    /// 新依赖列表。
    #[serde(default)]
    blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum TaskListStatusParam {
    Single(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskListParams {
    /// 只返回由该智能体 slot_id 负责的任务。
    #[serde(default)]
    owner: Option<String>,
    /// 只返回指定状态的任务。
    #[serde(default)]
    status: Option<TaskListStatusParam>,
    /// 未指定 status 时是否包含已删除任务，服务端默认为 true。
    #[serde(default)]
    include_deleted: Option<bool>,
    /// 最多返回的任务数，由 TCP 服务校验并限制。
    #[serde(default)]
    limit: Option<i64>,
}

impl TaskListParams {
    fn into_json(self) -> serde_json::Value {
        let status = match self.status {
            Some(TaskListStatusParam::Single(value)) => serde_json::json!(value),
            Some(TaskListStatusParam::Many(values)) => serde_json::json!(values),
            None => serde_json::Value::Null,
        };
        let mut args = serde_json::json!({
            "owner": self.owner,
            "status": status,
            "include_deleted": self.include_deleted,
            "limit": self.limit,
        });
        args.as_object_mut()
            .expect("任务列表参数必须序列化为对象")
            .retain(|_, value| !value.is_null());
        args
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RenameAgentParams {
    /// 要重命名的智能体 slot_id。
    slot_id: String,
    /// 新显示名称。
    new_name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ShutdownAgentParams {
    /// 要下线的智能体 slot_id。
    slot_id: String,
    /// 下线原因。
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DescribeAssistantParams {
    /// “可用助手”目录中的助手 ID。
    assistant_id: String,
    /// 说明使用的语言区域，例如 "en" 或 "zh"；省略时使用默认值。
    #[serde(default)]
    locale: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool router
// ---------------------------------------------------------------------------

#[tool_router]
impl TeamStdioServer {
    #[tool(
        name = "team_send_message",
        description = "向队员发送消息，或通过 to=\"*\" 广播。委派依赖用户附件的工作时，在 files 中转发附件绝对路径。"
    )]
    async fn send_message(&self, Parameters(params): Parameters<SendMessageParams>) -> CallToolResult {
        self.forward_to_tcp(
            "team_send_message",
            &serde_json::json!({
                "to": params.to,
                "message": params.message,
                "files": params.files,
            }),
        )
        .await
    }

    #[tool(
        name = "team_spawn_agent",
        description = r#"创建一个加入团队的新队员智能体。

只有满足以下条件之一时才能使用：
- 用户已在之前的消息中明确批准人员方案。
- 用户明确要求立即创建某个指定队员。

正常规划流程中，调用本工具前必须：
- 先用一句话说明增加队员的价值。
- 告知用户推荐哪些队员。
- 用表格列出名称、职责和推荐助手。
- 询问按原方案创建还是调整名称、职责或助手。
- 提醒用户：后续若配置不合适，仍可要求替换或调整队员。
- 不要在提出方案的同一回合调用本工具；等待后续消息中的明确批准。

调用时必须提供助手目录中的 assistant_id，不要传模型。新队员使用所选助手的
已配置或默认模型，用户可以在 UI 模型选择器中调整。

新智能体创建并加入团队后，可以向其分配任务和发送消息。"#
    )]
    async fn spawn_agent(&self, Parameters(params): Parameters<SpawnAgentParams>) -> CallToolResult {
        self.forward_to_tcp(
            "team_spawn_agent",
            &serde_json::json!({
                "name": params.name,
                "assistant_id": params.assistant_id,
            }),
        )
        .await
    }

    #[tool(name = "team_task_create", description = "在团队任务看板上创建任务。")]
    async fn task_create(&self, Parameters(params): Parameters<TaskCreateParams>) -> CallToolResult {
        self.forward_to_tcp(
            "team_task_create",
            &serde_json::json!({
                "subject": params.subject,
                "description": params.description,
                "owner": params.owner,
                "blocked_by": params.blocked_by,
            }),
        )
        .await
    }

    #[tool(name = "team_task_update", description = "更新团队任务看板中的现有任务。")]
    async fn task_update(&self, Parameters(params): Parameters<TaskUpdateParams>) -> CallToolResult {
        self.forward_to_tcp(
            "team_task_update",
            &serde_json::json!({
                "task_id": params.task_id,
                "status": params.status,
                "description": params.description,
                "owner": params.owner,
                "blocked_by": params.blocked_by,
            }),
        )
        .await
    }

    #[tool(
        name = "team_task_list",
        description = "列出团队任务看板。传入 {} 返回完整看板，也可用 owner/status/include_deleted/limit 筛选。"
    )]
    async fn task_list(&self, Parameters(params): Parameters<TaskListParams>) -> CallToolResult {
        self.forward_to_tcp("team_task_list", &params.into_json()).await
    }

    #[tool(name = "team_members", description = "列出所有团队成员及其角色和当前状态。")]
    async fn members(&self) -> CallToolResult {
        self.forward_to_tcp("team_members", &serde_json::json!({})).await
    }

    #[tool(name = "team_rename_agent", description = "重命名团队成员；仅限负责人。")]
    async fn rename_agent(&self, Parameters(params): Parameters<RenameAgentParams>) -> CallToolResult {
        self.forward_to_tcp(
            "team_rename_agent",
            &serde_json::json!({ "slot_id": params.slot_id, "new_name": params.new_name }),
        )
        .await
    }

    #[tool(
        name = "team_shutdown_agent",
        description = "发起队员下线；仅限负责人。向目标智能体发送 shutdown_request。"
    )]
    async fn shutdown_agent(&self, Parameters(params): Parameters<ShutdownAgentParams>) -> CallToolResult {
        self.forward_to_tcp(
            "team_shutdown_agent",
            &serde_json::json!({ "slot_id": params.slot_id, "reason": params.reason }),
        )
        .await
    }

    #[tool(
        name = "team_list_assistants",
        description = "列出可用于创建团队队员的助手。返回真实助手目录，包括 assistant_id、名称、后端、说明和技能。\n\n需要准确的队员 assistant_id 时，应在 team_spawn_agent 之前调用本工具。不要根据 claude/codex/gemini 等后端名称猜测；只能使用本工具返回的 assistant_id。"
    )]
    async fn list_assistants(&self) -> CallToolResult {
        self.forward_to_tcp("team_list_assistants", &serde_json::json!({}))
            .await
    }

    #[tool(
        name = "team_describe_assistant",
        description = "创建队员前获取助手详情。\n\n返回助手的完整说明、已启用技能和示例任务，以判断是否符合用户请求。系统提示词中的\n单行目录有两个或更多相关候选时使用本工具。\n\n先用 team_list_assistants 查找候选 assistant_id；确认匹配后，使用同一 assistant_id\n调用 team_spawn_agent。"
    )]
    async fn describe_assistant(&self, Parameters(params): Parameters<DescribeAssistantParams>) -> CallToolResult {
        self.forward_to_tcp(
            "team_describe_assistant",
            &serde_json::json!({ "assistant_id": params.assistant_id, "locale": params.locale }),
        )
        .await
    }
}

#[rmcp::tool_handler(router = Self::tool_router())]
impl rmcp::ServerHandler for TeamStdioServer {
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let tools = self
            .list_tools_from_tcp()
            .await
            .map_err(|_| rmcp::ErrorData::internal_error("列出本地团队工具失败", None))?;
        Ok(ListToolsResult::with_all_items(tools))
    }
}

// ---------------------------------------------------------------------------
// TCP forwarding
// ---------------------------------------------------------------------------

impl TeamStdioServer {
    /// One-shot TCP forward: connect → initialize (with auth) → tools/call → read response → close.
    async fn forward_to_tcp(&self, tool_name: &str, args: &serde_json::Value) -> CallToolResult {
        match self.do_forward(tool_name, args).await {
            Ok(result) => tool_success(result),
            Err(ToolForwardError::Boundary(err)) => {
                eprintln!("{}", err.stderr_line());
                tool_error(err.code(), tool_error_message(err.code()), None, None)
            }
            Err(ToolForwardError::Tool {
                code,
                message,
                upstream_code,
                domain_code,
            }) => tool_error(code, message, upstream_code, domain_code),
        }
    }

    async fn do_forward(&self, tool_name: &str, args: &serde_json::Value) -> Result<String, ToolForwardError> {
        let mut stream = TcpStream::connect((CONNECT_HOST, self.port))
            .await
            .map_err(|_| tcp_connect_error(self.port))?;
        stream.set_nodelay(true).ok();

        // initialize with auth
        let init_frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "auth_token": self.token,
                "slot_id": self.slot_id,
            }
        });
        let init_bytes = serde_json::to_vec(&init_frame).map_err(|_| json_serialize_error())?;
        write_frame(&mut stream, &init_bytes)
            .await
            .map_err(|_| tcp_write_error())?;
        let init_resp = read_frame(&mut stream).await.map_err(|_| tcp_read_error())?;
        drop(init_resp);

        // tools/call
        let call_frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        });
        let call_bytes = serde_json::to_vec(&call_frame).map_err(|_| json_serialize_error())?;
        write_frame(&mut stream, &call_bytes)
            .await
            .map_err(|_| tcp_write_error())?;
        let resp_bytes = read_frame(&mut stream).await.map_err(|_| tcp_read_error())?;

        let text = String::from_utf8_lossy(&resp_bytes).into_owned();

        parse_tool_response(&text)
    }

    async fn list_tools_from_tcp(&self) -> Result<Vec<Tool>, ToolForwardError> {
        let mut stream = TcpStream::connect((CONNECT_HOST, self.port))
            .await
            .map_err(|_| tcp_connect_error(self.port))?;
        stream.set_nodelay(true).ok();

        let init_frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "auth_token": self.token,
                "slot_id": self.slot_id,
            }
        });
        let init_bytes = serde_json::to_vec(&init_frame).map_err(|_| json_serialize_error())?;
        write_frame(&mut stream, &init_bytes)
            .await
            .map_err(|_| tcp_write_error())?;
        let init_resp = read_frame(&mut stream).await.map_err(|_| tcp_read_error())?;
        let init_text = String::from_utf8_lossy(&init_resp).into_owned();
        parse_json_rpc_success(&init_text)?;

        let list_frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
        });
        let list_bytes = serde_json::to_vec(&list_frame).map_err(|_| json_serialize_error())?;
        write_frame(&mut stream, &list_bytes)
            .await
            .map_err(|_| tcp_write_error())?;
        let resp_bytes = read_frame(&mut stream).await.map_err(|_| tcp_read_error())?;
        let text = String::from_utf8_lossy(&resp_bytes).into_owned();
        parse_tools_list_response(&text)
    }
}

#[derive(Debug)]
enum ToolForwardError {
    Boundary(CliBoundaryError),
    Tool {
        code: CliBoundaryCode,
        message: &'static str,
        upstream_code: Option<serde_json::Value>,
        domain_code: Option<serde_json::Value>,
    },
}

impl From<CliBoundaryError> for ToolForwardError {
    fn from(error: CliBoundaryError) -> Self {
        Self::Boundary(error)
    }
}

fn parse_tool_response(text: &str) -> Result<String, ToolForwardError> {
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|_| tool_response_unexpected())?;
    if value.get("error").is_some() {
        return Err(remote_tool_error(
            extract_nested_code(&value, &["error", "code"]),
            extract_nested_code(&value, &["error", "data", "domainCode"])
                .or_else(|| extract_nested_code(&value, &["error", "data", "code"]))
                .or_else(|| extract_nested_code(&value, &["error", "data", "errorCode"])),
        ));
    }
    let result = value.get("result").ok_or_else(tool_response_unexpected)?;
    if let Some(result) = result.as_str() {
        return Ok(result.to_owned());
    }
    if result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(remote_tool_error(
            extract_nested_code(result, &["structuredContent", "upstreamCode"])
                .or_else(|| extract_nested_code(result, &["upstreamCode"])),
            extract_nested_code(result, &["structuredContent", "domainCode"])
                .or_else(|| extract_nested_code(result, &["structuredContent", "code"]))
                .or_else(|| extract_nested_code(result, &["structuredContent", "errorCode"]))
                .or_else(|| extract_nested_code(result, &["domainCode"]))
                .or_else(|| extract_nested_code(result, &["code"]))
                .or_else(|| extract_nested_code(result, &["errorCode"])),
        ));
    }
    if let Some(content) = result.get("content").and_then(serde_json::Value::as_array) {
        let text_parts: Vec<&str> = content
            .iter()
            .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
            .collect();
        if !text_parts.is_empty() {
            return Ok(text_parts.join("\n"));
        }
    }
    Err(tool_response_unexpected().into())
}

fn parse_json_rpc_success(text: &str) -> Result<serde_json::Value, ToolForwardError> {
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|_| tool_response_unexpected())?;
    if value.get("error").is_some() {
        return Err(remote_tool_error(
            extract_nested_code(&value, &["error", "code"]),
            extract_nested_code(&value, &["error", "data", "domainCode"])
                .or_else(|| extract_nested_code(&value, &["error", "data", "code"]))
                .or_else(|| extract_nested_code(&value, &["error", "data", "errorCode"])),
        ));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(tool_response_unexpected)
        .map_err(Into::into)
}

#[derive(Deserialize)]
struct RemoteToolDescriptor {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, alias = "inputSchema")]
    input_schema: serde_json::Value,
}

fn parse_tools_list_response(text: &str) -> Result<Vec<Tool>, ToolForwardError> {
    let result = parse_json_rpc_success(text)?;
    let descriptors = serde_json::from_value::<Vec<RemoteToolDescriptor>>(
        result.get("tools").cloned().ok_or_else(tool_response_unexpected)?,
    )
    .map_err(|_| tool_response_unexpected())?;

    descriptors
        .into_iter()
        .map(|descriptor| {
            let schema = descriptor
                .input_schema
                .as_object()
                .cloned()
                .ok_or_else(tool_response_unexpected)?;
            Ok(Tool::new(descriptor.name, descriptor.description, schema))
        })
        .collect()
}

fn json_serialize_error() -> CliBoundaryError {
    CliBoundaryError::new(CliBoundaryCode::McpJsonSerializeFailed, SUBCOMMAND, ERR_JSON_SERIALIZE)
}

fn tcp_connect_error(port: u16) -> CliBoundaryError {
    CliBoundaryError::new(CliBoundaryCode::McpTcpConnectFailed, SUBCOMMAND, ERR_TCP_CONNECT)
        .with_field("host", CONNECT_HOST)
        .with_field("port", port.to_string())
}

fn tcp_write_error() -> CliBoundaryError {
    CliBoundaryError::new(CliBoundaryCode::McpTcpWriteFailed, SUBCOMMAND, ERR_TCP_WRITE)
}

fn tcp_read_error() -> CliBoundaryError {
    CliBoundaryError::new(CliBoundaryCode::McpTcpReadFailed, SUBCOMMAND, ERR_TCP_READ)
}

fn remote_tool_error(
    upstream_code: Option<serde_json::Value>,
    domain_code: Option<serde_json::Value>,
) -> ToolForwardError {
    ToolForwardError::Tool {
        code: CliBoundaryCode::McpToolRemoteError,
        message: ERR_TOOL_REMOTE,
        upstream_code,
        domain_code,
    }
}

fn tool_response_unexpected() -> CliBoundaryError {
    CliBoundaryError::new(
        CliBoundaryCode::McpToolResponseUnexpected,
        SUBCOMMAND,
        ERR_TOOL_RESPONSE_UNEXPECTED,
    )
}

fn tool_success(text: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

fn tool_error(
    code: CliBoundaryCode,
    message: &'static str,
    upstream_code: Option<serde_json::Value>,
    domain_code: Option<serde_json::Value>,
) -> CallToolResult {
    let mut structured = serde_json::json!({
        "code": code.as_str(),
        "message": message,
    });
    if let Some(upstream_code) = upstream_code {
        structured["upstreamCode"] = upstream_code;
    }
    if let Some(domain_code) = domain_code {
        structured["domainCode"] = domain_code;
    }

    let mut result = CallToolResult::error(vec![Content::text(message)]);
    result.structured_content = Some(structured);
    result
}

fn extract_nested_code(value: &serde_json::Value, path: &[&str]) -> Option<serde_json::Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    match current {
        serde_json::Value::String(_) | serde_json::Value::Number(_) => Some(current.clone()),
        _ => None,
    }
}

fn tool_error_message(code: CliBoundaryCode) -> &'static str {
    match code {
        CliBoundaryCode::McpJsonSerializeFailed => ERR_JSON_SERIALIZE,
        CliBoundaryCode::McpTcpConnectFailed => ERR_TCP_CONNECT,
        CliBoundaryCode::McpTcpWriteFailed => ERR_TCP_WRITE,
        CliBoundaryCode::McpTcpReadFailed => ERR_TCP_READ,
        CliBoundaryCode::McpToolRemoteError => ERR_TOOL_REMOTE,
        CliBoundaryCode::McpToolResponseUnexpected => ERR_TOOL_RESPONSE_UNEXPECTED,
        _ => "team stdio tool forwarding failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::error::CliBoundaryCode;
    use serde_json::json;
    use tokio::net::TcpListener;

    fn first_text(result: &CallToolResult) -> &str {
        result.content[0].as_text().expect("text content").text.as_str()
    }

    #[test]
    fn team_stdio_env_rejects_invalid_port_with_stable_code() {
        let err = TeamStdioEnv::from_values("bad", "tok", "slot-a").unwrap_err();
        assert_eq!(err.code(), CliBoundaryCode::McpEnvInvalidPort);
        assert_eq!(err.exit_code(), std::process::ExitCode::from(2));
    }

    #[test]
    fn team_stdio_env_accepts_valid_values() {
        let env = TeamStdioEnv::from_values("12345", "tok", "slot-a").unwrap();
        assert_eq!(env.port, 12345);
        assert_eq!(env.token, "tok");
        assert_eq!(env.slot_id, "slot-a");
    }

    #[test]
    fn spawn_agent_params_reject_legacy_custom_agent_id_alias() {
        let parsed = serde_json::from_value::<SpawnAgentParams>(json!({
            "name": "helper",
            "custom_agent_id": "assistant-123",
        }));
        assert!(parsed.is_err(), "legacy custom_agent_id alias should be rejected");
        let err = parsed.err().unwrap();

        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("custom_agent_id"));
    }

    #[test]
    fn describe_assistant_params_reject_legacy_custom_agent_id_alias() {
        let parsed = serde_json::from_value::<DescribeAssistantParams>(json!({
            "custom_agent_id": "assistant-123",
        }));
        assert!(parsed.is_err(), "legacy custom_agent_id alias should be rejected");
        let err = parsed.err().unwrap();

        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("custom_agent_id"));
    }

    #[test]
    fn task_list_params_accept_filter_arguments() {
        let parsed = serde_json::from_value::<TaskListParams>(json!({
            "owner": "worker-1",
            "status": ["pending", "in_progress"],
            "include_deleted": false,
            "limit": 50
        }))
        .expect("task_list filters should parse");

        let forwarded = parsed.into_json();
        assert_eq!(forwarded["owner"], "worker-1");
        assert_eq!(forwarded["status"], json!(["pending", "in_progress"]));
        assert_eq!(forwarded["include_deleted"], false);
        assert_eq!(forwarded["limit"], 50);
    }

    #[test]
    fn task_list_params_reject_unknown_fields() {
        let parsed = serde_json::from_value::<TaskListParams>(json!({
            "slot_id": "worker-1"
        }));
        assert!(parsed.is_err());
        assert!(parsed.unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn team_stdio_task_list_schema_exposes_filters() {
        let router = TeamStdioServer::tool_router();
        let tools = router.list_all();
        let task_list = tools
            .iter()
            .find(|tool| tool.name == "team_task_list")
            .expect("team_task_list tool missing");
        let properties = task_list.input_schema["properties"].as_object().unwrap();
        assert!(properties.contains_key("owner"));
        assert!(properties.contains_key("status"));
        assert!(properties.contains_key("include_deleted"));
        assert!(properties.contains_key("limit"));
    }

    #[test]
    fn team_stdio_router_exposes_team_list_assistants() {
        let router = TeamStdioServer::tool_router();
        let tools = router.list_all();
        let team_list_assistants = tools
            .iter()
            .find(|tool| tool.name == "team_list_assistants")
            .expect("team_list_assistants tool missing");
        let properties = team_list_assistants.input_schema["properties"].as_object().unwrap();
        assert!(
            properties.is_empty(),
            "team_list_assistants should not accept arguments"
        );
    }

    #[test]
    fn team_stdio_descriptions_match_prompt_registry() {
        let router = TeamStdioServer::tool_router();
        let tools = router.list_all();
        let mut actual_names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        actual_names.sort_unstable();
        let mut expected_names: Vec<_> = tjuaeui_team_prompts::tools::team_tool_specs()
            .iter()
            .map(|spec| spec.name)
            .collect();
        expected_names.sort_unstable();
        assert_eq!(actual_names, expected_names, "stdio tool registry drift");

        for spec in tjuaeui_team_prompts::tools::team_tool_specs() {
            let tool = tools
                .iter()
                .find(|tool| tool.name == spec.name)
                .unwrap_or_else(|| panic!("missing tool {}", spec.name));
            let description = tool
                .description
                .as_ref()
                .unwrap_or_else(|| panic!("missing description for {}", spec.name));
            assert_eq!(
                description.as_ref(),
                spec.description,
                "description drift for {}",
                spec.name
            );
        }
    }

    #[tokio::test]
    async fn list_tools_uses_team_server_filtered_descriptors() {
        let listener = TcpListener::bind((CONNECT_HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let init = read_frame(&mut socket).await.unwrap();
            let init_value: serde_json::Value = serde_json::from_slice(&init).unwrap();
            assert_eq!(init_value["method"], "initialize");
            assert_eq!(init_value["params"]["slot_id"], "worker-1");

            let init_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {}
            }))
            .unwrap();
            write_frame(&mut socket, &init_response).await.unwrap();

            let list = read_frame(&mut socket).await.unwrap();
            let list_value: serde_json::Value = serde_json::from_slice(&list).unwrap();
            assert_eq!(list_value["method"], "tools/list");

            let list_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [
                        {
                            "name": "team_send_message",
                            "description": "Send a message",
                            "input_schema": {
                                "type": "object",
                                "properties": {
                                    "to": { "type": "string" },
                                    "message": { "type": "string" }
                                },
                                "required": ["to", "message"]
                            }
                        }
                    ]
                }
            }))
            .unwrap();
            write_frame(&mut socket, &list_response).await.unwrap();
        });
        let server = TeamStdioServer {
            port,
            token: "dummy-token".into(),
            slot_id: "worker-1".into(),
        };

        let tools = server.list_tools_from_tcp().await.expect("tools/list");

        accept_task.await.unwrap();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, vec!["team_send_message"]);
        assert!(!names.contains(&"team_spawn_agent"));
        assert!(!names.contains(&"team_rename_agent"));
        assert!(!names.contains(&"team_shutdown_agent"));
        assert_eq!(
            tools[0]
                .input_schema
                .get("properties")
                .and_then(|value| value.as_object())
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn forward_to_tcp_reports_read_failure_after_accept_close() {
        let listener = TcpListener::bind((CONNECT_HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept_task = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let server = TeamStdioServer {
            port,
            token: "dummy-token".into(),
            slot_id: "dummy-slot".into(),
        };

        let result = server.forward_to_tcp("team_task_list", &json!({})).await;

        accept_task.await.unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(first_text(&result), "failed to read MCP frame from TCP listener");
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "MCP_TCP_READ_FAILED"
        );
    }

    #[tokio::test]
    async fn forward_to_tcp_sanitizes_tool_level_error_result() {
        let listener = TcpListener::bind((CONNECT_HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _init = read_frame(&mut socket).await.unwrap();
            let init_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {}
            }))
            .unwrap();
            write_frame(&mut socket, &init_response).await.unwrap();

            let _call = read_frame(&mut socket).await.unwrap();
            let tool_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": "upstream failure for conv-secret-123"
                        }
                    ],
                    "isError": true
                }
            }))
            .unwrap();
            write_frame(&mut socket, &tool_response).await.unwrap();
        });
        let server = TeamStdioServer {
            port,
            token: "dummy-token".into(),
            slot_id: "dummy-slot".into(),
        };

        let result = server.forward_to_tcp("team_task_list", &json!({})).await;

        accept_task.await.unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(first_text(&result), "local team tool returned an error");
        assert_eq!(
            result.structured_content.as_ref().unwrap()["code"],
            "MCP_TOOL_REMOTE_ERROR"
        );
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("conv-secret-123"));
    }

    #[tokio::test]
    async fn spawn_agent_forwards_only_assistant_first_arguments() {
        let listener = TcpListener::bind((CONNECT_HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _init = read_frame(&mut socket).await.unwrap();
            let init_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {}
            }))
            .unwrap();
            write_frame(&mut socket, &init_response).await.unwrap();

            let call = read_frame(&mut socket).await.unwrap();
            let call_value: serde_json::Value = serde_json::from_slice(&call).unwrap();
            assert_eq!(call_value["params"]["name"], json!("team_spawn_agent"));
            let arguments = &call_value["params"]["arguments"];
            assert_eq!(arguments["name"], json!("CodexCLI"));
            assert_eq!(arguments["assistant_id"], json!("bare:8e1acf31"));
            assert!(arguments.get("model").is_none());
            assert!(arguments.get("role").is_none());
            assert!(arguments.get("agent_type").is_none());
            assert!(arguments.get("backend").is_none());

            let tool_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [{ "type": "text", "text": "ok" }],
                    "isError": false
                }
            }))
            .unwrap();
            write_frame(&mut socket, &tool_response).await.unwrap();
        });
        let server = TeamStdioServer {
            port,
            token: "dummy-token".into(),
            slot_id: "dummy-slot".into(),
        };

        let result = server
            .spawn_agent(Parameters(SpawnAgentParams {
                name: "CodexCLI".into(),
                assistant_id: Some("bare:8e1acf31".into()),
            }))
            .await;

        accept_task.await.unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(first_text(&result), "ok");
    }

    #[tokio::test]
    async fn task_list_forwards_filter_arguments() {
        let listener = TcpListener::bind((CONNECT_HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _init = read_frame(&mut socket).await.unwrap();
            let init_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {}
            }))
            .unwrap();
            write_frame(&mut socket, &init_response).await.unwrap();

            let call = read_frame(&mut socket).await.unwrap();
            let call_value: serde_json::Value = serde_json::from_slice(&call).unwrap();
            assert_eq!(call_value["params"]["name"], json!("team_task_list"));
            assert_eq!(call_value["params"]["arguments"]["owner"], json!("worker-1"));
            assert_eq!(
                call_value["params"]["arguments"]["status"],
                json!(["pending", "in_progress"])
            );
            assert_eq!(call_value["params"]["arguments"]["include_deleted"], json!(false));
            assert_eq!(call_value["params"]["arguments"]["limit"], json!(50));

            let tool_response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [
                        {
                            "type": "text",
                            "text": "[]"
                        }
                    ]
                }
            }))
            .unwrap();
            write_frame(&mut socket, &tool_response).await.unwrap();
        });
        let server = TeamStdioServer {
            port,
            token: "dummy-token".into(),
            slot_id: "dummy-slot".into(),
        };
        let args = TaskListParams {
            owner: Some("worker-1".into()),
            status: Some(TaskListStatusParam::Many(vec!["pending".into(), "in_progress".into()])),
            include_deleted: Some(false),
            limit: Some(50),
        };

        let result = server.forward_to_tcp("team_task_list", &args.into_json()).await;

        accept_task.await.unwrap();
        assert_ne!(result.is_error, Some(true));
        assert_eq!(first_text(&result), "[]");
    }

    #[test]
    fn parse_tool_response_extracts_content_text() {
        let result = parse_tool_response(
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [
                        { "type": "text", "text": "first line" },
                        { "type": "text", "text": "second line" }
                    ],
                    "isError": false
                }
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(result, "first line\nsecond line");
    }

    #[test]
    fn parse_tool_response_sanitizes_top_level_error() {
        let err = parse_tool_response(
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "error": {
                    "code": -32000,
                    "message": "remote failure for conv-secret-123"
                }
            })
            .to_string(),
        )
        .unwrap_err();

        let ToolForwardError::Tool {
            code, upstream_code, ..
        } = err
        else {
            panic!("expected tool error");
        };
        assert_eq!(code, CliBoundaryCode::McpToolRemoteError);
        assert_eq!(upstream_code, Some(json!(-32000)));
    }
}
