#![allow(clippy::disallowed_types)]

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Json, Multipart, Query, State};
use axum::routing::{get, post};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tower_http::limit::RequestBodyLimitLayer;

use tjuaeui_api_types::{
    ApiResponse, BrowseDirectoryQuery, BrowseDirectoryResponse, CancelZipRequest, CopyFilesRequest, CopyFilesResponse,
    CreateTempFileRequest, DirOrFileResponse, FetchRemoteImageRequest, FileMetadataResponse, FileWatchRequest,
    GetFileMetadataRequest, GetFilesByDirRequest, GetImageBase64Request, GitBranchCreateRequest, GitBranchResponse,
    GitBranchSwitchRequest, GitCloneRequest, GitCommitFileResponse, GitCommitFilesRequest, GitCommitRequest,
    GitCommitResponse, GitFileChangeResponse, GitFileRequest, GitFileStatusResponse, GitHistoryRequest,
    GitRepositoryResponse, GitRevisionCheckoutRequest, GitRevisionRequest, GitRevisionResponse, GitStatusResponse,
    GitWorkspaceRequest, GitWorktreeCreateRequest, GitWorktreeRemoveRequest, GitWorktreeResponse,
    ListWorkspaceFilesRequest, ReadFileBufferRequest, ReadFileRequest, RemoveEntryRequest, RenameRequest,
    RenameResponse, WorkspaceFlatFileResponse, WorkspaceOfficeWatchRequest, WriteFileRequest, ZipRequest,
};
use tjuaeui_common::ApiError;
use tjuaeui_common::constants::UPLOAD_MAX_SIZE;

use crate::browse;
use crate::error::FileError;
use crate::traits::{FileServiceRef, FileWatchServiceRef, GitServiceRef};
use crate::types::{
    CopyResult, DirOrFile, FileMetadata, GitBranch, GitCommit, GitCommitFile, GitFileChange, GitFileStatus,
    GitRepositoryInfo, GitRevision, GitStatus, GitWorktree, WorkspaceFlatFile, ZipEntry,
};

impl From<FileError> for ApiError {
    fn from(error: FileError) -> Self {
        match error {
            FileError::BadRequest(message) => ApiError::BadRequest(message),
            FileError::Forbidden(message) => ApiError::Forbidden(message),
            FileError::PathOutsideSandbox {
                message,
                field,
                operation,
            } => ApiError::PathOutsideSandbox {
                message,
                field,
                operation,
            },
            FileError::NotFound(message) => ApiError::NotFound(message),
            FileError::Internal(message) => ApiError::Internal(message),
        }
    }
}

// ---------------------------------------------------------------------------
// Router state
// ---------------------------------------------------------------------------

type BrowseRootsResolver = dyn Fn() -> Vec<PathBuf> + Send + Sync;

/// Lazily resolves roots for the shallow `/api/fs/browse` endpoint.
#[derive(Clone)]
pub struct BrowseRoots {
    roots: Arc<OnceLock<Vec<PathBuf>>>,
    resolver: Arc<BrowseRootsResolver>,
}

impl BrowseRoots {
    pub fn new() -> Self {
        Self {
            roots: Arc::new(OnceLock::new()),
            resolver: Arc::new(browse::default_browse_roots),
        }
    }

    #[cfg(test)]
    fn with_resolver(resolver: impl Fn() -> Vec<PathBuf> + Send + Sync + 'static) -> Self {
        Self {
            roots: Arc::new(OnceLock::new()),
            resolver: Arc::new(resolver),
        }
    }

    fn get(&self) -> Vec<PathBuf> {
        self.roots.get_or_init(|| (self.resolver)()).clone()
    }
}

impl Default for BrowseRoots {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared state for all file-related route handlers.
#[derive(Clone)]
pub struct FileRouterState {
    pub file_service: FileServiceRef,
    pub watch_service: FileWatchServiceRef,
    pub git_service: GitServiceRef,
    pub allowed_roots: Vec<std::path::PathBuf>,
    /// Roots permitted by the shallow `/api/fs/browse` endpoint. This is
    /// typically wider than `allowed_roots` (it includes `cwd`, Windows
    /// drive letters, and `/` on Unix) because the WebUI host-file picker
    /// legitimately needs to reach outside any single workspace.
    pub browse_roots: BrowseRoots,
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

/// Build the file router with all `/api/fs/*` routes.
///
/// All routes require authentication (applied by the caller).
pub fn file_routes(state: FileRouterState) -> Router {
    // Upload route carries its own body-size limit (UPLOAD_MAX_SIZE, 30 MB).
    // We first disable the global `DefaultBodyLimit` that `tjuaeui-app`
    // installs (otherwise the `Multipart` extractor would cap the body at
    // `BODY_LIMIT`), then apply `RequestBodyLimitLayer` as the sole hard
    // cap. The layers are added in outer->inner order via `.layer()`.
    let upload_router = Router::new()
        .route("/api/fs/upload", post(upload_file))
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(UPLOAD_MAX_SIZE))
        .with_state(state.clone());

