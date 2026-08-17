#![allow(clippy::disallowed_types)]

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path as AxumPath, State};
use axum::routing::{delete, get, post, put};
use tjuaeui_api_types::{
    ApiResponse, CloneSkillRequest, CopySkillRequest, CreateSkillRequest, ImportSkillRequest,
    MarketFileComparisonResponse, MarketInfoResponse, MarketSkillComparisonResponse, MarketSkillResponse,
    MarketSyncStateResponse, MaterializeSkillsRequest, MaterializeSkillsResponse, MaterializedSkillRef,
    PublishMarketSkillRequest, PublishMarketSkillResponse, ReadAssistantRuleRequest, SkillGitStatusResponse,
    SkillPreferencesResponse, SkillSourceResponse, SkillWorkspaceResponse, UpdateSkillPreferencesRequest,
    WriteAssistantRuleRequest,
};
use tjuaeui_common::{ApiError, WorkspaceGitProvisioner, WorkspaceGitState};

use crate::classifier::AssistantRuleDispatcher;
use crate::skill_storage::{self, SkillPaths};
use crate::{InstalledSkill, MarketSyncState, SkillPreferences, SkillSource};

#[derive(Clone)]
pub struct SkillRouterState {
    pub skill_paths: SkillPaths,
    pub git: Arc<dyn WorkspaceGitProvisioner>,
    #[allow(clippy::type_complexity)]
    pub assistant_dispatcher: Option<Arc<dyn AssistantRuleDispatcher>>,
}

pub fn skill_routes(state: SkillRouterState) -> Router {
    Router::new()
        .route("/api/skills", get(list_skills))
        .route("/api/skills/market", get(list_market_skills))
        .route(
            "/api/skills/market/{market_id}/{slug}/install",
            post(install_market_skill),
        )
        .route(
            "/api/skills/market/{market_id}/{slug}/update",
            post(update_market_skill),
        )
        .route(
            "/api/skills/market/{market_id}/{slug}/compare",
            get(compare_market_skill),
        )
        .route(
            "/api/skills/market/{market_id}/{slug}/publish",
            post(publish_market_skill),
        )
        .route("/api/skills/{slug}/copy", post(copy_skill))
        .route("/api/skills/{slug}/preferences", put(update_preferences))
        .route("/api/skills/import", post(import_skill))
        .route("/api/skills/create", post(create_skill))
        .route("/api/skills/clone", post(clone_skill))
        .route("/api/skills/{name}", delete(delete_skill))
        .route("/api/skills/materialize-for-agent", post(materialize_for_agent))
        .route("/api/skills/assistant-rule/read", post(read_assistant_rule))
        .route("/api/skills/assistant-rule/write", post(write_assistant_rule))
        .route("/api/skills/assistant-rule/{id}", delete(delete_assistant_rule))
        .with_state(state)
}

async fn list_skills(
    State(state): State<SkillRouterState>,
) -> Result<Json<ApiResponse<Vec<SkillWorkspaceResponse>>>, ApiError> {
    let items = crate::list_installed_skills(&state.skill_paths.user_skills_dir).await?;
    let mut response = Vec::with_capacity(items.len());
    for item in items {
        response.push(to_workspace_response(&state, item).await);
    }
    Ok(Json(ApiResponse::ok(response)))
}

async fn list_market_skills(
    State(state): State<SkillRouterState>,
) -> Result<Json<ApiResponse<Vec<MarketSkillResponse>>>, ApiError> {
    let installed = crate::list_installed_skills(&state.skill_paths.user_skills_dir).await?;
    let indexes = crate::market_indexes().await?;
    let mut response = Vec::new();
    for index in indexes {
        let market = MarketInfoResponse {
            id: index.market.id.clone(),
            name: index.market.name.clone(),
            repository: index.repository.clone(),
            revision: index.revision.clone(),
        };
        for entry in index.skills.clone() {
            let local = installed.iter().find(|skill| {
                matches!(
                    &skill.source,
                    SkillSource::Market { market_id, repository, path, .. }
                        if market_id == &market.id && repository == &market.repository && path == &entry.path
                )
            });
            let installed_version = local.map(|skill| skill.version.clone());
            let sync_state = crate::market_sync_state(local, &index, &entry, state.git.clone()).await?;
            response.push(MarketSkillResponse {
                id: entry.id.clone(),
                slug: entry.id,
                name: entry.name,
                description: entry.description,
                version: entry.version,
                path: entry.path,
                digest: entry.digest,
                categories: entry.categories,
                market: market.clone(),
                installed: local.is_some(),
                installed_version,
                sync_state: to_sync_state_response(sync_state),
            });
        }
    }
    Ok(Json(ApiResponse::ok(response)))
}

