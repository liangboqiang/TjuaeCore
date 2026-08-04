#![warn(clippy::disallowed_types)]

//! MCP 运行投影查询、多智能体同步适配与连接试跑。
pub mod adapter;
pub mod adapters;
pub mod connection_test;
pub mod error;
pub mod routes;
pub mod service;
pub mod session_injection;
pub mod sync_service;
pub mod types;

pub use adapter::{DetectedServer, McpAgentAdapter};
pub use adapters::{
    ClaudeAdapter, CodeBuddyAdapter, CodexAdapter, GeminiAdapter, OpencodeAdapter, QwenAdapter, TjuaeCliAdapter,
    TjuaeUIAdapter,
};
pub use connection_test::McpConnectionTestService;
pub use error::McpError;
pub use routes::{McpRouterState, mcp_routes};
pub use service::McpConfigService;
pub use session_injection::{
    AcpMcpCapabilities, AcpSessionMcpServer, ImageGenConfig, NameValuePair, build_builtin_image_gen_server,
    build_session_mcp_servers, parse_acp_mcp_capabilities,
};
pub use sync_service::McpSyncService;
pub use types::{McpServer, McpServerTransport, McpTool};