    Router::new()
        // A. Core file operations
        .route("/api/fs/browse", get(browse_directory))
        .route("/api/fs/dir", post(get_files_by_dir))
        .route("/api/fs/list", post(list_workspace_files))
        .route("/api/fs/metadata", post(get_file_metadata))
        .route("/api/fs/read", post(read_file))
        .route("/api/fs/read-buffer", post(read_file_buffer))
        .route("/api/fs/write", post(write_file))
        .route("/api/fs/copy", post(copy_files))
        .route("/api/fs/remove", post(remove_entry))
        .route("/api/fs/rename", post(rename_entry))
        .route("/api/fs/temp", post(create_temp_file))
        .route("/api/fs/image-base64", post(get_image_base64))
        .route("/api/fs/fetch-remote-image", post(fetch_remote_image))
        .route("/api/fs/zip", post(create_zip))
        .route("/api/fs/zip/cancel", post(cancel_zip))
        // D. File watch
        .route("/api/fs/watch/start", post(start_watch))
        .route("/api/fs/watch/stop", post(stop_watch))
        .route("/api/fs/watch/stop-all", post(stop_all_watches))
        .route("/api/fs/office-watch/start", post(start_office_watch))
        .route("/api/fs/office-watch/stop", post(stop_office_watch))
        // E. Persistent workspace Git
        .route("/api/fs/git/ensure", post(git_ensure))
        .route("/api/fs/git/info", post(git_info))
        .route("/api/fs/git/status", post(git_status))
        .route("/api/fs/git/baseline", post(git_baseline))
        .route("/api/fs/git/index-content", post(git_index_content))
        .route("/api/fs/git/stage", post(git_stage_file))
        .route("/api/fs/git/stage-all", post(git_stage_all))
        .route("/api/fs/git/unstage", post(git_unstage_file))
        .route("/api/fs/git/unstage-all", post(git_unstage_all))
        .route("/api/fs/git/discard", post(git_discard))
        .route("/api/fs/git/history", post(git_history))
        .route("/api/fs/git/commit-files", post(git_commit_files))
        .route("/api/fs/git/revision", post(git_revision))
        .route("/api/fs/git/branch/create", post(git_create_branch))
        .route("/api/fs/git/branch/switch", post(git_switch_branch))
        .route("/api/fs/git/revision/checkout", post(git_checkout_revision))
        .route("/api/fs/git/clone", post(git_clone))
        .route("/api/fs/git/commit", post(git_commit))
        .route("/api/fs/git/fetch", post(git_fetch))
        .route("/api/fs/git/pull", post(git_pull))
        .route("/api/fs/git/push", post(git_push))
        .route("/api/fs/git/sync", post(git_sync))
        .route("/api/fs/git/worktree/create", post(git_create_worktree))
        .route("/api/fs/git/worktree/remove", post(git_remove_worktree))
        .with_state(state)
        .merge(upload_router)
}

// ---------------------------------------------------------------------------
// A. Core file operations — handlers
// ---------------------------------------------------------------------------

/// `GET /api/fs/browse` — shallow directory listing for the WebUI host-file
/// picker. Runs on the Tokio blocking pool because it does synchronous
/// filesystem I/O.
async fn browse_directory(
    State(state): State<FileRouterState>,
    Query(query): Query<BrowseDirectoryQuery>,
) -> Result<Json<ApiResponse<BrowseDirectoryResponse>>, ApiError> {
    let show_files = matches!(query.show_files.as_deref(), Some("true") | Some("1"));
    let raw_path = query.path.clone();
    let browse_roots = state.browse_roots.clone();

    let response = tokio::task::spawn_blocking(move || {
        let roots = browse_roots.get();
        browse::browse(raw_path.as_deref(), show_files, &roots)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("浏览任务失败：{}", e)))??;

    Ok(Json(ApiResponse::ok(response)))
}

async fn get_files_by_dir(
    State(state): State<FileRouterState>,
    body: Result<Json<GetFilesByDirRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<DirOrFileResponse>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let items = state.file_service.get_files_by_dir(&req.dir, &req.root).await?;
    let response: Vec<DirOrFileResponse> = items.into_iter().map(to_dir_or_file_response).collect();
    Ok(Json(ApiResponse::ok(response)))
}

async fn list_workspace_files(
    State(state): State<FileRouterState>,
    body: Result<Json<ListWorkspaceFilesRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<WorkspaceFlatFileResponse>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let root = req.root.trim();
    if root.is_empty() {
        return Err(ApiError::BadRequest("root 为必填项".to_owned()));
    }
    let items = state
        .file_service
        .list_workspace_files_with_extra_root(root, Some(Path::new(root)))
        .await?;

    let response: Vec<WorkspaceFlatFileResponse> = items.into_iter().map(to_flat_file_response).collect();
    Ok(Json(ApiResponse::ok(response)))
}

async fn get_file_metadata(
    State(state): State<FileRouterState>,
    body: Result<Json<GetFileMetadataRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<FileMetadataResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let meta = state
        .file_service
        .get_file_metadata(&req.path, req.workspace.as_deref().map(Path::new))
        .await?;
    Ok(Json(ApiResponse::ok(to_metadata_response(meta))))
}

async fn read_file(
    State(state): State<FileRouterState>,
    body: Result<Json<ReadFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<String>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let content = state
        .file_service
        .read_file(&req.path, req.workspace.as_deref().map(Path::new))
        .await?;
    Ok(Json(ApiResponse::ok(content)))
}

async fn read_file_buffer(
    State(state): State<FileRouterState>,
    body: Result<Json<ReadFileBufferRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<String>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let data = state
        .file_service
        .read_file_buffer(&req.path, req.workspace.as_deref().map(Path::new))
        .await?;
    // Binary data is base64-encoded for JSON transport.
    let encoded = data.map(|bytes| {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    });
    Ok(Json(ApiResponse::ok(encoded)))
}

async fn write_file(
    State(state): State<FileRouterState>,
    body: Result<Json<WriteFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<bool>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let workspace = req.workspace.unwrap_or_else(|| {
        std::path::Path::new(&req.path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let ok = state
        .file_service
        .write_file(&req.path, req.data.as_bytes(), &workspace)
        .await?;
    Ok(Json(ApiResponse::ok(ok)))
}

async fn copy_files(
    State(state): State<FileRouterState>,
    body: Result<Json<CopyFilesRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<CopyFilesResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let result = state
        .file_service
        .copy_files_to_workspace(&req.file_paths, &req.workspace, req.source_root.as_deref())
        .await?;
    Ok(Json(ApiResponse::ok(to_copy_response(result))))
}

async fn remove_entry(
    State(state): State<FileRouterState>,
    body: Result<Json<RemoveEntryRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let workspace = req.workspace.unwrap_or_else(|| {
        std::path::Path::new(&req.path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    state.file_service.remove_entry(&req.path, &workspace).await?;
    Ok(Json(ApiResponse::success()))
}

async fn rename_entry(
    State(state): State<FileRouterState>,
    body: Result<Json<RenameRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<RenameResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let workspace = req.workspace.unwrap_or_else(|| {
        std::path::Path::new(&req.path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let new_path = state
        .file_service
        .rename_entry_with_extra_root(&req.path, &req.new_name, Some(Path::new(&workspace)))
        .await?;
    Ok(Json(ApiResponse::ok(RenameResponse { new_path })))
}

async fn create_temp_file(
    State(state): State<FileRouterState>,
    body: Result<Json<CreateTempFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let path = state.file_service.create_temp_file(&req.file_name).await?;
    Ok(Json(ApiResponse::ok(path)))
}

/// Fields extracted from a `/api/fs/upload` multipart request.
struct UploadMultipartFields {
    file_data: Vec<u8>,
    file_name: Option<String>,
    dispo_file_name: Option<String>,
    conversation_id: Option<String>,
}

/// Strip any directory component from a file name and reject empty results.
/// The returned name is guaranteed not to contain path separators; deeper
/// traversal validation happens in [`IFileService::create_upload_file`].
fn sanitize_upload_filename(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let last = trimmed.rsplit(['/', '\\']).next().unwrap_or("");
    let last = last.trim();
    if last.is_empty() { None } else { Some(last.to_owned()) }
}

async fn extract_upload_multipart(mut multipart: Multipart) -> Result<UploadMultipartFields, ApiError> {
    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = None;
    let mut dispo_file_name: Option<String> = None;
    let mut conversation_id: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("multipart 解析错误：{e}")))?
    {
        let name = field.name().unwrap_or("").to_owned();
        match name.as_str() {
            "file" => {
                // Capture the Content-Disposition filename (if any) before
                // consuming the field body — `field.file_name()` is only
                // available on the field metadata, not on the Bytes below.
                dispo_file_name = field.file_name().and_then(sanitize_upload_filename);
                file_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| ApiError::BadRequest(format!("读取 file 失败：{e}")))?
                        .to_vec(),
                );
            }
            "file_name" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("读取 file_name 失败：{e}")))?;
                if let Some(name) = sanitize_upload_filename(&text) {
                    file_name = Some(name);
                }
            }
            "conversation_id" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("读取 conversation_id 失败：{e}")))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    conversation_id = Some(trimmed.to_owned());
                }
            }
            _ => {}
        }
    }

    let file_data = file_data.ok_or_else(|| ApiError::BadRequest("缺少 'file' 字段".to_owned()))?;

    Ok(UploadMultipartFields {
        file_data,
        file_name,
        dispo_file_name,
        conversation_id,
    })
}

