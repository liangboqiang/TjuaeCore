use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// A. Core file operations — Request DTOs
// ---------------------------------------------------------------------------

/// Request body for `POST /api/fs/dir` — get files by directory.
#[derive(Debug, Deserialize)]
pub struct GetFilesByDirRequest {
    pub dir: String,
    pub root: String,
}

/// Request body for `POST /api/fs/list` — list workspace files.
#[derive(Debug, Deserialize)]
pub struct ListWorkspaceFilesRequest {
    pub root: String,
}

/// Request body for `POST /api/fs/metadata` — get file metadata.
#[derive(Debug, Deserialize)]
pub struct GetFileMetadataRequest {
    pub path: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Request body for `POST /api/fs/read` — read file.
#[derive(Debug, Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Request body for `POST /api/fs/read-buffer` — read file as binary.
#[derive(Debug, Deserialize)]
pub struct ReadFileBufferRequest {
    pub path: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Request body for `POST /api/fs/write` — write file.
#[derive(Debug, Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub data: String,
    /// Workspace root, used to compute `relativePath` in the
    /// `fileStream.contentUpdate` event.  Falls back to the file's
    /// parent directory when absent.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Request body for `POST /api/fs/copy` — copy files to workspace.
#[derive(Debug, Deserialize)]
pub struct CopyFilesRequest {
    pub file_paths: Vec<String>,
    pub workspace: String,
    #[serde(default)]
    pub source_root: Option<String>,
}

/// Request body for `POST /api/fs/remove` — remove file or directory.
#[derive(Debug, Deserialize)]
pub struct RemoveEntryRequest {
    pub path: String,
    /// Workspace root, used to compute `relativePath` in the
    /// `fileStream.contentUpdate` event.  Falls back to the file's
    /// parent directory when absent.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Request body for `POST /api/fs/rename` — rename file or directory.
#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    pub path: String,
    pub new_name: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Request body for `POST /api/fs/temp` — create temp file.
#[derive(Debug, Deserialize)]
pub struct CreateTempFileRequest {
    pub file_name: String,
}

/// Request body for `POST /api/fs/image-base64` — get image as base64.
#[derive(Debug, Deserialize)]
pub struct GetImageBase64Request {
    pub path: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Request body for `POST /api/fs/fetch-remote-image` — fetch remote image.
#[derive(Debug, Deserialize)]
pub struct FetchRemoteImageRequest {
    pub url: String,
}

/// A single entry in a ZIP creation request.
#[derive(Debug, Clone, Deserialize)]
pub struct ZipFileEntry {
    pub name: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
}

/// Request body for `POST /api/fs/zip` — create ZIP archive.
#[derive(Debug, Deserialize)]
pub struct ZipRequest {
    pub path: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub source_root: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    pub files: Vec<ZipFileEntry>,
}

/// Request body for `POST /api/fs/zip/cancel` — cancel ZIP creation.
#[derive(Debug, Deserialize)]
pub struct CancelZipRequest {
    pub request_id: String,
}

/// Query parameters for `GET /api/fs/browse` — shallow directory browser.
///
/// Unlike `/api/fs/dir` (which returns a recursive tree scoped to a workspace
/// root), `browse` is a WebUI-only host-file picker: it lists a single
/// directory level, surfaces navigation hints (`can_go_up`, `parent_path`),
/// and on Windows supports a `__ROOT__` sentinel for the drive-list screen.
#[derive(Debug, Deserialize)]
pub struct BrowseDirectoryQuery {
    /// Directory to list. Empty string means "use default" (Windows: drive
    /// list; Unix: current working directory). `"__ROOT__"` on Windows is
    /// treated the same as an empty path.
    #[serde(default)]
    pub path: Option<String>,
    /// When true, include regular files in the response. Defaults to false
    /// (directories only).
    #[serde(default)]
    pub show_files: Option<String>,
}

/// A single entry in a `/api/fs/browse` response.
///
/// Uses camelCase on the wire to match the original Express contract the
/// frontend `DirectorySelectionModal` still consumes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub is_file: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Last-modified time as milliseconds since the unix epoch. Absent when
    /// the entry has no readable metadata (e.g. a Windows drive stub).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<i64>,
}

/// Response body for `GET /api/fs/browse`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowseDirectoryResponse {
    /// The resolved directory currently being listed. Empty string when the
    /// response is a Windows drive-list screen.
    pub current_path: String,
    /// Path to navigate to when the user clicks "up". `None` when already at
    /// the root. Value `"__ROOT__"` is a sentinel used on Windows to mean
    /// "return to the drive-list screen".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_path: Option<String>,
    pub items: Vec<BrowseEntry>,
    pub can_go_up: bool,
    pub truncated: bool,
    /// True when the response represents the Windows drive-list screen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_root: Option<bool>,
}

// ---------------------------------------------------------------------------
// A. Core file operations — Response DTOs
// ---------------------------------------------------------------------------

/// A node in the directory tree returned by `getFilesByDir`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirOrFileResponse {
    pub name: String,
    pub full_path: String,
    pub relative_path: String,
    pub is_dir: bool,
    pub is_file: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<DirOrFileResponse>>,
}

/// A flat file entry returned by `listWorkspaceFiles`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceFlatFileResponse {
    pub name: String,
    pub full_path: String,
    pub relative_path: String,
}

