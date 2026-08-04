#![allow(clippy::disallowed_types)]

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::http::StatusCode;
use axum::routing::{get, post};

use tjuaeui_api_types::{
    ApiResponse, AssetKind, MaterializeSkillsRequest, MaterializeSkillsResponse, MaterializedSkillRef,
    ReadAssistantRuleRequest, SkillListItemResponse, SkillSourceResponse,
};
use tjuaeui_auth::CurrentUser;
use tjuaeui_common::ApiError;

use crate::assistant_rules::AssistantRuleDispatcher;
use crate::skill_runtime::SkillPaths;
use crate::{AssetCatalogService, AssetError};

/// Runtime-only skill API dependencies.
#[derive(Clone)]
pub struct SkillRouterState {
    pub skill_paths: SkillPaths,
    /// Canonical, user-scoped editable skill Definitions.
    pub asset_catalog: Arc<AssetCatalogService>,
    /// Assistant rules are resolved exclusively through the canonical
    /// assistant Definition service. Missing wiring fails closed.
    pub assistant_dispatcher: Option<Arc<dyn AssistantRuleDispatcher>>,
}

/// Build the authenticated runtime skill router.
///
/// Local asset creation, editing, installation and deletion live exclusively
/// under the canonical local-asset API. This router intentionally exposes only
/// runtime reads/projection and assistant-rule reads.
pub fn skill_routes(state: SkillRouterState) -> Router {
    Router::new()
        .route("/api/skills", get(list_skills))
        .route("/api/skills/materialize-for-agent", post(materialize_for_agent))
        .route("/api/skills/assistant-rule/read", post(read_assistant_rule))
        .with_state(state)
}

/// List local skill Definitions for the authenticated user.
async fn list_skills(
    State(state): State<SkillRouterState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<SkillListItemResponse>>>, ApiError> {
    let assets = state
        .asset_catalog
        .list(&user.id, Some(AssetKind::Skill), None)
        .await
        .map_err(asset_api_error)?;
    let mut response: Vec<SkillListItemResponse> = assets
        .into_iter()
        .filter_map(|asset| {
            let name = asset.runtime_id?;
            Some(SkillListItemResponse {
                location: state
                    .skill_paths
                    .user_skills_dir
                    .join(&name)
                    .to_string_lossy()
                    .into_owned(),
                name,
                description: asset.description.unwrap_or_default(),
                is_custom: true,
                source: SkillSourceResponse::Asset,
            })
        })
        .collect();
    response.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(Json(ApiResponse::ok(response)))
}

/// Resolve requested local skills from the current user's active bindings.
async fn materialize_for_agent(
    State(state): State<SkillRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<MaterializeSkillsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<MaterializeSkillsResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let conversation_id = request.conversation_id.trim();
    if conversation_id.is_empty()
        || conversation_id.len() > 128
        || conversation_id.contains('/')
        || conversation_id.contains('\\')
        || conversation_id.contains("..")
    {
        return Err(ApiError::BadRequest("conversation_id 无效".into()));
    }
    let mut local_asset_ids = BTreeSet::new();
    let mut skills = Vec::with_capacity(request.skills.len());
    for reference in request.skills {
        let bound = state
            .asset_catalog
            .resolve_bound_runtime_asset(&user.id, AssetKind::Skill, &reference)
            .await
            .map_err(asset_api_error)?;
        if !local_asset_ids.insert(bound.provenance.local_asset_id.clone()) {
            return Err(ApiError::BadRequest(format!(
                "技能引用重复映射到本地资产 {}",
                bound.provenance.local_asset_id
            )));
        }
        skills.push(MaterializedSkillRef {
            name: bound.provenance.runtime_id,
            source_path: bound.workspace_path.to_string_lossy().into_owned(),
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(Json(ApiResponse::ok(MaterializeSkillsResponse { skills })))
}

async fn read_assistant_rule(
    State(state): State<SkillRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<ReadAssistantRuleRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let dispatcher = canonical_assistant_dispatcher(&state)?;
    let content = dispatcher
        .read_rule(&user.id, &request.assistant_id, request.locale.as_deref())
        .await
        .map_err(asset_api_error)?;
    Ok(Json(ApiResponse::ok(content)))
}

fn canonical_assistant_dispatcher(state: &SkillRouterState) -> Result<&Arc<dyn AssistantRuleDispatcher>, ApiError> {
    state
        .assistant_dispatcher
        .as_ref()
        .ok_or_else(|| ApiError::Internal("助手 Definition 服务未连接".into()))
}

fn asset_api_error(error: AssetError) -> ApiError {
    match error {
        AssetError::NotFound(_) => ApiError::coded(StatusCode::NOT_FOUND, "ASSET_NOT_FOUND", "本地技能不存在。", None),
        AssetError::InvalidMetadata(_) | AssetError::InvalidState(_) => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ASSET_RUNTIME_BINDING_INVALID",
            "技能运行绑定不存在或已失效。",
            None,
        ),
        AssetError::DigestMismatch { .. } | AssetError::CorruptObject(_) => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ASSET_RUNTIME_BINDING_STALE",
            "技能 Definition 已变化，请重新校验并激活。",
            None,
        ),
        _ => ApiError::coded(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ASSET_INTERNAL",
            "读取本地技能失败，请查看服务日志。",
            None,
        ),
    }
}
