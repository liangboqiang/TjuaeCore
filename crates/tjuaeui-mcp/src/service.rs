use std::sync::Arc;

use tjuaeui_api_types::McpServerResponse;
use tjuaeui_db::IMcpServerRepository;

use crate::error::McpError;
use crate::types::McpServer;

// ---------------------------------------------------------------------------
// McpConfigService
// ---------------------------------------------------------------------------

/// MCP 运行投影的只读查询服务。
///
/// Definition、Overlay、试跑与启停统一由类型化 Core 资产生命周期负责。
#[derive(Clone)]
pub struct McpConfigService {
    repo: Arc<dyn IMcpServerRepository>,
}

impl McpConfigService {
    pub fn new(repo: Arc<dyn IMcpServerRepository>) -> Self {
        Self { repo }
    }

    /// List all MCP servers.
    pub async fn list_servers(&self) -> Result<Vec<McpServerResponse>, McpError> {
        let rows = self.repo.list().await?;
        rows.into_iter()
            .map(|row| McpServer::from_row(row).map(McpServer::into_response))
            .collect()
    }

    /// Get a single MCP server by ID.
    pub async fn get_server(&self, id: &str) -> Result<McpServerResponse, McpError> {
        let row = self
            .repo
            .find_by_id(id)
            .await?
            .ok_or_else(|| McpError::NotFound(id.to_owned()))?;
        let server = McpServer::from_row(row)?;
        Ok(server.into_response())
    }
}