/// File metadata returned by `getFileMetadata`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadataResponse {
    pub name: String,
    pub path: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub last_modified: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_directory: Option<bool>,
}

/// Result of a batch copy operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyFilesResponse {
    pub copied_files: Vec<String>,
    pub failed_files: Vec<String>,
}

/// Result of a rename operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameResponse {
    pub new_path: String,
}

// ---------------------------------------------------------------------------
// D. File watch — Request DTOs
// ---------------------------------------------------------------------------

/// Request body for `POST /api/fs/watch/start` and `/stop`.
#[derive(Debug, Deserialize)]
pub struct FileWatchRequest {
    pub file_path: String,
}

/// Request body for `POST /api/fs/office-watch/start` and `/stop`.
#[derive(Debug, Deserialize)]
pub struct WorkspaceOfficeWatchRequest {
    pub workspace: String,
}

// ---------------------------------------------------------------------------
// E. Workspace Git — Request DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GitWorkspaceRequest {
    pub workspace: String,
}

#[derive(Debug, Deserialize)]
pub struct GitFileRequest {
    pub workspace: String,
    pub file_path: String,
}

#[derive(Debug, Deserialize)]
pub struct GitHistoryRequest {
    pub workspace: String,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default = "default_git_history_limit")]
    pub limit: usize,
}

fn default_git_history_limit() -> usize {
    100
}

#[derive(Debug, Deserialize)]
pub struct GitRevisionRequest {
    pub workspace: String,
    pub file_path: String,
    pub revision: String,
}

