use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of making a workspace Git-backed. This is a deliberately small
/// cross-domain contract: project and conversation services only need to know
/// that persistence exists and which branch is active; repository operations
/// remain owned by `tjuaeui-file`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitProvision {
    pub repository_root: String,
    pub workspace_path: String,
    pub branch: String,
    pub head_commit: String,
}

/// Neutral capability port used by all workspace-producing domains.
///
/// Implementations must preserve an existing repository and its history. A
/// non-repository workspace is initialized on `main` and receives one initial
/// commit so every Tjuae workspace starts clean.
#[async_trait]
pub trait WorkspaceGitProvisioner: Send + Sync {
    async fn ensure_workspace_git(&self, workspace: &Path) -> Result<WorkspaceGitProvision, String>;
}
