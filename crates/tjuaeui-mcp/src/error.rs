/// MCP crate-level errors.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("找不到 MCP 服务：{0}")]
    NotFound(String),

    #[error("MCP 服务名称冲突：{0}")]
    Conflict(String),

    #[error("MCP 服务修改无效：{0}")]
    InvalidEdit(String),

    #[error("传输配置无效：{0}")]
    InvalidTransport(String),

    #[error("智能体 CLI 未安装：{0}")]
    AgentNotInstalled(String),

    #[error("智能体操作失败：{0}")]
    AgentOperationFailed(String),

    #[error("连接测试失败：{0}")]
    ConnectionFailed(String),

    #[error("OAuth 错误：{0}")]
    OAuth(String),

    #[error("{0}")]
    Database(#[from] tjuaeui_db::DbError),

    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_messages() {
        assert_eq!(McpError::NotFound("mcp_1".into()).to_string(), "找不到 MCP 服务：mcp_1");
        assert_eq!(
            McpError::InvalidTransport("bad".into()).to_string(),
            "传输配置无效：bad"
        );
        assert_eq!(
            McpError::InvalidEdit("rename forbidden".into()).to_string(),
            "MCP 服务修改无效：rename forbidden"
        );
    }
}