#[derive(Debug, Deserialize)]
pub struct GitBranchCreateRequest {
    pub workspace: String,
    pub name: String,
    #[serde(default)]
    pub start_point: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitBranchSwitchRequest {
    pub workspace: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct GitRevisionCheckoutRequest {
    pub workspace: String,
    pub revision: String,
}

#[derive(Debug, Deserialize)]
pub struct GitCommitFilesRequest {
    pub workspace: String,
    pub revision: String,
}

#[derive(Debug, Deserialize)]
pub struct GitCloneRequest {
    pub repository_url: String,
    pub parent_directory: String,
}

#[derive(Debug, Deserialize)]
pub struct GitCommitRequest {
    pub workspace: String,
    pub message: String,
    #[serde(default)]
    pub include_unstaged: bool,
}

#[derive(Debug, Deserialize)]
pub struct GitWorktreeCreateRequest {
    pub workspace: String,
    pub path: String,
    pub branch: String,
    #[serde(default)]
    pub start_point: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitWorktreeRemoveRequest {
    pub workspace: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// E. Workspace Git — Response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitFileStatusResponse {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitFileChangeResponse {
    pub file_path: String,
    pub relative_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_relative_path: Option<String>,
    pub status: GitFileStatusResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatusResponse {
    pub conflicted: Vec<GitFileChangeResponse>,
    pub staged: Vec<GitFileChangeResponse>,
    pub unstaged: Vec<GitFileChangeResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBranchResponse {
    pub name: String,
    pub current: bool,
    pub checked_out: bool,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitWorktreeResponse {
    pub path: String,
    pub branch: Option<String>,
    pub head: String,
    pub current: bool,
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepositoryResponse {
    pub repository_root: String,
    pub workspace_path: String,
    pub workspace_relative_path: String,
    pub branch: String,
    pub head_commit: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
    pub branches: Vec<GitBranchResponse>,
    pub worktrees: Vec<GitWorktreeResponse>,
    pub remotes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitResponse {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    pub decorations: Vec<String>,
    pub author: String,
    pub authored_at: i64,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitFileResponse {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: GitFileStatusResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRevisionResponse {
    pub revision: String,
    pub file_path: String,
    pub original_revision: Option<String>,
    pub original_content: Option<String>,
    pub modified_content: Option<String>,
    pub patch: String,
    pub binary: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- Request deserialization tests --

    #[test]
    fn get_files_by_dir_request_deserialization() {
        let raw = r#"{"dir":"/home/user/project","root":"/home/user"}"#;
        let req: GetFilesByDirRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.dir, "/home/user/project");
        assert_eq!(req.root, "/home/user");
    }

    #[test]
    fn list_workspace_files_request_requires_root() {
        let req: ListWorkspaceFilesRequest = serde_json::from_str(r#"{"root":"/workspace"}"#).unwrap();
        assert_eq!(req.root, "/workspace");

        let missing_root = serde_json::from_str::<ListWorkspaceFilesRequest>(r#"{}"#);
        assert!(missing_root.is_err());
    }

    #[test]
    fn copy_files_request_snake_case() {
        let raw = json!({
            "file_paths": ["/a.txt", "/b.txt"],
            "workspace": "/ws",
            "source_root": "/src"
        });
        let req: CopyFilesRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.file_paths, vec!["/a.txt", "/b.txt"]);
        assert_eq!(req.workspace, "/ws");
        assert_eq!(req.source_root.as_deref(), Some("/src"));
    }

    #[test]
    fn copy_files_request_optional_source_root() {
        let raw = json!({
            "file_paths": ["/a.txt"],
            "workspace": "/ws"
        });
        let req: CopyFilesRequest = serde_json::from_value(raw).unwrap();
        assert!(req.source_root.is_none());
    }

    #[test]
    fn rename_request_snake_case() {
        let raw = r#"{"path":"/ws/old.txt","new_name":"new.txt","workspace":"/ws"}"#;
        let req: RenameRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.path, "/ws/old.txt");
        assert_eq!(req.new_name, "new.txt");
        assert_eq!(req.workspace.as_deref(), Some("/ws"));
    }

    #[test]
    fn zip_request_snake_case() {
        let raw = json!({
            "path": "/out.zip",
            "workspace": "/out",
            "source_root": "/src",
            "request_id": "req-1",
            "files": [
                { "name": "a.txt", "content": "hello" },
                { "name": "b.bin", "file_path": "/src/b.bin" }
            ]
        });
        let req: ZipRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.path, "/out.zip");
        assert_eq!(req.workspace.as_deref(), Some("/out"));
        assert_eq!(req.source_root.as_deref(), Some("/src"));
        assert_eq!(req.request_id.as_deref(), Some("req-1"));
        assert_eq!(req.files.len(), 2);
        assert_eq!(req.files[0].content.as_deref(), Some("hello"));
        assert!(req.files[0].file_path.is_none());
        assert!(req.files[1].content.is_none());
        assert_eq!(req.files[1].file_path.as_deref(), Some("/src/b.bin"));
    }

    #[test]
    fn zip_request_optional_request_id() {
        let raw = json!({
            "path": "/out.zip",
            "files": [{ "name": "a.txt", "content": "x" }]
        });
        let req: ZipRequest = serde_json::from_value(raw).unwrap();
        assert!(req.request_id.is_none());
    }

    #[test]
    fn file_watch_request_snake_case() {
        let raw = r#"{"file_path":"/path/to/file.txt"}"#;
        let req: FileWatchRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.file_path, "/path/to/file.txt");
    }

    #[test]
    fn git_revision_request_deserialization() {
        let raw = json!({
            "workspace": "/ws",
            "file_path": "src/main.rs",
            "revision": "abc123"
        });
        let req: GitRevisionRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.workspace, "/ws");
        assert_eq!(req.file_path, "src/main.rs");
        assert_eq!(req.revision, "abc123");
    }

    // -- Response serialization tests --

    #[test]
    fn dir_or_file_response_serialization() {
        let resp = DirOrFileResponse {
            name: "src".into(),
            full_path: "/project/src".into(),
            relative_path: "src".into(),
            is_dir: true,
            is_file: false,
            children: Some(vec![DirOrFileResponse {
                name: "main.rs".into(),
                full_path: "/project/src/main.rs".into(),
                relative_path: "src/main.rs".into(),
                is_dir: false,
                is_file: true,
                children: None,
            }]),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["name"], "src");
        assert_eq!(json["full_path"], "/project/src");
        assert_eq!(json["relative_path"], "src");
        assert_eq!(json["is_dir"], true);
        assert_eq!(json["is_file"], false);
        assert_eq!(json["children"][0]["name"], "main.rs");
    }

    #[test]
    fn dir_or_file_response_no_children_omitted() {
        let resp = DirOrFileResponse {
            name: "file.txt".into(),
            full_path: "/file.txt".into(),
            relative_path: "file.txt".into(),
            is_dir: false,
            is_file: true,
            children: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("children").is_none());
    }

    #[test]
    fn workspace_flat_file_response_serialization() {
        let resp = WorkspaceFlatFileResponse {
            name: "lib.rs".into(),
            full_path: "/project/src/lib.rs".into(),
            relative_path: "src/lib.rs".into(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["name"], "lib.rs");
        assert_eq!(json["full_path"], "/project/src/lib.rs");
        assert_eq!(json["relative_path"], "src/lib.rs");
    }

    #[test]
    fn file_metadata_response_serialization() {
        let resp = FileMetadataResponse {
            name: "readme.md".into(),
            path: "/project/readme.md".into(),
            size: 1024,
            mime_type: "text/markdown".into(),
            last_modified: 1700000000000,
            is_directory: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["name"], "readme.md");
        assert_eq!(json["path"], "/project/readme.md");
        assert_eq!(json["size"], 1024);
        assert_eq!(json["type"], "text/markdown");
        assert_eq!(json["last_modified"], 1700000000000_i64);
        assert!(json.get("is_directory").is_none());
    }

    #[test]
    fn file_metadata_response_with_directory_flag() {
        let resp = FileMetadataResponse {
            name: "src".into(),
            path: "/project/src".into(),
            size: 0,
            mime_type: "".into(),
            last_modified: 1700000000000,
            is_directory: Some(true),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["is_directory"], true);
    }

    #[test]
    fn copy_files_response_serialization() {
        let resp = CopyFilesResponse {
            copied_files: vec!["/ws/a.txt".into()],
            failed_files: vec!["/missing.txt".into()],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["copied_files"][0], "/ws/a.txt");
        assert_eq!(json["failed_files"][0], "/missing.txt");
    }

    #[test]
    fn git_status_response_serialization() {
        let resp = GitStatusResponse {
            conflicted: vec![],
            staged: vec![GitFileChangeResponse {
                file_path: "/ws/a.txt".into(),
                relative_path: "a.txt".into(),
                old_relative_path: None,
                status: GitFileStatusResponse::Added,
            }],
            unstaged: vec![],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["staged"][0]["status"], "added");
    }
}