async fn upload_file(
    State(state): State<FileRouterState>,
    multipart: Multipart,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let fields = extract_upload_multipart(multipart).await?;

    let file_name = fields
        .file_name
        .or(fields.dispo_file_name)
        .ok_or_else(|| ApiError::BadRequest("缺少文件名：请提供 'file_name' 或 multipart 文件名".to_owned()))?;

    let path = state
        .file_service
        .create_upload_file(&file_name, &fields.file_data, fields.conversation_id.as_deref())
        .await?;
    Ok(Json(ApiResponse::ok(path)))
}

async fn get_image_base64(
    State(state): State<FileRouterState>,
    body: Result<Json<GetImageBase64Request>, JsonRejection>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let data_url = state
        .file_service
        .get_image_base64(&req.path, req.workspace.as_deref().map(Path::new))
        .await?;
    Ok(Json(ApiResponse::ok(data_url)))
}

async fn fetch_remote_image(
    State(state): State<FileRouterState>,
    body: Result<Json<FetchRemoteImageRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let data_url = state.file_service.fetch_remote_image(&req.url).await;
    Ok(Json(ApiResponse::ok(data_url)))
}

async fn create_zip(
    State(state): State<FileRouterState>,
    body: Result<Json<ZipRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<bool>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let entries: Vec<ZipEntry> = req.files.into_iter().map(to_zip_entry).collect();
    let ok = state
        .file_service
        .create_zip_with_extra_roots(
            &req.path,
            entries,
            req.request_id,
            req.workspace.as_deref().map(Path::new),
            req.source_root.as_deref().map(Path::new),
        )
        .await?;
    Ok(Json(ApiResponse::ok(ok)))
}

