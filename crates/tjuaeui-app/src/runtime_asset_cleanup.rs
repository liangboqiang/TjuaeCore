use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tjuaeui_asset::{SkillPaths, is_projection_runtime_id};
use tjuaeui_common::now_ms;
use uuid::Uuid;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeAssetCleanupReport {
    pub assistants: usize,
    pub skills: usize,
    pub engines: usize,
    pub mcps: usize,
}

#[derive(Debug)]
struct QuarantinedDirectory {
    original: PathBuf,
    quarantined: PathBuf,
}

/// 清理没有 live RuntimeBinding 的严格托管投影。
///
/// portable runtimeId 绝不参与匹配。只有同时满足 projection ID 语法、类型专属
/// owner marker 和路径约束的旧表行才会失效；builtin 或无法证明所有权的行保持原样。
pub(crate) async fn cleanup_orphaned_runtime_asset_projections(
    pool: &SqlitePool,
    skill_paths: &SkillPaths,
    data_dir: &Path,
) -> Result<RuntimeAssetCleanupReport> {
    let live_projection_ids = sqlx::query_scalar::<_, String>(
        "SELECT binding.projection_runtime_id
         FROM asset_runtime_bindings binding
         INNER JOIN asset_runtime_states state
           ON state.user_id = binding.user_id
          AND state.asset_id = binding.asset_id
          AND state.asset_owner_id = binding.asset_owner_id
         WHERE state.state = 'active'",
    )
    .fetch_all(pool)
    .await
    .context("无法读取 live 资产运行绑定")?
    .into_iter()
    .filter(|value| is_projection_runtime_id(value))
    .collect::<BTreeSet<_>>();

    let assistant_rows = sqlx::query_as::<_, (String, String, String, String, Option<String>)>(
        "SELECT id, assistant_id, source, owner_type, source_ref
         FROM assistant_definitions",
    )
    .fetch_all(pool)
    .await
    .context("无法扫描助手运行投影")?;
    let orphan_assistants = assistant_rows
        .into_iter()
        .filter_map(|(id, assistant_id, source, owner_type, source_ref)| {
            managed_assistant_projection_id(&assistant_id, &source, &owner_type, source_ref.as_deref())
                .filter(|projection_id| !live_projection_ids.contains(projection_id))
                .map(|projection_id| (id, projection_id))
        })
        .collect::<Vec<_>>();

    let skill_rows = sqlx::query_as::<_, (String, String, String, String)>("SELECT id, name, path, source FROM skills")
        .fetch_all(pool)
        .await
        .context("无法扫描技能运行投影")?;
    let orphan_skills = skill_rows
        .into_iter()
        .filter_map(|(id, name, path, source)| {
            managed_skill_projection_id(&name, &path, &source, &skill_paths.user_skills_dir)
                .filter(|projection_id| !live_projection_ids.contains(projection_id))
                .map(|projection_id| (id, projection_id, PathBuf::from(path)))
        })
        .collect::<Vec<_>>();

    let engine_rows = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT id, agent_type, agent_source, agent_source_info FROM agent_metadata",
    )
    .fetch_all(pool)
    .await
    .context("无法扫描引擎运行投影")?;
    let orphan_engines = engine_rows
        .into_iter()
        .filter_map(|(id, agent_type, agent_source, source_info)| {
            managed_engine_projection_id(&id, &agent_type, &agent_source, source_info.as_deref())
                .filter(|projection_id| !live_projection_ids.contains(projection_id))
        })
        .collect::<Vec<_>>();

    let mcp_rows = sqlx::query_as::<_, (String, String, bool, Option<String>)>(
        "SELECT id, name, builtin, original_json FROM mcp_servers",
    )
    .fetch_all(pool)
    .await
    .context("无法扫描 MCP 运行投影")?;
    let orphan_mcps = mcp_rows
        .into_iter()
        .filter_map(|(id, name, builtin, original_json)| {
            managed_mcp_projection_id(&id, &name, builtin, original_json.as_deref())
                .filter(|projection_id| !live_projection_ids.contains(projection_id))
                .map(|_| id)
        })
        .collect::<Vec<_>>();

    let quarantine_root = data_dir.join(".runtime-projection-orphan-quarantine");
    let mut quarantined = Vec::new();
    for (_, _, path) in &orphan_skills {
        if std::fs::symlink_metadata(path).is_err() {
            continue;
        }
        std::fs::create_dir_all(&quarantine_root)
            .with_context(|| format!("无法创建运行投影隔离目录：{}", quarantine_root.display()))?;
        let quarantined_path = quarantine_root.join(Uuid::now_v7().to_string());
        if let Err(error) = std::fs::rename(path, &quarantined_path) {
            restore_quarantined_directories(&quarantined);
            return Err(error).with_context(|| format!("无法隔离孤儿技能目录：{}", path.display()));
        }
        quarantined.push(QuarantinedDirectory {
            original: path.clone(),
            quarantined: quarantined_path,
        });
    }

    let cleanup_result = async {
        let mut transaction = pool.begin().await?;
        let timestamp = now_ms();
        for (id, _) in &orphan_assistants {
            sqlx::query(
                "UPDATE assistant_definitions
                 SET deleted_at = COALESCE(deleted_at, ?), updated_at = ?
                 WHERE id = ?",
            )
            .bind(timestamp)
            .bind(timestamp)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        }
        for (id, _, _) in &orphan_skills {
            sqlx::query(
                "UPDATE skills
                 SET enabled = 0, deleted_at = COALESCE(deleted_at, ?), updated_at = ?
                 WHERE id = ?",
            )
            .bind(timestamp)
            .bind(timestamp)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        }
        for id in &orphan_engines {
            sqlx::query("DELETE FROM agent_metadata WHERE id = ?")
                .bind(id)
                .execute(&mut *transaction)
                .await?;
        }
        for id in &orphan_mcps {
            sqlx::query(
                "UPDATE mcp_servers
                 SET enabled = 0, deleted_at = COALESCE(deleted_at, ?), updated_at = ?
                 WHERE id = ?",
            )
            .bind(timestamp)
            .bind(timestamp)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await
    }
    .await;
    if let Err(error) = cleanup_result {
        restore_quarantined_directories(&quarantined);
        return Err(error).context("提交孤儿运行投影清理失败");
    }

    for directory in &quarantined {
        if let Err(error) = remove_quarantined_directory(&directory.quarantined) {
            tracing::warn!(
                path = %directory.quarantined.display(),
                error = %error,
                "孤儿技能投影已从运行路径隔离，但隔离目录暂未删除"
            );
        }
    }
    let _ = std::fs::remove_dir(&quarantine_root);
    for (_, projection_id) in &orphan_assistants {
        remove_assistant_projection_files(data_dir, projection_id);
    }

    let report = RuntimeAssetCleanupReport {
        assistants: orphan_assistants.len(),
        skills: orphan_skills.len(),
        engines: orphan_engines.len(),
        mcps: orphan_mcps.len(),
    };
    if report != RuntimeAssetCleanupReport::default() {
        tracing::info!(
            assistants = report.assistants,
            skills = report.skills,
            engines = report.engines,
            mcps = report.mcps,
            "已清理没有 live RuntimeBinding 的严格托管运行投影"
        );
    }
    Ok(report)
}

