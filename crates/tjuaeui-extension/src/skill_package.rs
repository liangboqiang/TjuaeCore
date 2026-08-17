//! One skill model: every installed skill is one local workspace.
//!
//! Markets only publish a static index that points at immutable directories in
//! Git repositories. Installing a market entry materializes that directory,
//! verifies it, records the optional market link in the manifest, and then
//! initializes an independent local Git repository. Runtime loading never
//! reads a market, archive, extension, or embedded fallback.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tjuaeui_common::WorkspaceGitProvisioner;
use tokio::sync::RwLock;
use tracing::warn;

use crate::error::ExtensionError;

pub const SKILL_PACKAGE_MANIFEST: &str = ".tjuae-skill.json";
pub const SKILL_ENTRY_FILE: &str = "SKILL.md";
const SKILL_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/tjuae-skill.v1.schema.json";
const MARKET_INDEXES_ENV: &str = "TJUAE_SKILL_MARKET_INDEX_URLS";
const DEFAULT_MARKET_INDEX_URL: &str = "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/dist/skills.json";
const MARKET_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

static MARKET_CACHE: LazyLock<RwLock<HashMap<String, CachedMarketIndex>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, Clone)]
struct CachedMarketIndex {
    fetched_at: SystemTime,
    index: MarketIndex,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillManifest {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub categories: Vec<String>,
    pub enabled: bool,
    pub auto_inject: bool,
    pub source: SkillSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum SkillSource {
    Local,
    Market {
        #[serde(rename = "marketId")]
        market_id: String,
        repository: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillPreferences {
    pub enabled: bool,
    pub auto_inject: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkill {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub path: PathBuf,
    pub source: SkillSource,
    pub categories: Vec<String>,
    pub preferences: SkillPreferences,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketIndex {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub schema_version: u32,
    pub market: MarketInfo,
    pub repository: String,
    pub revision: String,
    pub skills: Vec<MarketSkillEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketSkillEntry {
    pub id: String,
    pub path: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub categories: Vec<String>,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketSyncState {
    NotInstalled,
    Synced,
    LocalChanged,
    UpdateAvailable,
    Diverged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketFileComparison {
    pub path: String,
    pub status: String,
    pub binary: bool,
    pub local_content: Option<String>,
    pub remote_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketSkillComparison {
    pub slug: String,
    pub base_revision: String,
    pub remote_revision: String,
    pub sync_state: MarketSyncState,
    pub files: Vec<MarketFileComparison>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketSkillPublication {
    pub branch: String,
    pub commit: String,
    pub compare_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

pub async fn create_skill(
    root: &Path,
    slug: &str,
    name: &str,
    description: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    validate_slug(slug)?;
    let name = name.trim();
    let description = description.trim();
    if name.is_empty() || description.is_empty() {
        return Err(ExtensionError::InvalidRequest("技能名称和用途说明不能为空".to_owned()));
    }
    let target = root.join(slug);
    if target.exists() {
        return Err(ExtensionError::InvalidRequest(format!("技能 {slug} 已存在")));
    }
    tokio::fs::create_dir_all(&target).await?;
    let frontmatter = serde_yaml::to_string(&SkillFrontmatter {
        name: name.to_owned(),
        description: description.to_owned(),
    })
    .map_err(|error| ExtensionError::Internal(format!("生成技能说明失败：{error}")))?;
    let entry = format!(
        "---\n{}---\n\n# {name}\n\n{description}\n",
        frontmatter.trim_start_matches("---\n")
    );
    tokio::fs::write(target.join(SKILL_ENTRY_FILE), entry).await?;
    normalize_skill_workspace(
        &target,
        slug,
        SkillSource::Local,
        None,
        git,
        "chore(skill): 初始化技能工作区",
    )
    .await
}

pub async fn clone_skill(
    root: &Path,
    repository_url: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    tokio::fs::create_dir_all(root).await?;
    let provision = git
        .clone_workspace_repository(repository_url, root)
        .await
        .map_err(ExtensionError::Internal)?;
    let mut target = PathBuf::from(provision.workspace_path);
    if !target.join(SKILL_ENTRY_FILE).is_file() {
        return Err(ExtensionError::SkillImportNoSkillFound(target.display().to_string()));
    }
    let slug = read_manifest(&target)
        .await
        .ok()
        .map(|manifest| manifest.id)
        .or_else(|| target.file_name().and_then(|name| name.to_str()).map(str::to_owned))
        .ok_or_else(|| ExtensionError::InvalidSkillPath(target.display().to_string()))?;
    validate_slug(&slug)?;
    if target.file_name().and_then(|name| name.to_str()) != Some(&slug) {
        let renamed = root.join(&slug);
        if renamed.exists() {
            return Err(ExtensionError::InvalidRequest(format!("技能 {slug} 已存在")));
        }
        tokio::fs::rename(&target, &renamed).await?;
        target = renamed;
    }
    normalize_skill_workspace(
        &target,
        &slug,
        SkillSource::Local,
        None,
        git,
        "chore(skill): 接入 Tjuae 技能工作区",
    )
    .await
}

pub async fn market_indexes() -> Result<Vec<MarketIndex>, ExtensionError> {
    let default = fetch_market_index(DEFAULT_MARKET_INDEX_URL).await?;
    let mut indexes = vec![default];
    let mut ids = HashSet::from([indexes[0].market.id.clone()]);
    if let Ok(configured) = std::env::var(MARKET_INDEXES_ENV) {
        for url in configured.split(';').map(str::trim).filter(|value| !value.is_empty()) {
            if url == DEFAULT_MARKET_INDEX_URL {
                continue;
            }
            match fetch_market_index(url).await {
                Ok(index) if ids.insert(index.market.id.clone()) => indexes.push(index),
                Ok(index) => warn!(market_id = %index.market.id, %url, "忽略 ID 重复的外部技能市场"),
                Err(error) => warn!(%url, %error, "忽略暂时不可用的外部技能市场"),
            }
        }
    }
    Ok(indexes)
}

async fn fetch_market_index(url: &str) -> Result<MarketIndex, ExtensionError> {
    if let Some(cached) = MARKET_CACHE.read().await.get(url)
        && cached.fetched_at.elapsed().is_ok_and(|age| age < MARKET_CACHE_TTL)
    {
        return Ok(cached.index.clone());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| ExtensionError::Internal(format!("创建市场客户端失败：{error}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| ExtensionError::Internal(format!("读取技能市场失败：{error}")))?;
    let index: MarketIndex = response
        .json()
        .await
        .map_err(|error| ExtensionError::Internal(format!("解析技能市场失败：{error}")))?;
    validate_market_index(&index)?;
    MARKET_CACHE.write().await.insert(
        url.to_owned(),
        CachedMarketIndex {
            fetched_at: SystemTime::now(),
            index: index.clone(),
        },
    );
    Ok(index)
}

pub async fn install_market_skill(
    root: &Path,
    market_id: &str,
    slug: &str,
    replace: bool,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    validate_slug(slug)?;
    let index = market_indexes()
        .await?
        .into_iter()
        .find(|index| index.market.id == market_id)
        .ok_or_else(|| ExtensionError::InvalidRequest(format!("找不到技能市场：{market_id}")))?;
    let entry = index
        .skills
        .iter()
        .find(|entry| entry.id == slug)
        .ok_or_else(|| ExtensionError::SkillNotFound(slug.to_owned()))?
        .clone();
    let target = root.join(slug);
    let previous = if target.exists() {
        let installed = load_installed_skill(&target).await?;
        if !replace {
            return match &installed.source {
                SkillSource::Market { market_id, path, .. } if market_id == &index.market.id && path == &entry.path => {
                    Ok(installed)
                }
                _ => Err(ExtensionError::InvalidRequest(format!("本地技能 {slug} 已存在"))),
            };
        }
        match &installed.source {
            SkillSource::Market { market_id, path, .. } if market_id == &index.market.id && path == &entry.path => {
                if !git
                    .workspace_matches_market_baseline(&target)
                    .await
                    .map_err(ExtensionError::Internal)?
                {
                    return Err(ExtensionError::InvalidRequest(format!(
                        "本地技能 {slug} 有尚未发布或比较的修改，不能直接覆盖更新"
                    )));
                }
                Some(installed.preferences)
            }
            _ => {
                return Err(ExtensionError::InvalidRequest(format!(
                    "本地技能 {slug} 未关联当前市场，不能用市场内容覆盖"
                )));
            }
        }
    } else {
        None
    };
    tokio::fs::create_dir_all(root).await?;
    let staging = root.join(format!(".installing-{slug}"));
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging).await?;
    }
    git.materialize_repository_path(&index.repository, &index.revision, &entry.path, &staging)
        .await
        .map_err(ExtensionError::Internal)?;
    verify_workspace_digest(&staging, &entry.digest)?;
    let mut manifest = read_manifest(&staging).await?;
    validate_manifest_identity(&manifest, slug)?;
    match &manifest.source {
        SkillSource::Market {
            market_id,
            repository,
            path,
            ..
        } if market_id == &index.market.id && repository == &index.repository && path == &entry.path => {}
        _ => {
            return Err(ExtensionError::ManifestValidation(
                "市场目录中的技能清单未指向当前市场条目".to_owned(),
            ));
        }
    }
    manifest.version = entry.version.clone();
    manifest.categories = entry.categories.clone();
    let market_source = SkillSource::Market {
        market_id: index.market.id.clone(),
        repository: index.repository.clone(),
        path: entry.path.clone(),
        revision: Some(index.revision.clone()),
    };
    if let Some(preferences) = previous {
        manifest.enabled = preferences.enabled;
        manifest.auto_inject = preferences.auto_inject;
    }
    manifest.source = market_source.clone();
    write_manifest(&staging.join(SKILL_PACKAGE_MANIFEST), &manifest).await?;
    if target.exists() {
        git.commit_workspace_snapshot(&target, &format!("chore(skill): 保存 {slug} 更新前状态"))
            .await
            .map_err(ExtensionError::Internal)?;
        replace_worktree_contents(&target, &staging).await?;
        tokio::fs::remove_dir_all(&staging).await?;
    } else {
        rename_installed_directory(&staging, &target).await?;
    }
    let installed = normalize_skill_workspace(
        &target,
        slug,
        market_source,
        previous,
        git.clone(),
        &format!("chore(skill): 同步 {slug} 至 {}", entry.version),
    )
    .await?;
    git.mark_market_baseline(&target)
        .await
        .map_err(ExtensionError::Internal)?;
    Ok(installed)
}

pub async fn market_sync_state(
    installed: Option<&InstalledSkill>,
    index: &MarketIndex,
    entry: &MarketSkillEntry,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<MarketSyncState, ExtensionError> {
    let Some(installed) = installed else {
        return Ok(MarketSyncState::NotInstalled);
    };
    let SkillSource::Market {
        market_id,
        repository,
        path,
        revision,
    } = &installed.source
    else {
        return Ok(MarketSyncState::NotInstalled);
    };
    if market_id != &index.market.id || repository != &index.repository || path != &entry.path {
        return Ok(MarketSyncState::NotInstalled);
    }
    let local_changed = !git
        .workspace_matches_market_baseline(&installed.path)
        .await
        .map_err(ExtensionError::Internal)?;
    let remote_changed = revision.as_deref() != Some(index.revision.as_str());
    Ok(match (local_changed, remote_changed) {
        (false, false) => MarketSyncState::Synced,
        (true, false) => MarketSyncState::LocalChanged,
        (false, true) => MarketSyncState::UpdateAvailable,
        (true, true) => MarketSyncState::Diverged,
    })
}

pub async fn compare_market_skill(
    root: &Path,
    market_id: &str,
    slug: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<MarketSkillComparison, ExtensionError> {
    validate_slug(slug)?;
    let (index, entry) = find_market_entry(market_id, slug).await?;
    let installed = load_installed_skill(&root.join(slug)).await?;
    ensure_market_link(&installed, &index, &entry)?;
    let sync_state = market_sync_state(Some(&installed), &index, &entry, git.clone()).await?;
    let base_revision = match &installed.source {
        SkillSource::Market { revision, .. } => revision.clone().unwrap_or_default(),
        SkillSource::Local => String::new(),
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ExtensionError::Internal(error.to_string()))?
        .as_nanos();
    let remote = root.join(format!(".comparing-{slug}-{nonce}"));
    let result = async {
        git.materialize_repository_path(&index.repository, &index.revision, &entry.path, &remote)
            .await
            .map_err(ExtensionError::Internal)?;
        verify_workspace_digest(&remote, &entry.digest)?;
        compare_skill_trees(&installed.path, &remote)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&remote).await;
    Ok(MarketSkillComparison {
        slug: slug.to_owned(),
        base_revision,
        remote_revision: index.revision,
        sync_state,
        files: result?,
    })
}

pub async fn publish_market_skill(
    root: &Path,
    market_id: &str,
    slug: &str,
    fork_repository_url: &str,
    message: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<MarketSkillPublication, ExtensionError> {
    validate_slug(slug)?;
    let (index, entry) = find_market_entry(market_id, slug).await?;
    if index.market.id != "tjuae-hub" {
        return Err(ExtensionError::InvalidRequest("外部市场只读，不支持发布".to_owned()));
    }
    let installed = load_installed_skill(&root.join(slug)).await?;
    ensure_market_link(&installed, &index, &entry)?;
    let (upstream_owner, upstream_repo) = github_repository_identity(&index.repository)
        .ok_or_else(|| ExtensionError::InvalidRequest("TjuaeHub 仓库地址无效".to_owned()))?;
    let (fork_owner, fork_repo) = github_repository_identity(fork_repository_url)
        .ok_or_else(|| ExtensionError::InvalidRequest("请输入 TjuaeHub Fork 的 GitHub 仓库地址".to_owned()))?;
    if !fork_repo.eq_ignore_ascii_case(&upstream_repo) {
        return Err(ExtensionError::InvalidRequest(format!(
            "Fork 仓库必须与 {upstream_repo} 同名"
        )));
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ExtensionError::Internal(error.to_string()))?
        .as_secs();
    let branch = format!("tjuae/skill-{slug}-{timestamp}");
    let published = git
        .publish_workspace_path(&installed.path, fork_repository_url, &entry.path, &branch, message)
        .await
        .map_err(ExtensionError::Internal)?;
    let compare_url = format!(
        "https://github.com/{upstream_owner}/{upstream_repo}/compare/main...{fork_owner}:{}?expand=1",
        published.branch
    );
    Ok(MarketSkillPublication {
        branch: published.branch,
        commit: published.commit,
        compare_url,
    })
}

pub async fn list_installed_skills(root: &Path) -> Result<Vec<InstalledSkill>, ExtensionError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut skills = Vec::new();
    let mut entries = tokio::fs::read_dir(root).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        match load_installed_skill(&entry.path()).await {
            Ok(skill) => skills.push(skill),
            Err(error) => warn!(path = %entry.path().display(), %error, "忽略无效技能工作区"),
        }
    }
    skills.sort_by_key(|skill| skill.name.to_lowercase());
    Ok(skills)
}

pub async fn load_installed_skill(directory: &Path) -> Result<InstalledSkill, ExtensionError> {
    let directory = canonical_directory(directory)?;
    let manifest = read_manifest(&directory).await?;
    validate_manifest(&manifest, &directory)?;
    let entry_path = directory.join(SKILL_ENTRY_FILE);
    let source = tokio::fs::read_to_string(&entry_path).await?;
    let frontmatter = parse_frontmatter(&source, &entry_path)?;
    Ok(InstalledSkill {
        id: manifest.id.clone(),
        slug: manifest.id,
        name: frontmatter.name,
        description: frontmatter.description,
        version: manifest.version,
        path: directory,
        source: manifest.source,
        categories: manifest.categories,
        preferences: SkillPreferences {
            enabled: manifest.enabled,
            auto_inject: manifest.auto_inject,
        },
    })
}

pub async fn resolve_installed_skill(root: &Path, reference: &str) -> Result<Option<InstalledSkill>, ExtensionError> {
    let slug = reference.trim();
    validate_slug(slug)?;
    let candidate = root.join(slug);
    if !candidate.is_dir() {
        return Ok(None);
    }
    load_installed_skill(&candidate).await.map(Some)
}

pub async fn initialize_skill_workspaces(
    root: &Path,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<Vec<InstalledSkill>, ExtensionError> {
    tokio::fs::create_dir_all(root).await?;
    ensure_skill_repositories(root, git).await
}

pub async fn ensure_skill_repositories(
    root: &Path,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<Vec<InstalledSkill>, ExtensionError> {
    let skills = list_installed_skills(root).await?;
    for skill in &skills {
        git.ensure_workspace_git(&skill.path)
            .await
            .map_err(ExtensionError::Internal)?;
    }
    Ok(skills)
}

pub async fn import_skill(
    root: &Path,
    source: &Path,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    let source = canonical_directory(source)?;
    if !source.join(SKILL_ENTRY_FILE).is_file() {
        return Err(ExtensionError::SkillImportNoSkillFound(source.display().to_string()));
    }
    let source_manifest = read_manifest(&source).await.ok();
    let slug = source_manifest
        .as_ref()
        .map(|manifest| manifest.id.clone())
        .or_else(|| source.file_name().and_then(|name| name.to_str()).map(str::to_owned))
        .ok_or_else(|| ExtensionError::InvalidSkillPath(source.display().to_string()))?;
    validate_slug(&slug)?;
    let target = root.join(&slug);
    if target.exists() {
        return Err(ExtensionError::InvalidRequest(format!("技能 {slug} 已存在")));
    }
    copy_tree(&source, &target).await?;
    normalize_skill_workspace(
        &target,
        &slug,
        SkillSource::Local,
        None,
        git,
        "chore(skill): 导入技能工作区",
    )
    .await
}

pub async fn copy_skill(
    root: &Path,
    source_slug: &str,
    target_slug: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    validate_slug(source_slug)?;
    validate_slug(target_slug)?;
    let source = load_installed_skill(&root.join(source_slug)).await?;
    let target = root.join(target_slug);
    if target.exists() {
        return Err(ExtensionError::InvalidRequest(format!("技能 {target_slug} 已存在")));
    }
    copy_tree(&source.path, &target).await?;
    normalize_skill_workspace(
        &target,
        target_slug,
        SkillSource::Local,
        Some(source.preferences),
        git,
        "chore(skill): 创建技能副本",
    )
    .await
}

pub async fn update_skill_preferences(
    root: &Path,
    slug: &str,
    preferences: SkillPreferences,
) -> Result<InstalledSkill, ExtensionError> {
    validate_slug(slug)?;
    if preferences.auto_inject && !preferences.enabled {
        return Err(ExtensionError::InvalidRequest("未启用的技能不能自动注入".to_owned()));
    }
    let directory = root.join(slug);
    let mut manifest = read_manifest(&directory).await?;
    manifest.enabled = preferences.enabled;
    manifest.auto_inject = preferences.auto_inject;
    write_manifest(&directory.join(SKILL_PACKAGE_MANIFEST), &manifest).await?;
    load_installed_skill(&directory).await
}

pub async fn delete_installed_skill(root: &Path, slug: &str) -> Result<(), ExtensionError> {
    validate_slug(slug)?;
    let target = root.join(slug);
    if !target.is_dir() {
        return Err(ExtensionError::SkillNotFound(slug.to_owned()));
    }
    tokio::fs::remove_dir_all(target).await?;
    Ok(())
}

fn local_manifest(slug: &str, version: &str) -> SkillManifest {
    SkillManifest {
        schema: SKILL_SCHEMA_URL.to_owned(),
        schema_version: 1,
        id: slug.to_owned(),
        version: version.to_owned(),
        categories: Vec::new(),
        enabled: true,
        auto_inject: false,
        source: SkillSource::Local,
    }
}

/// The only normalization/write boundary shared by manual creation, Butler,
/// folder import, Git clone, copy, and market materialization.
async fn normalize_skill_workspace(
    directory: &Path,
    slug: &str,
    source: SkillSource,
    preferences: Option<SkillPreferences>,
    git: Arc<dyn WorkspaceGitProvisioner>,
    commit_message: &str,
) -> Result<InstalledSkill, ExtensionError> {
    validate_slug(slug)?;
    if !directory.join(SKILL_ENTRY_FILE).is_file() {
        return Err(ExtensionError::SkillImportNoSkillFound(directory.display().to_string()));
    }
    let mut manifest = read_manifest(directory)
        .await
        .unwrap_or_else(|_| local_manifest(slug, "0.1.0"));
    manifest.schema = SKILL_SCHEMA_URL.to_owned();
    manifest.schema_version = 1;
    manifest.id = slug.to_owned();
    if Version::parse(&manifest.version).is_err() {
        manifest.version = "0.1.0".to_owned();
    }
    let mut seen = HashSet::new();
    manifest
        .categories
        .retain(|category| !category.trim().is_empty() && seen.insert(category.clone()));
    manifest.source = source;
    if let Some(preferences) = preferences {
        manifest.enabled = preferences.enabled;
        manifest.auto_inject = preferences.auto_inject;
    }
    write_manifest(&directory.join(SKILL_PACKAGE_MANIFEST), &manifest).await?;
    git.ensure_workspace_git(directory)
        .await
        .map_err(ExtensionError::Internal)?;
    git.commit_workspace_snapshot(directory, commit_message)
        .await
        .map_err(ExtensionError::Internal)?;
    load_installed_skill(directory).await
}

async fn read_manifest(directory: &Path) -> Result<SkillManifest, ExtensionError> {
    Ok(serde_json::from_slice(
        &tokio::fs::read(directory.join(SKILL_PACKAGE_MANIFEST)).await?,
    )?)
}

async fn write_manifest(path: &Path, manifest: &SkillManifest) -> Result<(), ExtensionError> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let temporary = path.with_extension("json.tmp");
    tokio::fs::write(&temporary, [bytes, b"\n".to_vec()].concat()).await?;
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}

fn validate_manifest(manifest: &SkillManifest, directory: &Path) -> Result<(), ExtensionError> {
    validate_manifest_identity(
        manifest,
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ExtensionError::InvalidSkillPath(directory.display().to_string()))?,
    )
}

fn validate_manifest_identity(manifest: &SkillManifest, slug: &str) -> Result<(), ExtensionError> {
    if manifest.schema != SKILL_SCHEMA_URL || manifest.schema_version != 1 {
        return Err(ExtensionError::ManifestValidation(
            "技能必须使用唯一的 Tjuae v1 清单".to_owned(),
        ));
    }
    validate_slug(&manifest.id)?;
    if manifest.id != slug {
        return Err(ExtensionError::ManifestValidation(format!(
            "技能 ID {} 与目录 {slug} 不一致",
            manifest.id
        )));
    }
    Version::parse(&manifest.version).map_err(|error| ExtensionError::InvalidVersion {
        version: manifest.version.clone(),
        reason: error.to_string(),
    })?;
    let mut categories = HashSet::new();
    if manifest.categories.iter().any(|category| {
        let category = category.trim();
        category.is_empty() || !categories.insert(category)
    }) {
        return Err(ExtensionError::ManifestValidation("技能分类不能为空或重复".to_owned()));
    }
    if let SkillSource::Market {
        market_id,
        repository,
        path,
        revision,
    } = &manifest.source
    {
        if market_id.trim().is_empty() || repository.trim().is_empty() {
            return Err(ExtensionError::ManifestValidation("市场关联缺少市场或仓库".to_owned()));
        }
        validate_repository_path(path, slug)?;
        if revision.as_deref().is_some_and(|value| !is_revision(value)) {
            return Err(ExtensionError::ManifestValidation("市场修订号无效".to_owned()));
        }
    }
    Ok(())
}

async fn find_market_entry(market_id: &str, slug: &str) -> Result<(MarketIndex, MarketSkillEntry), ExtensionError> {
    let index = market_indexes()
        .await?
        .into_iter()
        .find(|index| index.market.id == market_id)
        .ok_or_else(|| ExtensionError::InvalidRequest(format!("找不到技能市场：{market_id}")))?;
    let entry = index
        .skills
        .iter()
        .find(|entry| entry.id == slug)
        .cloned()
        .ok_or_else(|| ExtensionError::SkillNotFound(slug.to_owned()))?;
    Ok((index, entry))
}

fn ensure_market_link(
    installed: &InstalledSkill,
    index: &MarketIndex,
    entry: &MarketSkillEntry,
) -> Result<(), ExtensionError> {
    match &installed.source {
        SkillSource::Market {
            market_id,
            repository,
            path,
            ..
        } if market_id == &index.market.id && repository == &index.repository && path == &entry.path => Ok(()),
        _ => Err(ExtensionError::InvalidRequest(format!(
            "本地技能 {} 未关联当前市场条目",
            installed.slug
        ))),
    }
}

fn compare_skill_trees(local: &Path, remote: &Path) -> Result<Vec<MarketFileComparison>, ExtensionError> {
    let local_files = read_comparison_tree(local)?;
    let remote_files = read_comparison_tree(remote)?;
    let mut paths = local_files
        .keys()
        .chain(remote_files.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let mut changes = Vec::new();
    for path in paths {
        let local_bytes = local_files.get(&path);
        let remote_bytes = remote_files.get(&path);
        if local_bytes == remote_bytes {
            continue;
        }
        let status = match (local_bytes, remote_bytes) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            (Some(_), Some(_)) => "modified",
            (None, None) => continue,
        };
        let local_content = local_bytes.and_then(|bytes| String::from_utf8(bytes.clone()).ok());
        let remote_content = remote_bytes.and_then(|bytes| String::from_utf8(bytes.clone()).ok());
        let binary = local_bytes.is_some_and(|bytes| local_content.is_none() && !bytes.is_empty())
            || remote_bytes.is_some_and(|bytes| remote_content.is_none() && !bytes.is_empty());
        changes.push(MarketFileComparison {
            path,
            status: status.to_owned(),
            binary,
            local_content,
            remote_content,
        });
    }
    Ok(changes)
}

fn read_comparison_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, ExtensionError> {
    const MAX_FILES: usize = 2_000;
    const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), ExtensionError> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), ".git" | "node_modules" | ".DS_Store" | "__MACOSX") {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(ExtensionError::SkillImportInvalidSource(format!(
                    "技能目录不能包含符号链接：{}",
                    entry.path().display()
                )));
            }
            if file_type.is_dir() {
                visit(root, &entry.path(), files)?;
                continue;
            }
            if files.len() >= MAX_FILES {
                return Err(ExtensionError::InvalidRequest("技能文件数量超过比较上限".to_owned()));
            }
            let metadata = entry.metadata()?;
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|error| ExtensionError::Internal(error.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = if metadata.len() > MAX_FILE_BYTES {
                vec![0]
            } else {
                std::fs::read(entry.path())?
            };
            files.insert(relative, bytes);
        }
        Ok(())
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn github_repository_identity(repository_url: &str) -> Option<(String, String)> {
    let value = repository_url.trim().trim_end_matches('/');
    let path = if let Some(path) = value.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = value.strip_prefix("ssh://git@github.com/") {
        path
    } else if let Some(path) = value.strip_prefix("https://github.com/") {
        path
    } else {
        value.strip_prefix("http://github.com/")?
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/');
    let owner = segments.next()?.trim();
    let repository = segments.next()?.trim();
    if owner.is_empty() || repository.is_empty() || segments.next().is_some() {
        return None;
    }
    Some((owner.to_owned(), repository.to_owned()))
}

fn validate_market_index(index: &MarketIndex) -> Result<(), ExtensionError> {
    if index.schema_version != 1 || index.market.id.trim().is_empty() || index.market.name.trim().is_empty() {
        return Err(ExtensionError::ManifestValidation("技能市场索引元数据无效".to_owned()));
    }
    if index.repository.trim().is_empty() || !is_revision(&index.revision) {
        return Err(ExtensionError::ManifestValidation(
            "技能市场仓库或修订号无效".to_owned(),
        ));
    }
    let mut ids = HashSet::new();
    for entry in &index.skills {
        validate_slug(&entry.id)?;
        validate_repository_path(&entry.path, &entry.id)?;
        Version::parse(&entry.version).map_err(|error| ExtensionError::InvalidVersion {
            version: entry.version.clone(),
            reason: error.to_string(),
        })?;
        if !ids.insert(&entry.id) || !is_sha256(&entry.digest) {
            return Err(ExtensionError::ManifestValidation(format!(
                "技能市场条目 {} 重复或摘要无效",
                entry.id
            )));
        }
    }
    Ok(())
}

fn validate_repository_path(path: &str, slug: &str) -> Result<(), ExtensionError> {
    let value = Path::new(path);
    if value.is_absolute()
        || path != format!("skills/{slug}")
        || value
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ExtensionError::ManifestValidation(format!("技能市场路径无效：{path}")));
    }
    Ok(())
}

fn validate_slug(slug: &str) -> Result<(), ExtensionError> {
    if slug.is_empty()
        || slug.len() > 80
        || slug.starts_with('-')
        || slug.ends_with('-')
        || slug
            .chars()
            .any(|character| !(character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'))
    {
        return Err(ExtensionError::InvalidSkillPath(slug.to_owned()));
    }
    Ok(())
}

fn is_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256-")
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn parse_frontmatter(source: &str, path: &Path) -> Result<SkillFrontmatter, ExtensionError> {
    let normalized = source.replace("\r\n", "\n");
    let body = normalized
        .strip_prefix("---\n")
        .and_then(|value| value.split_once("\n---\n").map(|(frontmatter, _)| frontmatter))
        .ok_or_else(|| ExtensionError::SkillInvalidFrontmatter(path.display().to_string()))?;
    let frontmatter: SkillFrontmatter = serde_yaml::from_str(body)
        .map_err(|error| ExtensionError::SkillInvalidFrontmatter(format!("{}：{error}", path.display())))?;
    if frontmatter.name.trim().is_empty() || frontmatter.description.trim().is_empty() {
        return Err(ExtensionError::SkillInvalidFrontmatter(path.display().to_string()));
    }
    Ok(frontmatter)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ExtensionError> {
    let directory = std::fs::canonicalize(path)
        .map_err(|error| ExtensionError::SkillImportInvalidSource(format!("{}：{error}", path.display())))?;
    if !directory.is_dir() {
        return Err(ExtensionError::SkillImportInvalidSource(
            directory.display().to_string(),
        ));
    }
    Ok(directory)
}

fn verify_workspace_digest(directory: &Path, expected: &str) -> Result<(), ExtensionError> {
    let actual = workspace_digest(directory)?;
    if actual != expected {
        return Err(ExtensionError::ManifestValidation(format!(
            "市场技能内容摘要不匹配：期望 {expected}，实际 {actual}"
        )));
    }
    Ok(())
}

fn workspace_digest(directory: &Path) -> Result<String, ExtensionError> {
    fn collect(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), ".git" | "node_modules" | ".DS_Store" | "__MACOSX") {
                continue;
            }
            if entry.file_type()?.is_dir() {
                collect(root, &entry.path(), paths)?;
            } else {
                paths.push(entry.path().strip_prefix(root).unwrap_or(&entry.path()).to_path_buf());
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    collect(directory, directory, &mut paths)?;
    paths.sort_by(|left, right| {
        left.to_string_lossy()
            .replace('\\', "/")
            .cmp(&right.to_string_lossy().replace('\\', "/"))
    });
    let mut hash = Sha256::new();
    hash.update(b"tjuae-skill-workspace-v1\0");
    for relative in paths {
        let display = relative.to_string_lossy().replace('\\', "/");
        hash.update(display.as_bytes());
        hash.update(b"\0");
        hash.update(std::fs::read(directory.join(&relative))?);
        hash.update(b"\0");
    }
    Ok(format!("sha256-{:x}", hash.finalize()))
}

async fn rename_installed_directory(source: &Path, target: &Path) -> Result<(), ExtensionError> {
    let mut last_error = None;
    for delay_ms in [0, 25, 50, 100, 200, 400] {
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        match tokio::fs::rename(source, target).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => last_error = Some(error),
            Err(error) => return Err(error.into()),
        }
    }
    Err(last_error.expect("rename retry records an error").into())
}

async fn replace_worktree_contents(target: &Path, source: &Path) -> Result<(), ExtensionError> {
    let mut existing = tokio::fs::read_dir(target).await?;
    while let Some(entry) = existing.next_entry().await? {
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().await?.is_dir() {
            tokio::fs::remove_dir_all(entry.path()).await?;
        } else {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    let mut incoming = tokio::fs::read_dir(source).await?;
    while let Some(entry) = incoming.next_entry().await? {
        tokio::fs::rename(entry.path(), target.join(entry.file_name())).await?;
    }
    Ok(())
}

async fn copy_tree(source: &Path, target: &Path) -> Result<(), ExtensionError> {
    tokio::fs::create_dir_all(target).await?;
    let mut entries = tokio::fs::read_dir(source).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().await?.is_symlink() {
            return Err(ExtensionError::SkillImportInvalidSource(format!(
                "技能目录不能包含符号链接：{}",
                entry.path().display()
            )));
        }
        let child_target = target.join(entry.file_name());
        if entry.file_type().await?.is_dir() {
            Box::pin(copy_tree(&entry.path(), &child_target)).await?;
        } else {
            tokio::fs::copy(entry.path(), child_target).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tempfile::TempDir;
    use tjuaeui_common::WorkspaceGitProvision;

    struct TestGit;

    #[async_trait]
    impl WorkspaceGitProvisioner for TestGit {
        async fn ensure_workspace_git(&self, workspace: &Path) -> Result<WorkspaceGitProvision, String> {
            Ok(WorkspaceGitProvision {
                repository_root: workspace.display().to_string(),
                workspace_path: workspace.display().to_string(),
                branch: "main".to_owned(),
                head_commit: "test".to_owned(),
            })
        }

        async fn commit_workspace_snapshot(&self, _workspace: &Path, _message: &str) -> Result<String, String> {
            Ok("test".to_owned())
        }
    }

    #[tokio::test]
    async fn create_list_and_copy_use_one_workspace_model() {
        let temp = TempDir::new().unwrap();
        let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);
        let created = create_skill(temp.path(), "cron", "cron", "test", git.clone())
            .await
            .unwrap();
        assert_eq!(created.source, SkillSource::Local);
        assert_eq!(list_installed_skills(temp.path()).await.unwrap().len(), 1);
        let copied = copy_skill(temp.path(), "cron", "cron-copy", git).await.unwrap();
        assert_eq!(copied.id, "cron-copy");
        assert_eq!(copied.source, SkillSource::Local);
    }

    #[test]
    fn digest_matches_market_algorithm() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("a.txt"), b"one").unwrap();
        std::fs::create_dir(temp.path().join("b")).unwrap();
        std::fs::write(temp.path().join("b/c.txt"), b"two").unwrap();
        assert_eq!(workspace_digest(temp.path()).unwrap().len(), 71);
    }

    #[test]
    fn comparison_reports_only_real_file_differences() {
        let local = TempDir::new().unwrap();
        let remote = TempDir::new().unwrap();
        std::fs::write(local.path().join("same.md"), b"same").unwrap();
        std::fs::write(remote.path().join("same.md"), b"same").unwrap();
        std::fs::write(local.path().join("local.md"), b"local").unwrap();
        std::fs::write(remote.path().join("remote.md"), b"remote").unwrap();
        std::fs::write(local.path().join("changed.md"), b"before").unwrap();
        std::fs::write(remote.path().join("changed.md"), b"after").unwrap();

        let changes = compare_skill_trees(local.path(), remote.path()).unwrap();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].path, "changed.md");
        assert_eq!(changes[0].status, "modified");
        assert_eq!(changes[1].status, "deleted");
        assert_eq!(changes[2].status, "added");
    }

    #[test]
    fn github_fork_parser_accepts_git_and_https_urls_only() {
        assert_eq!(
            github_repository_identity("https://github.com/example/TjuaeHub.git"),
            Some(("example".to_owned(), "TjuaeHub".to_owned()))
        );
        assert_eq!(
            github_repository_identity("git@github.com:example/TjuaeHub.git"),
            Some(("example".to_owned(), "TjuaeHub".to_owned()))
        );
        assert_eq!(github_repository_identity("https://example.com/TjuaeHub.git"), None);
    }
}