async fn cancel_zip(
    State(state): State<FileRouterState>,
    body: Result<Json<CancelZipRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<bool>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let ok = state.file_service.cancel_zip(&req.request_id).await;
    Ok(Json(ApiResponse::ok(ok)))
}

// ---------------------------------------------------------------------------
// D. File watch — handlers
// ---------------------------------------------------------------------------

async fn start_watch(
    State(state): State<FileRouterState>,
    body: Result<Json<FileWatchRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.watch_service.start_watch(&req.file_path).await?;
    Ok(Json(ApiResponse::success()))
}

async fn stop_watch(
    State(state): State<FileRouterState>,
    body: Result<Json<FileWatchRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.watch_service.stop_watch(&req.file_path).await?;
    Ok(Json(ApiResponse::success()))
}

async fn stop_all_watches(State(state): State<FileRouterState>) -> Result<Json<ApiResponse<()>>, ApiError> {
    state.watch_service.stop_all_watches().await?;
    Ok(Json(ApiResponse::success()))
}

async fn start_office_watch(
    State(state): State<FileRouterState>,
    body: Result<Json<WorkspaceOfficeWatchRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let allowed_roots: Vec<&Path> = state.allowed_roots.iter().map(std::path::PathBuf::as_path).collect();
    crate::path_safety::validate_path_with_extra_root(&req.workspace, &allowed_roots, Some(Path::new(&req.workspace)))?;
    state.watch_service.start_office_watch(&req.workspace).await?;
    Ok(Json(ApiResponse::success()))
}