fn managed_assistant_projection_id(
    assistant_id: &str,
    source: &str,
    owner_type: &str,
    source_ref: Option<&str>,
) -> Option<String> {
    if source != "user" || owner_type != "user" {
        return None;
    }
    let source_ref = source_ref?;
    let projection_id = source_ref
        .strip_prefix("asset:")
        .or_else(|| source_ref.strip_prefix("market:"))?;
    (assistant_id == projection_id && is_projection_runtime_id(projection_id)).then(|| projection_id.to_owned())
}

fn managed_skill_projection_id(name: &str, path: &str, source: &str, user_skills_dir: &Path) -> Option<String> {
    if source != "user" || !is_projection_runtime_id(name) {
        return None;
    }
    (Path::new(path) == user_skills_dir.join(name)).then(|| name.to_owned())
}

fn managed_engine_projection_id(
    id: &str,
    agent_type: &str,
    agent_source: &str,
    source_info: Option<&str>,
) -> Option<String> {
    if !is_projection_runtime_id(id) || agent_type != "acp" || !matches!(agent_source, "asset" | "extension") {
        return None;
    }
    let value: Value = serde_json::from_str(source_info?).ok()?;
    let object = value.as_object()?;
    let local_asset_id = object.get("tjuaeLocalAssetId")?.as_str()?.trim();
    let binary_name = object.get("binary_name")?.as_str()?.trim();
    let owner = object.get("hub_package_id")?.as_str()?;
    (!local_asset_id.is_empty() && !binary_name.is_empty() && owner == format!("asset:{}", stable_identity(id)))
        .then(|| id.to_owned())
}

