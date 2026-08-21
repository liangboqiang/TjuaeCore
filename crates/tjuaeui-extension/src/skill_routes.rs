#![allow(clippy::disallowed_types)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path as AxumPath, Query, State};
use axum::routing::{get, post, put};
use futures_util::future::join_all;
use tjuaeui_api_types::{
    ApiResponse, CompareSkillVersionsQuery, CopySkillRequest, CreateSkillRequest, ExportSkillRequest,
    ImportSkillRequest, PublishSkillVersionRequest, SaveSkillFileRequest, SkillCatalogDetailResponse,
    SkillCatalogFileContentResponse, SkillCatalogFileQuery, SkillCatalogItemResponse, SkillCatalogPageResponse,
    SkillCatalogQuery, SkillFileResponse, SkillIdentityResponse, SkillOperationResponse, SkillPreferencesResponse,
    SkillSourceResponse, SkillVersionComparisonResponse, SkillVersionFileDiffResponse, SkillVersionQuery,
    SkillVersionResponse, UpdateSkillPreferencesRequest, UpdateSkillProfileRequest,
};
use tjuaeui_common::{ApiError, WorkspaceGitProvisioner};
use tjuaeui_db::{ISkillUserPreferenceRepository, SkillUserPreferenceRow, UpsertSkillUserPreferenceParams};
use tjuaeui_runtime::Builder as CommandBuilder;

use crate::error::ExtensionError;
use crate::skill_package::{
    SKILL_PACKAGE_MANIFEST, publish_skill_version, reseal_skill_package, save_skill_manifest_content,
};
use crate::skill_storage::SkillPaths;
use crate::{CatalogDetail, CatalogSkill, SkillSpace};

#[derive(Clone)]
pub struct SkillRouterState {
    pub skill_paths: SkillPaths,
    pub git: Arc<dyn WorkspaceGitProvisioner>,
    pub preferences: Arc<dyn ISkillUserPreferenceRepository>,
    pub can_write_tjuae_hub: bool,
}

pub fn skill_routes(state: SkillRouterState) -> Router {
    Router::new()
        .route("/api/skills/catalog", get(list_skill_catalog))
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}",
            get(get_skill_detail).delete(delete_skill),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/file",
            get(get_skill_file).put(save_skill_file),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/profile",
            put(update_skill_profile),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/publish",
            post(publish_version),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/compare",
            get(compare_skill_versions),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/preferences",
            put(update_preferences),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/copy-to-mine",
            post(copy_to_mine),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/publish-to-tjuae-hub",
            post(publish_to_tjuae_hub),
        )
        .route(
            "/api/skills/catalog/{source}/{namespace}/{slug}/export",
            post(export_skill),
        )
        .route("/api/skills/import", post(import_skill))
        .route("/api/skills/create", post(create_skill))
        .with_state(state)
}

async fn list_skill_catalog(
    State(state): State<SkillRouterState>,
    Query(query): Query<SkillCatalogQuery>,
) -> Result<Json<ApiResponse<SkillCatalogPageResponse>>, ApiError> {
    let mut requested = parse_sources(&query.sources)?;
    // Provider cursors are intentionally opaque and incompatible with one
    // another (SkillHub uses page numbers, ClawHub uses opaque tokens). An
    // aggregate query therefore cannot expose one provider's cursor as if it
    // represented all sources. Pagination remains available for one selected
    // source; the all-source directory is search-first and returns one merged
    // page.
    let aggregate_cursor_allowed = requested.len() == 1;
    let preferences = state
        .preferences
        .list()
        .await
        .map_err(ExtensionError::from)?
        .into_iter()
        .map(|preference| {
            (
                preference_key(&preference.source, &preference.namespace, &preference.slug),
                preference,
            )
        })
        .collect::<HashMap<_, _>>();
    if query.enabled == Some(true) || query.auto_inject == Some(true) {
        requested.retain(|source| {
            preferences.values().any(|preference| {
                preference.source == source.id()
                    && preference.enabled
                    && (query.auto_inject != Some(true) || preference.auto_inject)
            })
        });
    }
    let mut items = Vec::new();
    let mut next_cursor = None;
    let limit = query.limit.unwrap_or(60);
    let provider_results = join_all(requested.into_iter().map(|source| {
        let root = state.skill_paths.user_skills_dir.clone();
        let q = query.q.clone();
        let cursor = query.cursor.clone();
        let git = state.git.clone();
        async move {
            let page = crate::list_catalog(&root, source, &q, "name", cursor.as_deref(), limit, git).await;
            (source, page)
        }
    }))
    .await;
    for (source, result) in provider_results {
        match result {
            Ok(page) => {
                if aggregate_cursor_allowed && next_cursor.is_none() {
                    next_cursor = page.next_cursor.clone();
                }
                items.extend(page.items.into_iter().map(|item| {
                    let identity = identity_for(&item);
                    let key = preference_key(source_id(identity.source), &identity.namespace, &identity.slug);
                    catalog_item_response(item, preferences.get(&key), state.can_write_tjuae_hub)
                }));
            }
            Err(error) => tracing::warn!(source = source.id(), %error, "skill provider unavailable"),
        }
    }
    items.retain(|item| matches_filters(item, &query));
    items.sort_by_key(|item| item.name.to_lowercase());
    let total = items.len() as u64;
    Ok(Json(ApiResponse::ok(SkillCatalogPageResponse {
        items,
        total,
        next_cursor,
    })))
}