async fn stop_office_watch(
    State(state): State<FileRouterState>,
    body: Result<Json<WorkspaceOfficeWatchRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.watch_service.stop_office_watch(&req.workspace).await?;
    Ok(Json(ApiResponse::success()))
}

// ---------------------------------------------------------------------------
// E. Persistent workspace Git — handlers
// ---------------------------------------------------------------------------

async fn git_ensure(
    State(state): State<FileRouterState>,
    body: Result<Json<GitWorkspaceRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<GitRepositoryResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let info = state.git_service.ensure(&req.workspace).await?;
    Ok(Json(ApiResponse::ok(to_git_repository_response(info))))
}

async fn git_info(
    State(state): State<FileRouterState>,
    body: Result<Json<GitWorkspaceRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<GitRepositoryResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let info = state.git_service.repository_info(&req.workspace).await?;
    Ok(Json(ApiResponse::ok(to_git_repository_response(info))))
}

async fn git_status(
    State(state): State<FileRouterState>,
    body: Result<Json<GitWorkspaceRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<GitStatusResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let result = state.git_service.status(&req.workspace).await?;
    Ok(Json(ApiResponse::ok(to_git_status_response(result))))
}

async fn git_baseline(
    State(state): State<FileRouterState>,
    body: Result<Json<GitFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<String>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let content = state
        .git_service
        .baseline_content(&req.workspace, &req.file_path)
        .await?;
    Ok(Json(ApiResponse::ok(content)))
}

async fn git_index_content(
    State(state): State<FileRouterState>,
    body: Result<Json<GitFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<String>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let content = state.git_service.index_content(&req.workspace, &req.file_path).await?;
    Ok(Json(ApiResponse::ok(content)))
}