async fn compare_market_skill(
    State(state): State<SkillRouterState>,
    AxumPath((market_id, slug)): AxumPath<(String, String)>,
) -> Result<Json<ApiResponse<MarketSkillComparisonResponse>>, ApiError> {
    let comparison =
        crate::compare_market_skill(&state.skill_paths.user_skills_dir, &market_id, &slug, state.git.clone()).await?;
    Ok(Json(ApiResponse::ok(MarketSkillComparisonResponse {
        slug: comparison.slug,
        base_revision: comparison.base_revision,
        remote_revision: comparison.remote_revision,
        sync_state: to_sync_state_response(comparison.sync_state),
        files: comparison
            .files
            .into_iter()
            .map(|file| MarketFileComparisonResponse {
                path: file.path,
                status: file.status,
                binary: file.binary,
                local_content: file.local_content,
                remote_content: file.remote_content,
            })
            .collect(),
    })))
}

async fn publish_market_skill(
    State(state): State<SkillRouterState>,
    AxumPath((market_id, slug)): AxumPath<(String, String)>,
    body: Result<Json<PublishMarketSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<PublishMarketSkillResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let result = crate::publish_market_skill(
        &state.skill_paths.user_skills_dir,
        &market_id,
        &slug,
        &request.fork_repository_url,
        &request.message,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(PublishMarketSkillResponse {
        branch: result.branch,
        commit: result.commit,
        compare_url: result.compare_url,
    })))
}

fn to_sync_state_response(state: MarketSyncState) -> MarketSyncStateResponse {
    match state {
        MarketSyncState::NotInstalled => MarketSyncStateResponse::NotInstalled,
        MarketSyncState::Synced => MarketSyncStateResponse::Synced,
        MarketSyncState::LocalChanged => MarketSyncStateResponse::LocalChanged,
        MarketSyncState::UpdateAvailable => MarketSyncStateResponse::UpdateAvailable,
        MarketSyncState::Diverged => MarketSyncStateResponse::Diverged,
    }
}

async fn install_market_skill(
    State(state): State<SkillRouterState>,
    AxumPath((market_id, slug)): AxumPath<(String, String)>,
) -> Result<Json<ApiResponse<SkillWorkspaceResponse>>, ApiError> {
    let skill = crate::install_market_skill(
        &state.skill_paths.user_skills_dir,
        &market_id,
        &slug,
        false,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(to_workspace_response(&state, skill).await)))
}

async fn update_market_skill(
    State(state): State<SkillRouterState>,
    AxumPath((market_id, slug)): AxumPath<(String, String)>,
) -> Result<Json<ApiResponse<SkillWorkspaceResponse>>, ApiError> {
    let skill = crate::install_market_skill(
        &state.skill_paths.user_skills_dir,
        &market_id,
        &slug,
        true,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(to_workspace_response(&state, skill).await)))
}

async fn copy_skill(
    State(state): State<SkillRouterState>,
    AxumPath(slug): AxumPath<String>,
    body: Result<Json<CopySkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillWorkspaceResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let skill = crate::copy_skill(
        &state.skill_paths.user_skills_dir,
        &slug,
        &request.target_slug,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(to_workspace_response(&state, skill).await)))
}

async fn update_preferences(
    State(state): State<SkillRouterState>,
    AxumPath(slug): AxumPath<String>,
    body: Result<Json<UpdateSkillPreferencesRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillWorkspaceResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let skill = crate::update_skill_preferences(
        &state.skill_paths.user_skills_dir,
        &slug,
        SkillPreferences {
            enabled: request.enabled,
            auto_inject: request.auto_inject,
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(to_workspace_response(&state, skill).await)))
}