async fn get_skill_detail(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    Query(query): Query<SkillVersionQuery>,
) -> Result<Json<ApiResponse<SkillCatalogDetailResponse>>, ApiError> {
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    let preference = state
        .preferences
        .get(&source, &namespace, &slug)
        .await
        .map_err(ExtensionError::from)?;
    let requested_version = query.version.as_deref().or_else(|| {
        preference
            .as_ref()
            .filter(|value| !value.follow_latest)
            .and_then(|value| value.selected_version.as_deref())
    });
    let detail = crate::catalog_detail(
        &state.skill_paths.user_skills_dir,
        space,
        &namespace,
        &slug,
        requested_version,
        state.git.clone(),
    )
    .await?;
    let response = detail_response(
        detail,
        preference.as_ref(),
        requested_version,
        state.can_write_tjuae_hub,
    )?;
    Ok(Json(ApiResponse::ok(response)))
}

async fn get_skill_file(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    Query(query): Query<SkillCatalogFileQuery>,
) -> Result<Json<ApiResponse<SkillCatalogFileContentResponse>>, ApiError> {
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    let local_hub_root = state
        .skill_paths
        .tjuae_hub_worktree_dir
        .as_ref()
        .map(|root| root.join("skills"));
    let editable_hub_worktree = if space == SkillSpace::TjuaeHub && state.can_write_tjuae_hub {
        match local_hub_root.as_ref() {
            Some(root) => crate::load_installed_skill(&root.join(&slug))
                .await
                .ok()
                .is_some_and(|skill| version_is_editable(query.version.as_deref(), &skill.version)),
            None => false,
        }
    } else {
        false
    };
    let file = if editable_hub_worktree
        && local_hub_root
            .as_ref()
            .is_some_and(|root| root.join(&slug).join(&query.path).is_file())
    {
        let root = local_hub_root.as_ref().expect("checked above");
        let target = safe_local_file(root, &slug, &query.path)?;
        let content = tokio::fs::read_to_string(&target).await.map_err(ExtensionError::from)?;
        crate::CatalogFileContent {
            path: query.path.clone(),
            size: content.len() as u64,
            content,
        }
    } else {
        crate::catalog_file_content(
            &state.skill_paths.user_skills_dir,
            space,
            &namespace,
            &slug,
            &query.path,
            query.version.as_deref(),
            state.git.clone(),
        )
        .await?
    };
    Ok(Json(ApiResponse::ok(SkillCatalogFileContentResponse {
        path: file.path,
        content: file.content,
        size: file.size,
        editable: space == SkillSpace::Mine || editable_hub_worktree,
    })))
}

async fn save_skill_file(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    body: Result<Json<SaveSkillFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillCatalogFileContentResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let space = SkillSpace::parse(&source)?;
    let _ = namespace;
    let root = match space {
        SkillSpace::Mine => state.skill_paths.user_skills_dir.clone(),
        SkillSpace::TjuaeHub if state.can_write_tjuae_hub => state
            .skill_paths
            .tjuae_hub_worktree_dir
            .as_ref()
            .map(|root| root.join("skills"))
            .ok_or_else(|| ExtensionError::InvalidRequest("未配置 TjuaeHub 开发工作副本".into()))?,
        _ => return Err(ExtensionError::InvalidRequest("这个技能来源是只读的".into()).into()),
    };
    let directory = root.join(&slug);
    crate::load_installed_skill(&directory).await?;
    let target = safe_local_file(&root, &slug, &request.path)?;
    if request.path == SKILL_PACKAGE_MANIFEST {
        save_skill_manifest_content(&directory, &request.content).await?;
    } else {
        tokio::fs::write(&target, request.content.as_bytes())
            .await
            .map_err(ExtensionError::from)?;
        reseal_skill_package(&directory).await?;
    }
    Ok(Json(ApiResponse::ok(SkillCatalogFileContentResponse {
        path: request.path,
        size: request.content.len() as u64,
        content: request.content,
        editable: true,
    })))
}

