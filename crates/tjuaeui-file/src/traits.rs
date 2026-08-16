use std::path::Path;
use std::sync::Arc;

use crate::error::FileError;

use crate::types::{
    CopyResult, DirOrFile, FileMetadata, GitCommit, GitCommitFile, GitRepositoryInfo, GitRevision, GitStatus,
    GitWorktree, WorkspaceFlatFile, ZipEntry,
};

/// Core file operations: directory browsing, file read/write, management,
/// image processing, and ZIP packaging.
///
/// All path parameters MUST be validated against the sandbox rules (see
/// `path_safety` module) before reaching this trait's implementations.
#[async_trait::async_trait]
pub trait IFileService: Send + Sync {
    // -- Directory browsing --

    /// List the immediate children of `dir`, returning a tree with one level
    /// of depth. `root` is the workspace root used to compute relative paths.
    async fn get_files_by_dir(&self, dir: &str, root: &str) -> Result<Vec<DirOrFile>, FileError>;

    /// Recursively list all files under `root` as a flat list.
    /// Returns at most 20,000 entries.
    async fn list_workspace_files(&self, root: &str) -> Result<Vec<WorkspaceFlatFile>, FileError>;

    /// Recursively list all files under `root`, allowing one trusted
    /// request-scoped workspace root in addition to the service sandbox.
    async fn list_workspace_files_with_extra_root(
        &self,
        root: &str,
        extra_root: Option<&Path>,
    ) -> Result<Vec<WorkspaceFlatFile>, FileError> {
        let _ = extra_root;
        self.list_workspace_files(root).await
    }

    /// Get metadata for a single file or directory.
    async fn get_file_metadata(&self, path: &str, extra_root: Option<&Path>) -> Result<FileMetadata, FileError>;

    // -- File read/write --

    /// Read a file as UTF-8 text. Returns `None` if the file does not exist.
    /// Files larger than 256 MB are rejected.
    async fn read_file(&self, path: &str, extra_root: Option<&Path>) -> Result<Option<String>, FileError>;

    /// Read a file as raw bytes. Returns `None` if the file does not exist.
    /// Files larger than 256 MB are rejected.
    async fn read_file_buffer(&self, path: &str, extra_root: Option<&Path>) -> Result<Option<Vec<u8>>, FileError>;

    /// Write `data` to `path`. On success, emits a
    /// `fileStream.contentUpdate` event with `operation = write`.
    async fn write_file(&self, path: &str, data: &[u8], workspace: &str) -> Result<bool, FileError>;

    // -- File management --

    /// Copy files into `workspace`, preserving directory structure relative to
    /// `source_root`. Returns lists of copied and failed paths.
    async fn copy_files_to_workspace(
        &self,
        file_paths: &[String],
        workspace: &str,
        source_root: Option<&str>,
    ) -> Result<CopyResult, FileError>;

    /// Remove a file or directory (recursively). On success, emits a
    /// `fileStream.contentUpdate` event with `operation = delete`.
    async fn remove_entry(&self, path: &str, workspace: &str) -> Result<(), FileError>;

    /// Rename a file or directory. Returns the new absolute path.
    async fn rename_entry(&self, path: &str, new_name: &str) -> Result<String, FileError>;

    /// Rename a file or directory, allowing one request-scoped root in
    /// addition to the service sandbox.
    async fn rename_entry_with_extra_root(
        &self,
        path: &str,
        new_name: &str,
        extra_root: Option<&Path>,
    ) -> Result<String, FileError> {
        let _ = extra_root;
        self.rename_entry(path, new_name).await
    }

    /// Create an empty temporary file and return its absolute path.
    async fn create_temp_file(&self, file_name: &str) -> Result<String, FileError>;

    /// Write `data` to a temporary file and return its absolute path.
    ///
    /// When `conversation_id` is provided, the file is placed under a
    /// per-conversation sub-directory (`<tmp>/tjuaeui/<conversation_id>/`);
    /// otherwise the shared `<tmp>/tjuaeui/` directory is used (same as
    /// [`create_temp_file`](Self::create_temp_file)).
    ///
    /// `file_name` must not contain path separators or traversal patterns.
    async fn create_upload_file(
        &self,
        file_name: &str,
        data: &[u8],
        conversation_id: Option<&str>,
    ) -> Result<String, FileError>;

    // -- Image processing --

    /// Read a local image and return a base64 Data URL
    /// (e.g. `data:image/png;base64,...`).
    async fn get_image_base64(&self, path: &str, extra_root: Option<&Path>) -> Result<String, FileError>;

    /// Download a remote image and return a base64 Data URL.
    /// On failure, returns a placeholder SVG Data URL.
    async fn fetch_remote_image(&self, url: &str) -> String;

    // -- ZIP --