fn managed_mcp_projection_id(id: &str, name: &str, builtin: bool, original_json: Option<&str>) -> Option<String> {
    if builtin || !is_projection_runtime_id(name) || id != stable_identity(name) {
        return None;
    }
    let value: Value = serde_json::from_str(original_json?).ok()?;
    let root = value.as_object()?;
    if root.len() != 1 {
        return None;
    }
    let marker = root.get("$tjuaeAsset")?.as_object()?;
    if marker.len() != 3
        || marker.get("id")?.as_str()? != id
        || marker.get("kind")?.as_str()? != "mcp"
        || marker.get("tjuaeLocalAssetId")?.as_str()?.trim().is_empty()
    {
        return None;
    }
    Some(name.to_owned())
}

fn stable_identity(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))[..24].to_owned()
}

fn restore_quarantined_directories(directories: &[QuarantinedDirectory]) {
    for directory in directories.iter().rev() {
        if let Err(error) = std::fs::rename(&directory.quarantined, &directory.original) {
            tracing::error!(
                original = %directory.original.display(),
                quarantined = %directory.quarantined.display(),
                error = %error,
                "无法恢复未提交的孤儿技能目录隔离"
            );
        }
    }
}

fn remove_quarantined_directory(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path))
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn remove_assistant_projection_files(data_dir: &Path, projection_id: &str) {
    let prefix = format!("{projection_id}.");
    for directory in [data_dir.join("assistant-rules"), data_dir.join("assistant-avatars")] {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(&prefix) {
                continue;
            }
            if let Err(error) = std::fs::remove_file(entry.path()) {
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %error,
                    "孤儿助手投影文件暂未删除"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjuaeui_db::{
        IAssistantDefinitionRepository, SqliteAssistantDefinitionRepository, UpsertAssistantDefinitionParams,
        init_database_memory,
    };

    fn projection(hex_digit: char) -> String {
        format!("tjuae-proj-v1-{}", hex_digit.to_string().repeat(64))
    }

    #[test]
    fn managed_markers_are_strict_and_do_not_accept_builtin_lookalikes() {
        let id = projection('a');
        assert_eq!(
            managed_assistant_projection_id(&id, "user", "user", Some(&format!("asset:{id}"))),
            Some(id.clone())
        );
        assert!(managed_assistant_projection_id(&id, "builtin", "system", Some(&format!("asset:{id}"))).is_none());

        let info = serde_json::json!({
            "binary_name": "adapter",
            "hub_package_id": format!("asset:{}", stable_identity(&id)),
            "tjuaeLocalAssetId": "engine:local"
        })
        .to_string();
        assert_eq!(
            managed_engine_projection_id(&id, "acp", "asset", Some(&info)),
            Some(id.clone())
        );
        assert!(managed_engine_projection_id(&id, "acp", "builtin", Some(&info)).is_none());

        let mcp_id = stable_identity(&id);
        let marker = serde_json::json!({
            "$tjuaeAsset": {
                "id": mcp_id,
                "kind": "mcp",
                "tjuaeLocalAssetId": "mcp:local"
            }
        })
        .to_string();
        assert_eq!(
            managed_mcp_projection_id(&stable_identity(&id), &id, false, Some(&marker)),
            Some(id.clone())
        );
        assert!(managed_mcp_projection_id(&stable_identity(&id), &id, true, Some(&marker)).is_none());
    }

    async fn insert_assistant(
        repo: &SqliteAssistantDefinitionRepository,
        id: &str,
        source: &str,
        owner_type: &str,
        source_ref: &str,
    ) {
        let definition_id = format!("definition-{}", stable_identity(id));
        repo.upsert(&UpsertAssistantDefinitionParams {
            id: &definition_id,
            assistant_id: id,
            source,
            owner_type,
            source_ref: Some(source_ref),
            name: id,
            name_i18n: "{}",
            description: None,
            description_i18n: "{}",
            avatar_type: "none",
            avatar_value: None,
            agent_id: "632f31d2",
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
            default_skills_mode: "auto",
            default_skill_ids: "[]",
            custom_skill_names: "[]",
            default_mcps_mode: "auto",
            default_mcp_ids: "[]",
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn startup_cleanup_removes_only_strict_orphans_and_preserves_live_and_builtin_rows() {
        let database = init_database_memory().await.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let skill_paths = tjuaeui_asset::resolve_skill_paths(temp.path(), temp.path());
        std::fs::create_dir_all(&skill_paths.user_skills_dir).unwrap();

        let orphan = projection('a');
        let live = projection('b');
        let builtin = projection('c');
        for id in [&orphan, &live, &builtin] {
            let path = skill_paths.user_skills_dir.join(id);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), "# skill").unwrap();
        }
        for (id, source) in [(&orphan, "user"), (&live, "user"), (&builtin, "builtin")] {
            sqlx::query(
                "INSERT INTO skills
                    (id, name, description, path, source, enabled, created_at, updated_at)
                 VALUES (?, ?, 'test', ?, ?, 1, 1, 1)",
            )
            .bind(format!("skill-row-{}", stable_identity(id)))
            .bind(id)
            .bind(skill_paths.user_skills_dir.join(id).to_string_lossy().into_owned())
            .bind(source)
            .execute(database.pool())
            .await
            .unwrap();
        }

        sqlx::query(
            "INSERT INTO asset_records (
                user_id, id, kind, display_name, origin, trust, scope, editability,
                workspace_key, definition_digest, entry_file, runtime_id, created_at, updated_at
             ) VALUES (
                'system_default_user', 'live-skill', 'skill', 'Live', 'local', 'official',
                'user', 'full', 'workspace/live', 'sha256-live', 'SKILL.md',
                'portable-live', 1, 1
             )",
        )
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO asset_runtime_states
                (user_id, asset_owner_id, asset_id, state, updated_at)
             VALUES
                ('system_default_user', 'system_default_user', 'live-skill', 'active', 1)",
        )
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO asset_try_run_receipts (
                user_id, asset_owner_id, asset_id, receipt_id, idempotency_key,
                definition_digest, overlay_version, portable_runtime_id,
                projection_runtime_id, created_at
             ) VALUES (
                'system_default_user', 'system_default_user', 'live-skill',
                'receipt-live', 'try-live', 'sha256-live', 0, 'portable-live', ?, 1
             )",
        )
        .bind(&live)
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO asset_runtime_bindings (
                user_id, asset_owner_id, asset_id, kind, projection_kind,
                portable_runtime_id, projection_runtime_id, definition_digest,
                overlay_version, health_status, try_run_receipt_id, projected_at
             ) VALUES (
                'system_default_user', 'system_default_user', 'live-skill', 'skill', 'skill',
                'portable-live', ?, 'sha256-live', 0, 'healthy', 'receipt-live', 1
             )",
        )
        .bind(&live)
        .execute(database.pool())
        .await
        .unwrap();

        let assistant_repo = SqliteAssistantDefinitionRepository::new(database.pool().clone());
        insert_assistant(&assistant_repo, &orphan, "user", "user", &format!("asset:{orphan}")).await;
        insert_assistant(
            &assistant_repo,
            &builtin,
            "builtin",
            "system",
            &format!("asset:{builtin}"),
        )
        .await;

        let orphan_engine_info = serde_json::json!({
            "binary_name": "orphan-adapter",
            "hub_package_id": format!("asset:{}", stable_identity(&orphan)),
            "tjuaeLocalAssetId": "engine:orphan"
        })
        .to_string();
        let builtin_engine_info = serde_json::json!({
            "binary_name": "builtin-adapter",
            "hub_package_id": format!("asset:{}", stable_identity(&builtin)),
            "tjuaeLocalAssetId": "engine:builtin"
        })
        .to_string();
        for (id, source, info) in [
            (&orphan, "asset", &orphan_engine_info),
            (&builtin, "builtin", &builtin_engine_info),
        ] {
            sqlx::query(
                "INSERT INTO agent_metadata (
                    id, name, agent_type, agent_source, agent_source_info,
                    enabled, sort_order, created_at, updated_at
                 ) VALUES (?, ?, 'acp', ?, ?, 1, 1, 1, 1)",
            )
            .bind(id)
            .bind(id)
            .bind(source)
            .bind(info)
            .execute(database.pool())
            .await
            .unwrap();
        }

        for (name, is_builtin) in [(&orphan, false), (&builtin, true)] {
            let id = stable_identity(name);
            let marker = serde_json::json!({
                "$tjuaeAsset": {
                    "id": id,
                    "kind": "mcp",
                    "tjuaeLocalAssetId": format!("mcp:{name}")
                }
            })
            .to_string();
            sqlx::query(
                "INSERT INTO mcp_servers (
                    id, name, enabled, transport_type, transport_config,
                    original_json, builtin, created_at, updated_at
                 ) VALUES (?, ?, 1, 'stdio', '{}', ?, ?, 1, 1)",
            )
            .bind(stable_identity(name))
            .bind(name)
            .bind(marker)
            .bind(is_builtin)
            .execute(database.pool())
            .await
            .unwrap();
        }

        let report = cleanup_orphaned_runtime_asset_projections(database.pool(), &skill_paths, temp.path())
            .await
            .unwrap();
        assert_eq!(
            report,
            RuntimeAssetCleanupReport {
                assistants: 1,
                skills: 1,
                engines: 1,
                mcps: 1,
            }
        );
        assert!(!skill_paths.user_skills_dir.join(&orphan).exists());
        assert!(skill_paths.user_skills_dir.join(&live).is_dir());
        assert!(skill_paths.user_skills_dir.join(&builtin).is_dir());

        let orphan_skill_deleted: Option<i64> = sqlx::query_scalar("SELECT deleted_at FROM skills WHERE name = ?")
            .bind(&orphan)
            .fetch_one(database.pool())
            .await
            .unwrap();
        let live_skill_deleted: Option<i64> = sqlx::query_scalar("SELECT deleted_at FROM skills WHERE name = ?")
            .bind(&live)
            .fetch_one(database.pool())
            .await
            .unwrap();
        let builtin_skill_deleted: Option<i64> = sqlx::query_scalar("SELECT deleted_at FROM skills WHERE name = ?")
            .bind(&builtin)
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert!(orphan_skill_deleted.is_some());
        assert!(live_skill_deleted.is_none());
        assert!(builtin_skill_deleted.is_none());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_metadata WHERE id = ?")
                .bind(&orphan)
                .fetch_one(database.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM agent_metadata WHERE id = ?")
                .bind(&builtin)
                .fetch_one(database.pool())
                .await
                .unwrap(),
            1
        );
        assert!(
            sqlx::query_scalar::<_, Option<i64>>("SELECT deleted_at FROM mcp_servers WHERE name = ?")
                .bind(&orphan)
                .fetch_one(database.pool())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            sqlx::query_scalar::<_, Option<i64>>("SELECT deleted_at FROM mcp_servers WHERE name = ?")
                .bind(&builtin)
                .fetch_one(database.pool())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            assistant_repo
                .get_by_assistant_id_including_deleted(&orphan)
                .await
                .unwrap()
                .unwrap()
                .deleted_at
                .is_some()
        );
        assert!(
            assistant_repo
                .get_by_assistant_id_including_deleted(&builtin)
                .await
                .unwrap()
                .unwrap()
                .deleted_at
                .is_none()
        );
    }
}
