use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use tjuaeui_api_types::{AssetEditability, AssetKind, AssetOrigin, AssetRuntimeCommandRequest, AssetScope, AssetTrust};
use tjuaeui_asset::{
    AssetCatalogService, AssetDefinitionFile, AssetError, AssetRuntimeProjector, LocalAssetInput,
    RuntimeAssetDefinition, RuntimeProjectionTransaction, TrackedAssetInput, prepare_definition,
};
use tjuaeui_db::{SqliteAssetRepository, SqliteAssistantDefinitionRepository};

use super::*;
use crate::skill_resolver::{ResolvedRuntimeSkill, RuntimeSkillResolutionError};

const TRACKED_PACKAGE: &str = "tjuae/assistant-provenance";
const TRACKED_REMOTE_ASSISTANT_ID: &str = "org.tjuae.assistant.provenance";
const TRACKED_VERSION: &str = "2.3.4";
const TRACKED_REVISION: &str = "ffffffffffffffffffffffffffffffffffffffff";

struct PersistOnlyRuntimeProjector;
struct PersistOnlyTransaction;

#[async_trait::async_trait]
impl AssetRuntimeProjector for PersistOnlyRuntimeProjector {
    async fn validate(&self, _user_id: &str, _assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError> {
        Ok(())
    }

    async fn try_run(&self, _user_id: &str, _assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError> {
        Ok(())
    }

    async fn prepare_replace(
        &self,
        _user_id: &str,
        _assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        Ok(Box::new(PersistOnlyTransaction))
    }

    async fn prepare_remove(
        &self,
        _user_id: &str,
        _assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        Ok(Box::new(PersistOnlyTransaction))
    }
}

#[async_trait::async_trait]
impl RuntimeProjectionTransaction for PersistOnlyTransaction {
    async fn apply(&mut self) -> Result<(), AssetError> {
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), AssetError> {
        Ok(())
    }

    async fn finalize(self: Box<Self>) {}
}

pub(super) struct AssistantRuntimeFixture {
    pub(super) local_asset_id: String,
    pub(super) local_definition_digest: String,
    pub(super) definitions: Arc<SqliteAssistantDefinitionRepository>,
    _data_dir: tempfile::TempDir,
}

struct CatalogFixture {
    catalog: Arc<AssetCatalogService>,
    definitions: Arc<SqliteAssistantDefinitionRepository>,
    data_dir: tempfile::TempDir,
}

async fn catalog_fixture(user_id: &str) -> CatalogFixture {
    let database = init_database_memory().await.unwrap();
    let now = tjuaeui_common::now_ms();
    sqlx::query(
        "INSERT OR IGNORE INTO users (id, username, password_hash, created_at, updated_at)
         VALUES (?, ?, '', ?, ?)",
    )
    .bind(user_id)
    .bind(format!(
        "runtime-provenance-{user_id}-{}",
        tjuaeui_common::generate_short_id()
    ))
    .bind(now)
    .bind(now)
    .execute(database.pool())
    .await
    .unwrap();

    let data_dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
    let catalog = Arc::new(
        AssetCatalogService::new(repository, data_dir.path())
            .with_runtime_projector(Arc::new(PersistOnlyRuntimeProjector)),
    );
    let definitions = Arc::new(SqliteAssistantDefinitionRepository::new(database.pool().clone()));
    CatalogFixture {
        catalog,
        definitions,
        data_dir,
    }
}

async fn upsert_runtime_assistant_definition(
    definitions: &SqliteAssistantDefinitionRepository,
    definition_id: &str,
    assistant_id: &str,
    agent_id: &str,
    source: &str,
    source_ref: &str,
) {
    definitions
        .upsert(&UpsertAssistantDefinitionParams {
            id: definition_id,
            assistant_id,
            source,
            owner_type: "user",
            source_ref: Some(source_ref),
            name: assistant_id,
            name_i18n: "{}",
            description: Some("runtime provenance fixture"),
            description_i18n: "{}",
            avatar_type: "none",
            avatar_value: None,
            agent_id,
            rule_resource_type: "none",
            rule_resource_ref: None,
            recommended_prompts: "[]",
            recommended_prompts_i18n: "{}",
            default_model_mode: "auto",
            default_model_value: None,
            default_permission_mode: "auto",
            default_permission_value: None,
            default_thought_level_mode: "auto",
            default_thought_level_value: None,
            default_skills_mode: "fixed",
            default_skill_ids: "[]",
            custom_skill_names: "[]",
            default_mcps_mode: "auto",
            default_mcp_ids: "[]",
        })
        .await
        .unwrap();
}

async fn wire_assistant_runtime_provenance(
    service: &ConversationService,
    user_id: &str,
    definition_id: &str,
    assistant_id: &str,
    agent_id: &str,
    tracked: bool,
) -> AssistantRuntimeFixture {
    let fixture = catalog_fixture(user_id).await;
    // The public/canonical assistant selector uses the local asset id. The
    // user-scoped projection id is derived separately and must never leak into
    // the API-facing snapshot identity.
    let local_asset_id = assistant_id.to_owned();
    let files = vec![AssetDefinitionFile::text(
        "assistant.md",
        format!("assistant: {assistant_id}\n"),
    )];
    let local = LocalAssetInput {
        id: local_asset_id.clone(),
        kind: AssetKind::Assistant,
        display_name: assistant_id.to_owned(),
        description: Some("runtime provenance fixture".into()),
        origin: if tracked { AssetOrigin::Hub } else { AssetOrigin::Local },
        trust: if tracked {
            AssetTrust::Verified
        } else {
            AssetTrust::Community
        },
        scope: AssetScope::User,
        editability: AssetEditability::Full,
        entry_file: Some("assistant.md".into()),
        runtime_id: Some(assistant_id.to_owned()),
        files: files.clone(),
        dependency_runtime_ids: BTreeMap::new(),
    };
    let local_definition_digest = if tracked {
        let remote_digest = prepare_definition(files).unwrap().1.digest;
        fixture
            .catalog
            .install_tracked(
                user_id,
                &format!("install-{local_asset_id}"),
                TrackedAssetInput {
                    local,
                    package_name: TRACKED_PACKAGE.into(),
                    remote_asset_id: TRACKED_REMOTE_ASSISTANT_ID.into(),
                    version: TRACKED_VERSION.into(),
                    source_revision: TRACKED_REVISION.into(),
                    remote_digest: remote_digest.clone(),
                },
            )
            .await
            .unwrap();
        fixture
            .catalog
            .get(user_id, &local_asset_id)
            .await
            .unwrap()
            .asset
            .definition_digest
    } else {
        fixture
            .catalog
            .register_local(user_id, local)
            .await
            .unwrap()
            .asset
            .definition_digest
    };
    let command = |suffix: &str| AssetRuntimeCommandRequest {
        idempotency_key: format!("runtime-{suffix}-{local_asset_id}"),
        expected_definition_digest: local_definition_digest.clone(),
        expected_overlay_version: None,
    };
    fixture
        .catalog
        .validate_runtime(user_id, &local_asset_id, command("validate"))
        .await
        .unwrap();
    fixture
        .catalog
        .try_run(user_id, &local_asset_id, command("try-run"))
        .await
        .unwrap();
    fixture
        .catalog
        .activate(user_id, &local_asset_id, command("activate"))
        .await
        .unwrap();
    let bound = fixture
        .catalog
        .resolve_bound_runtime_asset(user_id, AssetKind::Assistant, &local_asset_id)
        .await
        .unwrap();
    let source = if tracked { "generated" } else { "user" };
    let source_ref = format!(
        "{}:{}",
        if tracked { "market" } else { "asset" },
        bound.projection_runtime_id
    );
    upsert_runtime_assistant_definition(
        &fixture.definitions,
        definition_id,
        &bound.projection_runtime_id,
        agent_id,
        source,
        &source_ref,
    )
    .await;
    service.with_runtime_asset_catalog(fixture.catalog.clone());
    service.with_assistant_definition_repo(fixture.definitions.clone());

    AssistantRuntimeFixture {
        local_asset_id,
        local_definition_digest,
        definitions: fixture.definitions,
        _data_dir: fixture.data_dir,
    }
}

pub(super) async fn wire_local_assistant_runtime_provenance(
    service: &ConversationService,
    user_id: &str,
    definition_id: &str,
    assistant_id: &str,
    agent_id: &str,
) -> AssistantRuntimeFixture {
    wire_assistant_runtime_provenance(service, user_id, definition_id, assistant_id, agent_id, false).await
}

async fn seed_tjuae_cli_snapshot(
    repo: &Arc<MockRepo>,
    user_id: &str,
    definition_id: &str,
    assistant_id: &str,
    agent_id: &str,
) -> ConversationRow {
    let row = ConversationRow {
        id: format!("runtime-provenance-{}", tjuaeui_common::generate_short_id()),
        user_id: user_id.into(),
        name: "runtime provenance".into(),
        r#type: "tjuaecli".into(),
        extra: json!({ "workspace": ensure_test_workspace_path() }).to_string(),
        model: None,
        status: Some("finished".into()),
        source: Some("tjuaeui".into()),
        channel_chat_id: None,
        pinned: false,
        pinned_at: None,
        created_at: 1,
        updated_at: 1,
        project_id: None,
        folder_id: None,
    };
    repo.create(&row).await.unwrap();
    repo.upsert_assistant_snapshot(&UpsertConversationAssistantSnapshotParams {
        conversation_id: &row.id,
        assistant_definition_id: definition_id,
        assistant_id,
        assistant_source: "user",
        agent_id,
        rules_content: "",
        default_model_mode: "auto",
        resolved_model_id: None,
        default_permission_mode: "auto",
        resolved_permission_value: None,
        default_thought_level_mode: "auto",
        resolved_thought_level_value: None,
        default_skills_mode: "fixed",
        resolved_skill_ids: "[]",
        default_mcps_mode: "auto",
        resolved_mcp_ids: "[]",
    })
    .await
    .unwrap();
    row
}

struct CatalogBackedSkillResolver {
    catalog: Arc<AssetCatalogService>,
}

#[async_trait::async_trait]
impl SkillResolver for CatalogBackedSkillResolver {
    async fn resolve_skills(&self, _names: &[String]) -> Vec<ResolvedAgentSkill> {
        Vec::new()
    }

    async fn resolve_runtime_skills(
        &self,
        user_id: &str,
        references: &[String],
    ) -> Result<Vec<ResolvedRuntimeSkill>, RuntimeSkillResolutionError> {
        let mut resolved = Vec::with_capacity(references.len());
        for reference in references {
            let provenance = self
                .catalog
                .resolve_runtime_provenance(user_id, AssetKind::Skill, reference)
                .await?;
            let workspace_key = self
                .catalog
                .content_store()
                .workspace_key(user_id, &provenance.local_asset_id);
            let source_path = self.catalog.content_store().workspace_path(&workspace_key)?;
            resolved.push(ResolvedRuntimeSkill {
                name: provenance.runtime_id.clone(),
                source_path,
                provenance,
            });
        }
        Ok(resolved)
    }

    async fn link_workspace_skills(
        &self,
        _workspace: &Path,
        _rel_dirs: &[&str],
        _skills: &[ResolvedAgentSkill],
    ) -> usize {
        0
    }
}

fn skill_input(id: &str, runtime_id: &str, content: &str, tracked: bool) -> LocalAssetInput {
    LocalAssetInput {
        id: id.into(),
        kind: AssetKind::Skill,
        display_name: runtime_id.into(),
        description: None,
        origin: if tracked { AssetOrigin::Hub } else { AssetOrigin::Local },
        trust: if tracked {
            AssetTrust::Verified
        } else {
            AssetTrust::Community
        },
        scope: AssetScope::User,
        editability: AssetEditability::Full,
        entry_file: Some("SKILL.md".into()),
        runtime_id: Some(runtime_id.into()),
        files: vec![AssetDefinitionFile::text("SKILL.md", content)],
        dependency_runtime_ids: BTreeMap::new(),
    }
}

async fn seed_skill_conversation(repo: &Arc<MockRepo>, user_id: &str, reference: &str) -> ConversationRow {
    let row = ConversationRow {
        id: format!("skill-runtime-provenance-{}", tjuaeui_common::generate_short_id()),
        user_id: user_id.into(),
        name: "skill runtime provenance".into(),
        r#type: "tjuaecli".into(),
        extra: json!({
            "workspace": ensure_test_workspace_path(),
            "skills": [reference],
        })
        .to_string(),
        model: None,
        status: Some("finished".into()),
        source: Some("tjuaeui".into()),
        channel_chat_id: None,
        pinned: false,
        pinned_at: None,
        created_at: 1,
        updated_at: 1,
        project_id: None,
        folder_id: None,
    };
    repo.create(&row).await.unwrap();
    row
}

#[tokio::test]
async fn tracked_assistant_request_carries_exact_catalog_upstream() {
    let (service, _broadcaster, repo, _task_manager) = make_service();
    let fixture = wire_assistant_runtime_provenance(
        &service,
        "user_1",
        "asstdef-runtime-provenance",
        "assistant-runtime-provenance",
        "tjuaecli",
        true,
    )
    .await;
    let row = seed_tjuae_cli_snapshot(
        &repo,
        "user_1",
        "asstdef-runtime-provenance",
        "assistant-runtime-provenance",
        "tjuaecli",
    )
    .await;

    let options = service.build_task_options(&row).await.unwrap();
    let request = options.runtime_asset_request.as_ref().unwrap();
    let asset = &request.core_assets[0];
    assert_eq!(asset.local_asset_id, fixture.local_asset_id);
    assert_eq!(asset.local_definition_digest, fixture.local_definition_digest);
    assert_ne!(asset.runtime_content_digest, asset.local_definition_digest);
    assert_eq!(asset.upstream_package.as_deref(), Some(TRACKED_PACKAGE));
    assert_eq!(asset.upstream_asset_id.as_deref(), Some(TRACKED_REMOTE_ASSISTANT_ID));
    assert_eq!(asset.upstream_version.as_deref(), Some(TRACKED_VERSION));
    assert_eq!(asset.upstream_revision.as_deref(), Some(TRACKED_REVISION));

    let receipt = core_only_runtime_asset_receipt(request).unwrap();
    assert_eq!(receipt.assets, request.core_assets);
}

#[tokio::test]
async fn local_skill_request_has_catalog_digest_and_no_upstream() {
    let fixture = catalog_fixture("user_1").await;
    let registered = fixture
        .catalog
        .register_local(
            "user_1",
            skill_input(
                "skill-local-provenance",
                "skill-runtime-provenance",
                "# Local provenance\n",
                false,
            ),
        )
        .await
        .unwrap();
    let resolver = Arc::new(CatalogBackedSkillResolver {
        catalog: fixture.catalog.clone(),
    });
    let (service, _broadcaster, repo, _task_manager) = make_service_with_resolver(resolver);
    service.with_runtime_asset_catalog(fixture.catalog.clone());
    let row = seed_skill_conversation(&repo, "user_1", "skill-runtime-provenance").await;

    let options = service.build_task_options(&row).await.unwrap();
    let request = options.runtime_asset_request.as_ref().unwrap();
    let skill = &request.managed_skills[0];
    assert_eq!(skill.asset.local_asset_id, "skill-local-provenance");
    assert_eq!(skill.asset.local_definition_digest, registered.asset.definition_digest);
    assert_ne!(skill.asset.runtime_content_digest, skill.asset.local_definition_digest);
    assert!(skill.asset.upstream_package.is_none());
    assert!(skill.asset.upstream_asset_id.is_none());
    assert!(skill.asset.upstream_version.is_none());
    assert!(skill.asset.upstream_revision.is_none());
    let workspace_key = fixture
        .catalog
        .content_store()
        .workspace_key("user_1", "skill-local-provenance");
    assert_eq!(
        skill.root,
        fixture.catalog.content_store().workspace_path(&workspace_key).unwrap()
    );
}

#[tokio::test]
async fn remote_ids_are_never_guessed_as_local_provenance() {
    let (assistant_service, _broadcaster, assistant_repo, _task_manager) = make_service();
    let assistant_fixture = wire_assistant_runtime_provenance(
        &assistant_service,
        "user_1",
        "asstdef-remote-id",
        "assistant-runtime-remote-id",
        "tjuaecli",
        true,
    )
    .await;
    upsert_runtime_assistant_definition(
        &assistant_fixture.definitions,
        "asstdef-remote-id",
        "assistant-runtime-remote-id",
        "tjuaecli",
        "generated",
        &format!("market:{TRACKED_REMOTE_ASSISTANT_ID}"),
    )
    .await;
    let assistant_row = seed_tjuae_cli_snapshot(
        &assistant_repo,
        "user_1",
        "asstdef-remote-id",
        "assistant-runtime-remote-id",
        "tjuaecli",
    )
    .await;
    let assistant_error = assistant_service.build_task_options(&assistant_row).await.unwrap_err();
    assert!(assistant_error.to_string().contains("助手 Definition 资产来源不合法"));

    let skill_fixture = catalog_fixture("user_1").await;
    let local = skill_input(
        "skill-local-remote-id",
        "skill-runtime-remote-id",
        "# Tracked skill\n",
        true,
    );
    let remote_digest = prepare_definition(local.files.clone()).unwrap().1.digest;
    skill_fixture
        .catalog
        .install_tracked(
            "user_1",
            "install-skill-remote-id",
            TrackedAssetInput {
                local,
                package_name: "tjuae/skill-provenance".into(),
                remote_asset_id: "org.tjuae.skill.provenance".into(),
                version: "1.0.0".into(),
                source_revision: TRACKED_REVISION.into(),
                remote_digest,
            },
        )
        .await
        .unwrap();
    let resolver = Arc::new(CatalogBackedSkillResolver {
        catalog: skill_fixture.catalog.clone(),
    });
    let (skill_service, _broadcaster, skill_repo, _task_manager) = make_service_with_resolver(resolver);
    skill_service.with_runtime_asset_catalog(skill_fixture.catalog.clone());
    let skill_row = seed_skill_conversation(&skill_repo, "user_1", "org.tjuae.skill.provenance").await;
    let skill_error = skill_service.build_task_options(&skill_row).await.unwrap_err();
    assert!(skill_error.to_string().contains("技能资产来源解析失败"));
}