async fn git_stage_file(
    State(state): State<FileRouterState>,
    body: Result<Json<GitFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.git_service.stage_file(&req.workspace, &req.file_path).await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_stage_all(
    State(state): State<FileRouterState>,
    body: Result<Json<GitWorkspaceRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.git_service.stage_all(&req.workspace).await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_unstage_file(
    State(state): State<FileRouterState>,
    body: Result<Json<GitFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.git_service.unstage_file(&req.workspace, &req.file_path).await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_unstage_all(
    State(state): State<FileRouterState>,
    body: Result<Json<GitWorkspaceRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.git_service.unstage_all(&req.workspace).await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_discard(
    State(state): State<FileRouterState>,
    body: Result<Json<GitFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.git_service.discard_file(&req.workspace, &req.file_path).await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_history(
    State(state): State<FileRouterState>,
    body: Result<Json<GitHistoryRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<GitCommitResponse>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let commits = state
        .git_service
        .history(
            &req.workspace,
            req.file_path.as_deref(),
            req.reference.as_deref(),
            req.limit,
        )
        .await?;
    Ok(Json(ApiResponse::ok(
        commits.into_iter().map(to_git_commit_response).collect(),
    )))
}

async fn git_commit_files(
    State(state): State<FileRouterState>,
    body: Result<Json<GitCommitFilesRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<GitCommitFileResponse>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let files = state.git_service.commit_files(&req.workspace, &req.revision).await?;
    Ok(Json(ApiResponse::ok(
        files.into_iter().map(to_git_commit_file_response).collect(),
    )))
}

async fn git_revision(
    State(state): State<FileRouterState>,
    body: Result<Json<GitRevisionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<GitRevisionResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let revision = state
        .git_service
        .revision(&req.workspace, &req.file_path, &req.revision)
        .await?;
    Ok(Json(ApiResponse::ok(to_git_revision_response(revision))))
}

async fn git_create_branch(
    State(state): State<FileRouterState>,
    body: Result<Json<GitBranchCreateRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state
        .git_service
        .create_branch(&req.workspace, &req.name, req.start_point.as_deref())
        .await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_switch_branch(
    State(state): State<FileRouterState>,
    body: Result<Json<GitBranchSwitchRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.git_service.switch_branch(&req.workspace, &req.name).await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_checkout_revision(
    State(state): State<FileRouterState>,
    body: Result<Json<GitRevisionCheckoutRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state
        .git_service
        .checkout_revision(&req.workspace, &req.revision)
        .await?;
    Ok(Json(ApiResponse::success()))
}

async fn git_clone(
    State(state): State<FileRouterState>,
    body: Result<Json<GitCloneRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<GitRepositoryResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let repository = state
        .git_service
        .clone_repository(&req.repository_url, &req.parent_directory)
        .await?;
    Ok(Json(ApiResponse::ok(to_git_repository_response(repository))))
}

async fn git_commit(
    State(state): State<FileRouterState>,
    body: Result<Json<GitCommitRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let hash = state
        .git_service
        .commit(&req.workspace, &req.message, req.include_unstaged)
        .await?;
    Ok(Json(ApiResponse::ok(hash)))
}

macro_rules! git_workspace_mutation_handler {
    ($name:ident, $method:ident) => {
        async fn $name(
            State(state): State<FileRouterState>,
            body: Result<Json<GitWorkspaceRequest>, JsonRejection>,
        ) -> Result<Json<ApiResponse<()>>, ApiError> {
            let Json(req) = body.map_err(ApiError::from)?;
            state.git_service.$method(&req.workspace).await?;
            Ok(Json(ApiResponse::success()))
        }
    };
}

git_workspace_mutation_handler!(git_fetch, fetch);
git_workspace_mutation_handler!(git_pull, pull);
git_workspace_mutation_handler!(git_push, push);
git_workspace_mutation_handler!(git_sync, sync);

async fn git_create_worktree(
    State(state): State<FileRouterState>,
    body: Result<Json<GitWorktreeCreateRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<GitWorktreeResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let worktree = state
        .git_service
        .create_worktree(&req.workspace, &req.path, &req.branch, req.start_point.as_deref())
        .await?;
    Ok(Json(ApiResponse::ok(to_git_worktree_response(worktree))))
}

async fn git_remove_worktree(
    State(state): State<FileRouterState>,
    body: Result<Json<GitWorktreeRemoveRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.git_service.remove_worktree(&req.workspace, &req.path).await?;
    Ok(Json(ApiResponse::success()))
}

// ---------------------------------------------------------------------------
// Domain → DTO conversions
// ---------------------------------------------------------------------------

fn to_dir_or_file_response(d: DirOrFile) -> DirOrFileResponse {
    let children = if d.is_dir {
        Some(d.children.into_iter().map(to_dir_or_file_response).collect())
    } else {
        None
    };
    DirOrFileResponse {
        name: d.name,
        full_path: d.full_path,
        relative_path: d.relative_path,
        is_dir: d.is_dir,
        is_file: !d.is_dir,
        children,
    }
}

fn to_flat_file_response(f: WorkspaceFlatFile) -> WorkspaceFlatFileResponse {
    WorkspaceFlatFileResponse {
        name: f.name,
        full_path: f.full_path,
        relative_path: f.relative_path,
    }
}

fn to_metadata_response(m: FileMetadata) -> FileMetadataResponse {
    FileMetadataResponse {
        name: m.name,
        path: m.path,
        size: m.size,
        mime_type: m.mime_type,
        last_modified: m.last_modified,
        is_directory: if m.is_directory { Some(true) } else { None },
    }
}

fn to_copy_response(r: CopyResult) -> CopyFilesResponse {
    CopyFilesResponse {
        copied_files: r.copied_files,
        failed_files: r.failed_files,
    }
}

fn to_zip_entry(e: tjuaeui_api_types::ZipFileEntry) -> ZipEntry {
    if let Some(content) = e.content {
        ZipEntry::Text { name: e.name, content }
    } else if let Some(file_path) = e.file_path {
        ZipEntry::Disk {
            name: e.name,
            file_path,
        }
    } else {
        // Fallback: treat as empty text entry
        ZipEntry::Text {
            name: e.name,
            content: String::new(),
        }
    }
}

fn to_git_file_status(status: GitFileStatus) -> GitFileStatusResponse {
    match status {
        GitFileStatus::Added => GitFileStatusResponse::Added,
        GitFileStatus::Modified => GitFileStatusResponse::Modified,
        GitFileStatus::Deleted => GitFileStatusResponse::Deleted,
        GitFileStatus::Renamed => GitFileStatusResponse::Renamed,
        GitFileStatus::Untracked => GitFileStatusResponse::Untracked,
        GitFileStatus::Conflicted => GitFileStatusResponse::Conflicted,
    }
}

fn to_git_file_change_response(c: GitFileChange) -> GitFileChangeResponse {
    GitFileChangeResponse {
        file_path: c.file_path,
        relative_path: c.relative_path,
        old_relative_path: c.old_relative_path,
        status: to_git_file_status(c.status),
    }
}

fn to_git_status_response(status: GitStatus) -> GitStatusResponse {
    GitStatusResponse {
        conflicted: status.conflicted.into_iter().map(to_git_file_change_response).collect(),
        staged: status.staged.into_iter().map(to_git_file_change_response).collect(),
        unstaged: status.unstaged.into_iter().map(to_git_file_change_response).collect(),
    }
}

fn to_git_branch_response(branch: GitBranch) -> GitBranchResponse {
    GitBranchResponse {
        name: branch.name,
        current: branch.current,
        checked_out: branch.checked_out,
        commit: branch.commit,
    }
}

fn to_git_worktree_response(worktree: GitWorktree) -> GitWorktreeResponse {
    GitWorktreeResponse {
        path: worktree.path,
        branch: worktree.branch,
        head: worktree.head,
        current: worktree.current,
        locked: worktree.locked,
    }
}

fn to_git_repository_response(info: GitRepositoryInfo) -> GitRepositoryResponse {
    GitRepositoryResponse {
        repository_root: info.repository_root,
        workspace_path: info.workspace_path,
        workspace_relative_path: info.workspace_relative_path,
        branch: info.branch,
        head_commit: info.head_commit,
        upstream: info.upstream,
        ahead: info.ahead,
        behind: info.behind,
        dirty: info.dirty,
        branches: info.branches.into_iter().map(to_git_branch_response).collect(),
        worktrees: info.worktrees.into_iter().map(to_git_worktree_response).collect(),
        remotes: info.remotes,
    }
}

fn to_git_commit_response(commit: GitCommit) -> GitCommitResponse {
    GitCommitResponse {
        hash: commit.hash,
        short_hash: commit.short_hash,
        parents: commit.parents,
        decorations: commit.decorations,
        author: commit.author,
        authored_at: commit.authored_at,
        subject: commit.subject,
    }
}

fn to_git_commit_file_response(file: GitCommitFile) -> GitCommitFileResponse {
    GitCommitFileResponse {
        path: file.path,
        old_path: file.old_path,
        status: to_git_file_status(file.status),
    }
}

fn to_git_revision_response(revision: GitRevision) -> GitRevisionResponse {
    GitRevisionResponse {
        revision: revision.revision,
        file_path: revision.file_path,
        original_revision: revision.original_revision,
        original_content: revision.original_content,
        modified_content: revision.modified_content,
        patch: revision.patch,
        binary: revision.binary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn file_path_outside_sandbox_maps_to_explicit_api_code() {
        let api_err = ApiError::from(FileError::PathOutsideSandbox {
            message: "path '/tmp/x' is outside the allowed sandbox".into(),
            field: Some("path"),
            operation: Some("access"),
        });
        assert_eq!(api_err.error_code(), "PATH_OUTSIDE_SANDBOX");
        assert_eq!(api_err.error_details().unwrap()["field"], "path");
        assert_eq!(api_err.error_details().unwrap()["operation"], "access");
    }

    #[test]
    fn browse_roots_are_resolved_lazily() {
        let calls = Arc::new(AtomicUsize::new(0));
        let roots = BrowseRoots::with_resolver({
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                vec![std::env::current_dir().unwrap()]
            }
        });

        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let first = roots.get();
        let second = roots.get();

        assert!(!first.is_empty());
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dir_or_file_response_conversion_file() {
        let d = DirOrFile {
            name: "test.txt".into(),
            full_path: "/ws/test.txt".into(),
            relative_path: "test.txt".into(),
            is_dir: false,
            children: vec![],
        };
        let r = to_dir_or_file_response(d);
        assert_eq!(r.name, "test.txt");
        assert!(!r.is_dir);
        assert!(r.is_file);
        assert!(r.children.is_none());
    }

    #[test]
    fn dir_or_file_response_conversion_dir_with_children() {
        let d = DirOrFile {
            name: "src".into(),
            full_path: "/ws/src".into(),
            relative_path: "src".into(),
            is_dir: true,
            children: vec![DirOrFile {
                name: "main.rs".into(),
                full_path: "/ws/src/main.rs".into(),
                relative_path: "src/main.rs".into(),
                is_dir: false,
                children: vec![],
            }],
        };
        let r = to_dir_or_file_response(d);
        assert!(r.is_dir);
        assert!(!r.is_file);
        let children = r.children.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "main.rs");
    }

    #[test]
    fn flat_file_response_conversion() {
        let f = WorkspaceFlatFile {
            name: "lib.rs".into(),
            full_path: "/ws/src/lib.rs".into(),
            relative_path: "src/lib.rs".into(),
        };
        let r = to_flat_file_response(f);
        assert_eq!(r.name, "lib.rs");
        assert_eq!(r.full_path, "/ws/src/lib.rs");
        assert_eq!(r.relative_path, "src/lib.rs");
    }

    #[test]
    fn metadata_response_conversion_file() {
        let m = FileMetadata {
            name: "readme.md".into(),
            path: "/ws/readme.md".into(),
            size: 1024,
            mime_type: "text/markdown".into(),
            last_modified: 1700000000000,
            is_directory: false,
        };
        let r = to_metadata_response(m);
        assert_eq!(r.name, "readme.md");
        assert_eq!(r.size, 1024);
        assert!(r.is_directory.is_none());
    }

    #[test]
    fn metadata_response_conversion_directory() {
        let m = FileMetadata {
            name: "src".into(),
            path: "/ws/src".into(),
            size: 0,
            mime_type: "".into(),
            last_modified: 1700000000000,
            is_directory: true,
        };
        let r = to_metadata_response(m);
        assert_eq!(r.is_directory, Some(true));
    }

    #[test]
    fn zip_entry_conversion_text() {
        let e = tjuaeui_api_types::ZipFileEntry {
            name: "a.txt".into(),
            content: Some("hello".into()),
            file_path: None,
        };
        let z = to_zip_entry(e);
        match z {
            ZipEntry::Text { name, content } => {
                assert_eq!(name, "a.txt");
                assert_eq!(content, "hello");
            }
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn zip_entry_conversion_disk() {
        let e = tjuaeui_api_types::ZipFileEntry {
            name: "b.bin".into(),
            content: None,
            file_path: Some("/src/b.bin".into()),
        };
        let z = to_zip_entry(e);
        match z {
            ZipEntry::Disk { name, file_path } => {
                assert_eq!(name, "b.bin");
                assert_eq!(file_path, "/src/b.bin");
            }
            _ => panic!("expected Disk variant"),
        }
    }

    #[test]
    fn zip_entry_conversion_empty_fallback() {
        let e = tjuaeui_api_types::ZipFileEntry {
            name: "empty.txt".into(),
            content: None,
            file_path: None,
        };
        let z = to_zip_entry(e);
        match z {
            ZipEntry::Text { name, content } => {
                assert_eq!(name, "empty.txt");
                assert!(content.is_empty());
            }
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn git_status_response_conversion() {
        let result = GitStatus {
            conflicted: vec![],
            staged: vec![GitFileChange {
                file_path: "/ws/a.txt".into(),
                relative_path: "a.txt".into(),
                old_relative_path: None,
                status: GitFileStatus::Added,
            }],
            unstaged: vec![GitFileChange {
                file_path: "/ws/b.txt".into(),
                relative_path: "b.txt".into(),
                old_relative_path: None,
                status: GitFileStatus::Modified,
            }],
        };
        let r = to_git_status_response(result);
        assert_eq!(r.staged.len(), 1);
        assert_eq!(r.staged[0].file_path, "/ws/a.txt");
        assert_eq!(r.staged[0].status, GitFileStatusResponse::Added);
        assert_eq!(r.unstaged.len(), 1);
        assert_eq!(r.unstaged[0].status, GitFileStatusResponse::Modified);
    }

    // ---- sanitize_upload_filename -----------------------------------------

    #[test]
    fn sanitize_upload_filename_strips_directory_components() {
        assert_eq!(sanitize_upload_filename("a/b/c.png").as_deref(), Some("c.png"));
        assert_eq!(sanitize_upload_filename("C:\\tmp\\d.jpg").as_deref(), Some("d.jpg"));
        assert_eq!(
            sanitize_upload_filename("  spaced.txt  ").as_deref(),
            Some("spaced.txt")
        );
    }

    #[test]
    fn sanitize_upload_filename_rejects_empty() {
        assert_eq!(sanitize_upload_filename(""), None);
        assert_eq!(sanitize_upload_filename("   "), None);
        assert_eq!(sanitize_upload_filename("/"), None);
        assert_eq!(sanitize_upload_filename("a/b/"), None);
    }

    #[test]
    fn sanitize_upload_filename_plain_passthrough() {
        assert_eq!(sanitize_upload_filename("image.png").as_deref(), Some("image.png"));
    }
}