async fn update_skill_profile(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    body: Result<Json<UpdateSkillProfileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillCatalogDetailResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    let root = match space {
        SkillSpace::Mine => state.skill_paths.user_skills_dir.clone(),
        SkillSpace::TjuaeHub if state.can_write_tjuae_hub => state
            .skill_paths
            .tjuae_hub_worktree_dir
            .as_ref()
            .map(|root| root.join("skills"))
            .ok_or_else(|| ExtensionError::InvalidRequest("未配置 TjuaeHub 开发工作副本".into()))?,
        _ => return Err(ExtensionError::InvalidRequest("这个技能来源是只读的".into()).into()),
    };
    let updated = crate::update_skill_profile(
        &root.join(&slug),
        &request.name,
        &request.description,
        request.categories,
        request.icon_data_url,
    )
    .await?;
    let preference = state
        .preferences
        .get(&source, &namespace, &slug)
        .await
        .map_err(ExtensionError::from)?;
    let mut detail = crate::catalog_detail(
        &state.skill_paths.user_skills_dir,
        space,
        &namespace,
        &slug,
        None,
        state.git.clone(),
    )
    .await?;
    detail.skill.name = updated.name;
    detail.skill.description = updated.description;
    detail.skill.categories = updated.categories;
    detail.skill.icon_url = updated.icon_url;
    let response = detail_response(detail, preference.as_ref(), None, state.can_write_tjuae_hub)?;
    Ok(Json(ApiResponse::ok(response)))
}

async fn publish_version(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    body: Result<Json<PublishSkillVersionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    let (directory, commit_workspace) = match space {
        SkillSpace::Mine => {
            let directory = state.skill_paths.user_skills_dir.join(&slug);
            (directory.clone(), directory)
        }
        SkillSpace::TjuaeHub if state.can_write_tjuae_hub => {
            let hub = state
                .skill_paths
                .tjuae_hub_worktree_dir
                .as_ref()
                .ok_or_else(|| ExtensionError::InvalidRequest("未配置 TjuaeHub 开发工作副本".into()))?;
            (hub.join("skills").join(&slug), hub.clone())
        }
        _ => return Err(ExtensionError::InvalidRequest("这个技能来源是只读的".into()).into()),
    };
    if !directory.is_dir() {
        return Err(ExtensionError::SkillNotFound(slug).into());
    }
    let (skill, commit) = publish_skill_version(
        &directory,
        &commit_workspace,
        &request.version,
        &request.message,
        state.git.clone(),
    )
    .await?;
    if space == SkillSpace::TjuaeHub {
        let mut command = CommandBuilder::clean_cli("node");
        command
            .current_dir(&commit_workspace)
            .env("TJUAE_SOURCE_REVISION", &commit)
            .arg(".github/scripts/build-assets.js");
        let output = tokio::time::timeout(std::time::Duration::from_secs(120), command.output())
            .await
            .map_err(|_| ExtensionError::Internal("TjuaeHub 索引生成超时".to_owned()))?
            .map_err(|error| ExtensionError::Internal(format!("无法生成 TjuaeHub 索引：{error}")))?;
        if !output.status.success() {
            return Err(
                ExtensionError::InvalidRequest(String::from_utf8_lossy(&output.stderr).trim().to_owned()).into(),
            );
        }
        state
            .git
            .commit_workspace_snapshot(&commit_workspace, "chore(hub): 更新技能目录索引")
            .await
            .map_err(ExtensionError::Internal)?;
        state
            .git
            .push_workspace(&commit_workspace)
            .await
            .map_err(ExtensionError::Internal)?;
    }
    Ok(Json(ApiResponse::ok(SkillOperationResponse {
        identity: identity(source_response(space), namespace, skill.slug),
        version: skill.version,
    })))
}

