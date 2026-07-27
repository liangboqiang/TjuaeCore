#![allow(clippy::disallowed_types)]

use std::path::{Path as FsPath, PathBuf};

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::routing::post;
use tjuaeui_api_types::{
    ApiResponse, DocumentConversionRequest, GetSnapshotContentRequest, ListSnapshotsRequest, PreviewSnapshotInfoDto,
    SaveSnapshotRequest, SnapshotContentResponse,
};
use tjuaeui_auth::CurrentUser;
use tjuaeui_common::ApiError;
use tjuaeui_file::{FileError, path_safety::validate_path_with_extra_root};

use crate::error::OfficeError;
use crate::state::OfficeRouterState;

impl From<OfficeError> for ApiError {
    fn from(err: OfficeError) -> Self {
        match err {
            OfficeError::Io(error) => ApiError::Internal(format!("输入输出错误：{error}")),
            OfficeError::Snapshot(message) => ApiError::Internal(format!("快照错误：{message}")),
            OfficeError::Json(error) => ApiError::Internal(format!("JSON 错误：{error}")),
            OfficeError::Conversion(message) => ApiError::Internal(format!("转换错误：{message}")),
            OfficeError::ToolNotFound(tool) => ApiError::BadRequest(format!("找不到外部工具：{tool}")),
        }
    }
}

pub fn office_routes(state: OfficeRouterState) -> Router {
    Router::new()
        .route("/api/preview-history/list", post(list_snapshots))
        .route("/api/preview-history/save", post(save_snapshot))
        .route("/api/preview-history/get-content", post(get_snapshot_content))
        .route("/api/document/convert", post(convert_document))
        .with_state(state)
}

async fn list_snapshots(
    State(state): State<OfficeRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<ListSnapshotsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<PreviewSnapshotInfoDto>>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let snapshots = state.snapshot_service.list(&request.target).await?;
    Ok(Json(ApiResponse::ok(snapshots)))
}

async fn save_snapshot(
    State(state): State<OfficeRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<SaveSnapshotRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<PreviewSnapshotInfoDto>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let snapshot = state.snapshot_service.save(&request.target, &request.content).await?;
    Ok(Json(ApiResponse::ok(snapshot)))
}

async fn get_snapshot_content(
    State(state): State<OfficeRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<GetSnapshotContentRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Option<SnapshotContentResponse>>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let snapshot = state
        .snapshot_service
        .get_content(&request.target, &request.snapshot_id)
        .await?;
    Ok(Json(ApiResponse::ok(snapshot)))
}

async fn convert_document(
    State(state): State<OfficeRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<DocumentConversionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<tjuaeui_api_types::DocumentConversionResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let validated_path = validate_office_path(&state, &request.file_path, request.workspace.as_deref())?;
    let response = state
        .conversion_service
        .convert(validated_path.to_string_lossy().as_ref(), request.to)
        .await?;
    Ok(Json(ApiResponse::ok(response)))
}

fn validate_office_path(
    state: &OfficeRouterState,
    file_path: &str,
    workspace: Option<&str>,
) -> Result<PathBuf, ApiError> {
    let allowed_roots: Vec<&FsPath> = state.allowed_roots.iter().map(PathBuf::as_path).collect();
    validate_path_with_extra_root(file_path, &allowed_roots, workspace.map(FsPath::new))
        .map_err(file_error_to_api_error)
}

fn file_error_to_api_error(error: FileError) -> ApiError {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_external_tool_maps_to_bad_request() {
        let error = ApiError::from(OfficeError::ToolNotFound("pandoc".to_owned()));
        assert!(matches!(error, ApiError::BadRequest(message) if message == "找不到外部工具：pandoc"));
    }

    #[test]
    fn path_outside_sandbox_preserves_structured_error() {
        let error = file_error_to_api_error(FileError::PathOutsideSandbox {
            message: "outside".to_owned(),
            field: Some("file_path"),
            operation: Some("read"),
        });
        assert!(matches!(error, ApiError::PathOutsideSandbox { .. }));
    }
}
