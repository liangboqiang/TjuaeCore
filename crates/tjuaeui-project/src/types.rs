use serde::Serialize;
use tjuaeui_common::TimestampMs;
use tjuaeui_db::{DbError, FolderRow, ProjectExplorerRow, ProjectRow};

/// Filesystem operation intent carried into containment resolution. Kept
/// distinct so future access rules (e.g. write-only guards) can branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    Read,
    Write,
    Remove,
    Rename,
    Browse,
}

/// Result of a folder/project resolution: the reused-or-created project, its
/// folder, and the workspace explorer entry binding the two.
#[derive(Debug, Clone)]
pub struct ResolveOutput {
    pub project: ProjectRow,
    pub folder: FolderRow,
    pub project_explorer: ProjectExplorerRow,
}

/// Runtime availability of a folder's resource root, computed at read time
/// (never persisted). `file:` provider yields the first three; `disconnected`
/// is reserved for remote providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Available,
    Missing,
    PermissionDenied,
    Disconnected,
}

impl RuntimeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeStatus::Available => "available",
            RuntimeStatus::Missing => "missing",
            RuntimeStatus::PermissionDenied => "permission_denied",
            RuntimeStatus::Disconnected => "disconnected",
        }
    }
}

/// API-facing folder view. Excludes scheme/authority/path (parsed on demand,
/// not stored); `default_display_name` and `runtime_status` are derived.
#[derive(Debug, Clone, Serialize)]
pub struct FolderDto {
    pub folder_id: String,
    pub resource_uri: String,
    pub resource_canonical: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_display_name: Option<String>,
    pub runtime_status: RuntimeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_error: Option<String>,
}

/// One Project Explorer entry with its joined folder view.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectExplorerEntry {
    pub pe_id: String,
    pub project_id: String,
    pub folder_id: String,
    pub role: String,
    pub display_name: Option<String>,
    pub order_index: i64,
    pub folder: FolderDto,
}

/// Explorer view aggregated onto a project.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectExplorerView {
    pub workspace_pe_id: String,
    pub entries: Vec<ProjectExplorerEntry>,
}

/// Aggregated project detail returned by `get_project`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetail {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub explorer: ProjectExplorerView,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

/// Attach an additional (non-workspace) folder to a project.
#[derive(Debug, Clone)]
pub struct AttachInput {
    pub project_id: String,
    pub uri: String,
    pub display_name: Option<String>,
}

/// Resolve a `pe_id + relative_path` reference to a concrete resource.
#[derive(Debug, Clone)]
pub struct ReferenceInput {
    pub pe_id: String,
    pub relative_path: String,
    pub op: FileOp,
}

/// A reference resolved to a concrete child resource within a folder root.
/// Identity + containment only — no IO is performed to produce it.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedResource {
    pub project_id: String,
    pub pe_id: String,
    pub folder_id: String,
    pub root_resource_uri: String,
    pub root_resource_canonical: String,
    pub relative_path: String,
    pub resource_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absolute_path: Option<String>,
}

/// Creation-binding-chain errors, stable to UI-consumable codes (see
/// `service-contract.md` error table). Each variant carries structured context
/// rather than relying on message parsing.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("找不到文件夹：{path}")]
    FolderNotFound { path: String },

    #[error("路径不是目录：{path}")]
    FolderNotDirectory { path: String },

    #[error("权限不足：{path}")]
    FolderPermissionDenied { path: String },

    #[error("无法规范化资源 URI：{uri}")]
    FolderCanonicalizeFailed { uri: String },

    #[error("临时目录已存在：{path}")]
    TempDirExists { path: String },

    #[error("无法为工作区 {path} 建立 Git：{reason}")]
    GitProvisionFailed { path: String, reason: String },

    #[error("工作区路径缺失，无法补写")]
    WorkspaceMissing,

    #[error("所属文件夹 {folder_id} 与项目 {project_id} 的工作区文件夹不匹配")]
    WorkspaceFolderMismatch { project_id: String, folder_id: String },

    #[error("工作区文件夹 {folder_id} 对应多个标准项目")]
    StandardProjectConflict { folder_id: String },

    #[error("项目 {project_id} 已引用文件夹 {folder_id}")]
    ProjectExplorerDuplicate { project_id: String, folder_id: String },

    #[error("文件夹与项目 {project_id} 中现有的资源管理器条目重叠")]
    ProjectExplorerOverlap { project_id: String },

    #[error("找不到项目：{project_id}")]
    ProjectNotFound { project_id: String },

    #[error("找不到 project_explorer 条目：{pe_id}")]
    ProjectExplorerNotFound { pe_id: String },

    #[error("工作区条目不可修改：{pe_id}")]
    WorkspaceEntryImmutable { pe_id: String },

    #[error("相对路径无效：{relative_path}")]
    InvalidRelativePath { relative_path: String },

    #[error("资源超出文件夹根目录：{relative_path}")]
    ResourceOutsideFolder { relative_path: String },

    #[error("不支持的资源协议：{scheme}")]
    UnsupportedResourceScheme { scheme: String },

    #[error(transparent)]
    Database(#[from] DbError),
}

impl ProjectError {
    /// Stable, UI-consumable error code.
    pub fn code(&self) -> &'static str {
        match self {
            ProjectError::FolderNotFound { .. } => "folder_not_found",
            ProjectError::FolderNotDirectory { .. } => "folder_not_directory",
            ProjectError::FolderPermissionDenied { .. } => "folder_permission_denied",
            ProjectError::FolderCanonicalizeFailed { .. } => "folder_canonicalize_failed",
            ProjectError::TempDirExists { .. } => "temp_dir_exists",
            ProjectError::GitProvisionFailed { .. } => "git_provision_failed",
            ProjectError::WorkspaceMissing => "workspace_missing",
            ProjectError::WorkspaceFolderMismatch { .. } => "workspace_folder_mismatch",
            ProjectError::StandardProjectConflict { .. } => "standard_project_conflict",
            ProjectError::ProjectNotFound { .. } => "project_not_found",
            ProjectError::ProjectExplorerDuplicate { .. } => "project_explorer_duplicate",
            ProjectError::ProjectExplorerOverlap { .. } => "project_explorer_overlap",
            ProjectError::ProjectExplorerNotFound { .. } => "project_explorer_not_found",
            ProjectError::WorkspaceEntryImmutable { .. } => "workspace_entry_immutable",
            ProjectError::InvalidRelativePath { .. } => "invalid_relative_path",
            ProjectError::ResourceOutsideFolder { .. } => "resource_outside_folder",
            ProjectError::UnsupportedResourceScheme { .. } => "unsupported_resource_scheme",
            ProjectError::Database(_) => "internal_db_error",
        }
    }
}