async fn compare_skill_versions(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    Query(query): Query<CompareSkillVersionsQuery>,
) -> Result<Json<ApiResponse<SkillVersionComparisonResponse>>, ApiError> {
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    let comparison = crate::compare_catalog_versions(
        &state.skill_paths.user_skills_dir,
        space,
        &namespace,
        &slug,
        &query.base,
        &query.target,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(SkillVersionComparisonResponse {
        identity: identity(source_response(space), namespace, slug),
        base_version: query.base,
        target_version: query.target,
        files: comparison
            .into_iter()
            .map(|file| SkillVersionFileDiffResponse {
                path: file.path,
                status: file.status,
                binary: file.binary,
                base_content: file.base_content,
                target_content: file.target_content,
            })
            .collect(),
    })))
}

async fn update_preferences(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    body: Result<Json<UpdateSkillPreferencesRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillPreferencesResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    if request.auto_inject && !request.enabled {
        return Err(ExtensionError::InvalidRequest("自动注入的技能必须先启用".into()).into());
    }
    let existing = state
        .preferences
        .get(&source, &namespace, &slug)
        .await
        .map_err(ExtensionError::from)?;
    let mut selected_version = request
        .selected_version
        .clone()
        .or_else(|| existing.as_ref().and_then(|value| value.selected_version.clone()));
    // Disabling and assistant-injection changes must remain available while a
    // remote Hub is offline. An explicit version is validated by the runtime
    // snapshot operation only when it is actually enabled. `followLatest`
    // deliberately refreshes provider metadata before enabling.
    if request.enabled && (request.follow_latest || selected_version.is_none()) {
        let detail = crate::catalog_detail(
            &state.skill_paths.user_skills_dir,
            space,
            &namespace,
            &slug,
            request.selected_version.as_deref(),
            state.git.clone(),
        )
        .await?;
        selected_version = request
            .selected_version
            .clone()
            .or_else(|| detail.versions.first().cloned())
            .or_else(|| detail.skill.version.clone());
    }
    if request.enabled {
        let selected_version = selected_version
            .as_deref()
            .ok_or_else(|| ExtensionError::InvalidVersion {
                version: String::new(),
                reason: "该来源没有可用版本".to_owned(),
            })?;
        crate::ensure_runtime_snapshot(
            &state.skill_paths.user_skills_dir,
            &state.skill_paths.runtime_cache_dir,
            space,
            &namespace,
            &slug,
            selected_version,
            state.git.clone(),
        )
        .await?;
    }
    let preference = state
        .preferences
        .upsert(UpsertSkillUserPreferenceParams {
            source: &source,
            namespace: &namespace,
            slug: &slug,
            selected_version: selected_version.as_deref(),
            follow_latest: request.follow_latest,
            enabled: request.enabled,
            auto_inject: request.auto_inject,
        })
        .await
        .map_err(ExtensionError::from)?;
    Ok(Json(ApiResponse::ok(preference_response(Some(&preference)))))
}

async fn copy_to_mine(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    body: Result<Json<CopySkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    if space == SkillSpace::Mine {
        return Err(ExtensionError::InvalidRequest("该技能已在“我的技能”中".into()).into());
    }
    let copied = crate::copy_catalog_version_to_mine(
        &state.skill_paths.user_skills_dir,
        space,
        &namespace,
        &slug,
        &request.version,
        &request.target_slug,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(operation_for_mine(&copied.slug, &copied.version))))
}

async fn export_skill(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    body: Result<Json<ExportSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<bool>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let space = SkillSpace::parse(&source)?;
    let namespace = route_namespace(&namespace);
    let snapshot = crate::ensure_runtime_snapshot(
        &state.skill_paths.user_skills_dir,
        &state.skill_paths.runtime_cache_dir,
        space,
        &namespace,
        &slug,
        &request.version,
        state.git.clone(),
    )
    .await?;
    crate::export_skill_directory_archive(&snapshot, Path::new(&request.output_path)).await?;
    Ok(Json(ApiResponse::ok(true)))
}

async fn publish_to_tjuae_hub(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
    body: Result<Json<CopySkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    if SkillSpace::parse(&source)? != SkillSpace::Mine || !state.can_write_tjuae_hub {
        return Err(ExtensionError::InvalidRequest("当前用户没有 TjuaeHub 开发权限".into()).into());
    }
    let _ = route_namespace(&namespace);
    let hub = state
        .skill_paths
        .tjuae_hub_worktree_dir
        .as_ref()
        .ok_or_else(|| ExtensionError::InvalidRequest("未配置 TjuaeHub 开发工作副本".into()))?;
    let published = crate::publish_mine_to_tjuae_hub(
        &state.skill_paths.user_skills_dir,
        hub,
        &slug,
        &request.version,
        &request.target_slug,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(SkillOperationResponse {
        identity: identity(SkillSourceResponse::TjuaeHub, "official".into(), published.slug),
        version: published.version,
    })))
}

