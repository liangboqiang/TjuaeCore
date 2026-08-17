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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspacePathPublishResult {
    pub branch: String,
    pub commit: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceGitState {
    Clean,
    Modified,
    Conflicted,
    Unknown,
}

/// Neutral capability port used by all workspace-producing domains.
///
/// Implementations must preserve an existing repository and its history. A
/// non-repository workspace is initialized on `main` and receives one initial
/// commit so every Tjuae workspace starts clean.
#[async_trait]
pub trait WorkspaceGitProvisioner: Send + Sync {
    async fn ensure_workspace_git(&self, workspace: &Path) -> Result<WorkspaceGitProvision, String>;

    /// Compact state for workspace cards. Detailed status and operations stay
    /// in the shared Git service used by the Workbench.
    async fn workspace_git_state(&self, _workspace: &Path) -> Result<WorkspaceGitState, String> {
        Ok(WorkspaceGitState::Unknown)
    }

    /// Commit the complete managed workspace after an application-owned
    /// installation or update. Interactive Git operations remain in the file
    /// service; this narrow hook keeps domain installers on the same Git
    /// authority without spawning their own commands.
    async fn commit_workspace_snapshot(&self, _workspace: &Path, _message: &str) -> Result<String, String> {
        Err("当前 Git 实现不支持托管快照提交".to_owned())
    }

    /// 将一个技能仓库克隆到托管目录，并返回克隆后的工作区信息。
    async fn clone_workspace_repository(
        &self,
        _repository_url: &str,
        _parent_directory: &Path,
    ) -> Result<WorkspaceGitProvision, String> {
        Err("当前 Git 实现不支持克隆工作区".to_owned())
    }

    /// Materialize one directory from a repository at an immutable revision.
    /// The destination contains source files only; the transport checkout is
    /// removed so the caller can initialize an independent local workspace.
    async fn materialize_repository_path(
        &self,
        _repository_url: &str,
        _revision: &str,
        _source_path: &str,
        _destination: &Path,
    ) -> Result<(), String> {
        Err("当前 Git 实现不支持读取市场工作区".to_owned())
    }

    /// Return true only when the worktree is clean and HEAD still points to
    /// the last market synchronization baseline.
    async fn workspace_matches_market_baseline(&self, _workspace: &Path) -> Result<bool, String> {
        Err("当前 Git 实现不支持市场基线检查".to_owned())
    }

    /// Move the private market baseline ref to the current clean HEAD.
    async fn mark_market_baseline(&self, _workspace: &Path) -> Result<(), String> {
        Err("当前 Git 实现不支持记录市场基线".to_owned())
    }

    /// Publish one local workspace into a directory of another Git repository.
    /// The implementation owns all Git execution and must use typed arguments;
    /// callers only provide already-validated domain values.
    async fn publish_workspace_path(
        &self,
        _workspace: &Path,
        _target_repository_url: &str,
        _target_path: &str,
        _branch: &str,
        _message: &str,
    ) -> Result<WorkspacePathPublishResult, String> {
        Err("当前 Git 实现不支持发布工作区".to_owned())
    }
}