    /// Create a ZIP archive at `path` from `entries`.
    /// If `request_id` is provided, the operation can be cancelled via
    /// [`cancel_zip`](Self::cancel_zip).
    async fn create_zip(
        &self,
        path: &str,
        entries: Vec<ZipEntry>,
        request_id: Option<String>,
    ) -> Result<bool, FileError>;

    /// Create a ZIP archive, allowing request-scoped roots for the output path
    /// and disk source entries in addition to the service sandbox.
    async fn create_zip_with_extra_roots(
        &self,
        path: &str,
        entries: Vec<ZipEntry>,
        request_id: Option<String>,
        output_root: Option<&Path>,
        source_root: Option<&Path>,
    ) -> Result<bool, FileError> {
        let _ = (output_root, source_root);
        self.create_zip(path, entries, request_id).await
    }

    /// Cancel an in-progress ZIP operation by its `request_id`.
    /// Returns `true` if a matching operation was found and cancelled.
    async fn cancel_zip(&self, request_id: &str) -> bool;
}

/// File system watching: single-file changes and workspace Office file
/// additions.
#[async_trait::async_trait]
pub trait IFileWatchService: Send + Sync {
    /// Start watching a single file for changes.
    /// Emits `fileWatch.fileChanged` events on the broadcast channel.
    async fn start_watch(&self, file_path: &str) -> Result<(), FileError>;

    /// Stop watching a previously registered file.
    async fn stop_watch(&self, file_path: &str) -> Result<(), FileError>;

    /// Stop all active file watches.
    async fn stop_all_watches(&self) -> Result<(), FileError>;

    /// Start watching a workspace directory for new Office files
    /// (.pptx, .docx, .xlsx).
    /// Emits `workspaceOfficeWatch.fileAdded` events.
    async fn start_office_watch(&self, workspace: &str) -> Result<(), FileError>;

    /// Stop watching a workspace directory for Office files.
    async fn stop_office_watch(&self, workspace: &str) -> Result<(), FileError>;
}

/// Persistent Git authority for every Tjuae workspace.
#[async_trait::async_trait]
pub trait IGitService: Send + Sync {
    async fn ensure(&self, workspace: &str) -> Result<GitRepositoryInfo, FileError>;
    async fn repository_info(&self, workspace: &str) -> Result<GitRepositoryInfo, FileError>;
    async fn status(&self, workspace: &str) -> Result<GitStatus, FileError>;
    async fn baseline_content(&self, workspace: &str, file_path: &str) -> Result<Option<String>, FileError>;
    async fn index_content(&self, workspace: &str, file_path: &str) -> Result<Option<String>, FileError>;
    async fn stage_file(&self, workspace: &str, file_path: &str) -> Result<(), FileError>;
    async fn stage_all(&self, workspace: &str) -> Result<(), FileError>;
    async fn unstage_file(&self, workspace: &str, file_path: &str) -> Result<(), FileError>;
    async fn unstage_all(&self, workspace: &str) -> Result<(), FileError>;
    async fn discard_file(&self, workspace: &str, file_path: &str) -> Result<(), FileError>;
    async fn history(
        &self,
        workspace: &str,
        file_path: Option<&str>,
        reference: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GitCommit>, FileError>;
    async fn commit_files(&self, workspace: &str, revision: &str) -> Result<Vec<GitCommitFile>, FileError>;
    async fn revision(&self, workspace: &str, file_path: &str, revision: &str) -> Result<GitRevision, FileError>;
    async fn create_branch(&self, workspace: &str, name: &str, start_point: Option<&str>) -> Result<(), FileError>;
    async fn switch_branch(&self, workspace: &str, name: &str) -> Result<(), FileError>;
    async fn checkout_revision(&self, workspace: &str, revision: &str) -> Result<(), FileError>;
    async fn clone_repository(
        &self,
        repository_url: &str,
        parent_directory: &str,
    ) -> Result<GitRepositoryInfo, FileError>;
    async fn commit(&self, workspace: &str, message: &str, include_unstaged: bool) -> Result<String, FileError>;
    async fn fetch(&self, workspace: &str) -> Result<(), FileError>;
    async fn pull(&self, workspace: &str) -> Result<(), FileError>;
    async fn push(&self, workspace: &str) -> Result<(), FileError>;
    async fn sync(&self, workspace: &str) -> Result<(), FileError>;
    async fn create_worktree(
        &self,
        workspace: &str,
        path: &str,
        branch: &str,
        start_point: Option<&str>,
    ) -> Result<GitWorktree, FileError>;
    async fn remove_worktree(&self, workspace: &str, path: &str) -> Result<(), FileError>;
}

/// Convenience alias for an Arc-wrapped file service.
pub type FileServiceRef = Arc<dyn IFileService>;

/// Convenience alias for an Arc-wrapped file watch service.
pub type FileWatchServiceRef = Arc<dyn IFileWatchService>;

/// Convenience alias for an Arc-wrapped Git service.
pub type GitServiceRef = Arc<dyn IGitService>;