async fn import_skill(
    State(state): State<SkillRouterState>,
    body: Result<Json<ImportSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let skill = crate::import_skill_archive(
        &state.skill_paths.user_skills_dir,
        Path::new(&request.archive_path),
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(operation_for_mine(&skill.slug, &skill.version))))
}

async fn create_skill(
    State(state): State<SkillRouterState>,
    body: Result<Json<CreateSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SkillOperationResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let skill = crate::create_skill(
        &state.skill_paths.user_skills_dir,
        &request.slug,
        &request.name,
        &request.description,
        state.git.clone(),
    )
    .await?;
    Ok(Json(ApiResponse::ok(operation_for_mine(&skill.slug, &skill.version))))
}

async fn delete_skill(
    State(state): State<SkillRouterState>,
    AxumPath((source, namespace, slug)): AxumPath<(String, String, String)>,
) -> Result<Json<ApiResponse<bool>>, ApiError> {
    let namespace = route_namespace(&namespace);
    if SkillSpace::parse(&source)? != SkillSpace::Mine {
        return Err(ExtensionError::InvalidRequest("远程来源是只读目录，不能从本机删除".into()).into());
    }
    crate::delete_installed_skill(&state.skill_paths.user_skills_dir, &slug).await?;
    state
        .preferences
        .delete(&source, &namespace, &slug)
        .await
        .map_err(ExtensionError::from)?;
    Ok(Json(ApiResponse::ok(true)))
}

fn detail_response(
    detail: CatalogDetail,
    preference: Option<&SkillUserPreferenceRow>,
    requested_version: Option<&str>,
    can_write_hub: bool,
) -> Result<SkillCatalogDetailResponse, ExtensionError> {
    let versions = detail
        .versions
        .iter()
        .map(|version| SkillVersionResponse {
            version: version.clone(),
            content_hash: None,
            published_at: None,
        })
        .collect::<Vec<_>>();
    let selected = requested_version
        .or(preference.and_then(|value| value.selected_version.as_deref()))
        .or(detail.skill.version.as_deref())
        .unwrap_or_else(|| detail.versions.first().map(String::as_str).unwrap_or("0.0.0"))
        .to_owned();
    if !detail.versions.is_empty() && !detail.versions.iter().any(|version| version == &selected) {
        return Err(ExtensionError::InvalidVersion {
            version: selected,
            reason: "该来源没有这个版本".into(),
        });
    }
    Ok(SkillCatalogDetailResponse {
        skill: catalog_item_response(detail.skill, preference, can_write_hub),
        selected_version: selected,
        versions,
        files: detail
            .files
            .into_iter()
            .map(|file| SkillFileResponse {
                path: file.path,
                size: file.size,
                sha256: file.sha256,
            })
            .collect(),
        readme: detail.readme,
    })
}

fn catalog_item_response(
    skill: CatalogSkill,
    preference: Option<&SkillUserPreferenceRow>,
    can_write_hub: bool,
) -> SkillCatalogItemResponse {
    let identity = identity_for(&skill);
    let source = identity.source;
    SkillCatalogItemResponse {
        identity,
        name: skill.name,
        description: skill.description,
        latest_version: skill.version.unwrap_or_else(|| "0.0.0".into()),
        categories: skill.categories,
        icon_url: skill.icon_url,
        author: skill.author,
        preferences: preference_response(preference),
        editable: source == SkillSourceResponse::Mine || (source == SkillSourceResponse::TjuaeHub && can_write_hub),
        can_copy_to_mine: source != SkillSourceResponse::Mine,
        can_publish_to_tjuae_hub: source == SkillSourceResponse::Mine && can_write_hub,
    }
}

fn identity_for(skill: &CatalogSkill) -> SkillIdentityResponse {
    let source = source_response(skill.space);
    identity(source, skill.namespace.clone(), skill.slug.clone())
}

fn identity(source: SkillSourceResponse, namespace: String, slug: String) -> SkillIdentityResponse {
    SkillIdentityResponse {
        source,
        namespace,
        slug,
    }
}

fn source_id(source: SkillSourceResponse) -> &'static str {
    match source {
        SkillSourceResponse::Mine => "mine",
        SkillSourceResponse::TjuaeHub => "tjuae-hub",
        SkillSourceResponse::SkillHub => "skillhub",
        SkillSourceResponse::ClawHub => "clawhub",
    }
}