async fn to_workspace_response(state: &SkillRouterState, skill: InstalledSkill) -> SkillWorkspaceResponse {
    let git_status = state
        .git
        .workspace_git_state(&skill.path)
        .await
        .unwrap_or(WorkspaceGitState::Unknown);
    SkillWorkspaceResponse {
        id: skill.id,
        slug: skill.slug,
        name: skill.name,
        description: skill.description,
        version: skill.version,
        path: skill.path.to_string_lossy().into_owned(),
        source: match skill.source {
            SkillSource::Local => SkillSourceResponse::Local,
            SkillSource::Market {
                market_id,
                repository,
                path,
                revision,
            } => SkillSourceResponse::Market {
                market_id,
                repository,
                path,
                revision,
            },
        },
        categories: skill.categories,
        preferences: SkillPreferencesResponse {
            enabled: skill.preferences.enabled,
            auto_inject: skill.preferences.auto_inject,
        },
        git_status: match git_status {
            WorkspaceGitState::Clean => SkillGitStatusResponse::Clean,
            WorkspaceGitState::Modified => SkillGitStatusResponse::Modified,
            WorkspaceGitState::Conflicted => SkillGitStatusResponse::Conflicted,
            WorkspaceGitState::Unknown => SkillGitStatusResponse::Unknown,
        },
    }
}

async fn import_skill(
    State(state): State<SkillRouterState>,
    body: Result<Json<ImportSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillWorkspaceResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let skill = crate::import_skill(
        &state.skill_paths.user_skills_dir,
        Path::new(&request.skill_path),
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(to_workspace_response(&state, skill).await)))
}

async fn create_skill(
    State(state): State<SkillRouterState>,
    body: Result<Json<CreateSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillWorkspaceResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let skill = crate::create_skill(
        &state.skill_paths.user_skills_dir,
        &request.slug,
        &request.name,
        &request.description,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(to_workspace_response(&state, skill).await)))
}

async fn clone_skill(
    State(state): State<SkillRouterState>,
    body: Result<Json<CloneSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillWorkspaceResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let skill = crate::clone_skill(
        &state.skill_paths.user_skills_dir,
        &request.repository_url,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(to_workspace_response(&state, skill).await)))
}

async fn delete_skill(
    State(state): State<SkillRouterState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    crate::delete_installed_skill(&state.skill_paths.user_skills_dir, &name).await?;
    Ok(Json(ApiResponse::success()))
}

async fn materialize_for_agent(
    State(state): State<SkillRouterState>,
    body: Result<Json<MaterializeSkillsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<MaterializeSkillsResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    if request.conversation_id.trim().is_empty() {
        return Err(ApiError::BadRequest("conversationId 不能为空".into()));
    }
    let mut skills = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for reference in &request.skills {
        let Some(skill) = crate::resolve_installed_skill(&state.skill_paths.user_skills_dir, reference).await? else {
            continue;
        };
        if skill.preferences.enabled && seen.insert(skill.id.clone()) {
            skills.push(MaterializedSkillRef {
                name: skill.slug,
                source_path: skill.path.to_string_lossy().into_owned(),
            });
        }
    }
    Ok(Json(ApiResponse::ok(MaterializeSkillsResponse { skills })))
}

async fn read_assistant_rule(
    State(state): State<SkillRouterState>,
    body: Result<Json<ReadAssistantRuleRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    if let Some(dispatcher) = &state.assistant_dispatcher {
        let content = dispatcher
            .read_rule(&request.assistant_id, request.locale.as_deref())
            .await?;
        return Ok(Json(ApiResponse::ok(content)));
    }
    let content =
        skill_storage::read_assistant_rule(&state.skill_paths, &request.assistant_id, request.locale.as_deref())
            .await?;
    Ok(Json(ApiResponse::ok(content)))
}

async fn write_assistant_rule(
    State(state): State<SkillRouterState>,
    body: Result<Json<WriteAssistantRuleRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<bool>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    if let Some(dispatcher) = &state.assistant_dispatcher {
        dispatcher
            .write_rule(&request.assistant_id, request.locale.as_deref(), &request.content)
            .await?;
        return Ok(Json(ApiResponse::ok(true)));
    }
    let ok = skill_storage::write_assistant_rule(
        &state.skill_paths,
        &request.assistant_id,
        &request.content,
        request.locale.as_deref(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(ok)))
}

async fn delete_assistant_rule(
    State(state): State<SkillRouterState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<bool>>, ApiError> {
    if let Some(dispatcher) = &state.assistant_dispatcher {
        return Ok(Json(ApiResponse::ok(dispatcher.delete_rule(&id).await?)));
    }
    Ok(Json(ApiResponse::ok(
        skill_storage::delete_assistant_rule(&state.skill_paths, &id).await?,
    )))
}