fn source_response(space: SkillSpace) -> SkillSourceResponse {
    match space {
        SkillSpace::Mine => SkillSourceResponse::Mine,
        SkillSpace::TjuaeHub => SkillSourceResponse::TjuaeHub,
        SkillSpace::SkillHub => SkillSourceResponse::SkillHub,
        SkillSpace::ClawHub => SkillSourceResponse::ClawHub,
    }
}

fn preference_response(row: Option<&SkillUserPreferenceRow>) -> SkillPreferencesResponse {
    row.map(|row| SkillPreferencesResponse {
        selected_version: row.selected_version.clone(),
        follow_latest: row.follow_latest,
        enabled: row.enabled,
        auto_inject: row.auto_inject,
    })
    .unwrap_or_default()
}

fn preference_key(source: &str, namespace: &str, slug: &str) -> String {
    format!("{source}\u{1f}{namespace}\u{1f}{slug}")
}

fn route_namespace(namespace: &str) -> String {
    if namespace == "~" {
        String::new()
    } else {
        namespace.to_owned()
    }
}

fn version_is_editable(requested: Option<&str>, workspace_version: &str) -> bool {
    requested.is_none_or(|version| version == workspace_version)
}

fn parse_sources(value: &str) -> Result<Vec<SkillSpace>, ExtensionError> {
    if value.trim().is_empty() {
        return Ok(vec![
            SkillSpace::Mine,
            SkillSpace::TjuaeHub,
            SkillSpace::SkillHub,
            SkillSpace::ClawHub,
        ]);
    }
    value.split(',').map(|item| SkillSpace::parse(item.trim())).collect()
}

fn matches_filters(item: &SkillCatalogItemResponse, query: &SkillCatalogQuery) -> bool {
    if query.enabled.is_some_and(|enabled| item.preferences.enabled != enabled)
        || query
            .auto_inject
            .is_some_and(|enabled| item.preferences.auto_inject != enabled)
    {
        return false;
    }
    let categories = query
        .categories
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    categories
        .iter()
        .all(|value| item.categories.iter().any(|item| item == value))
}

fn safe_local_file(root: &Path, slug: &str, relative: &str) -> Result<std::path::PathBuf, ExtensionError> {
    if relative.is_empty()
        || relative.contains('\\')
        || Path::new(relative).is_absolute()
        || Path::new(relative)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ExtensionError::PathTraversal(relative.into()));
    }
    let workspace = std::fs::canonicalize(root.join(slug))?;
    let target = workspace.join(relative);
    if !target.starts_with(&workspace) {
        return Err(ExtensionError::PathTraversal(relative.into()));
    }
    Ok(target)
}

fn operation_for_mine(slug: &str, version: &str) -> SkillOperationResponse {
    SkillOperationResponse {
        identity: identity(SkillSourceResponse::Mine, "local".into(), slug.into()),
        version: version.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_filter_is_explicit_and_comparison_route_has_one_identity() {
        assert_eq!(
            parse_sources("mine,skillhub").unwrap(),
            vec![SkillSpace::Mine, SkillSpace::SkillHub]
        );
        assert!(parse_sources("unknown").is_err());
        assert_eq!(
            preference_key("skillhub", "alice", "writer"),
            "skillhub\u{1f}alice\u{1f}writer"
        );
    }

    #[test]
    fn only_the_current_hub_worktree_version_is_editable() {
        assert!(version_is_editable(None, "2.0.0"));
        assert!(version_is_editable(Some("2.0.0"), "2.0.0"));
        assert!(!version_is_editable(Some("1.0.0"), "2.0.0"));
    }

    #[test]
    fn detail_defaults_to_the_provider_latest_version_not_list_order() {
        let detail = CatalogDetail {
            skill: CatalogSkill {
                id: "skillhub:owner/demo".into(),
                space: SkillSpace::SkillHub,
                slug: "demo".into(),
                namespace: "owner".into(),
                name: "Demo".into(),
                description: String::new(),
                version: Some("1.0.3".into()),
                categories: vec![],
                icon_url: None,
                author: None,
            },
            readme: String::new(),
            files: vec![],
            versions: vec!["1.0.1".into(), "1.0.3".into(), "1.0.2".into()],
            security_reports: vec![],
        };

        let response = detail_response(detail, None, None, false).unwrap();

        assert_eq!(response.selected_version, "1.0.3");
    }
}
