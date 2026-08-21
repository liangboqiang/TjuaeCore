use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, Response, StatusCode, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tjuaeui_common::{WorkspaceGitProvisioner, WorkspaceRevisionFile};

use crate::error::ExtensionError;
use crate::skill_package::{
    InstalledSkill, SKILL_ENTRY_FILE, SKILL_PACKAGE_MANIFEST, SkillManifest, list_installed_skills,
    load_skill_from_directory, normalize_skill_workspace, seal_skill_package,
};

const SKILLHUB_BASE_URL: &str = "https://api.skillhub.cn";
const CLAWHUB_BASE_URL: &str = "https://clawhub.ai";
const CATALOG_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const CATALOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PAGE_SIZE: u32 = 100;
const MAX_FILE_COUNT: usize = 2_000;
const MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;
const MAX_PACKAGE_SIZE: u64 = 20 * 1024 * 1024;

fn unique_versions(versions: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    versions
        .into_iter()
        .filter(|version| !version.is_empty() && seen.insert(version.clone()))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillSpace {
    Mine,
    TjuaeHub,
    SkillHub,
    ClawHub,
}

impl SkillSpace {
    pub fn parse(value: &str) -> Result<Self, ExtensionError> {
        match value {
            "mine" => Ok(Self::Mine),
            "tjuae-hub" => Ok(Self::TjuaeHub),
            "skillhub" => Ok(Self::SkillHub),
            "clawhub" => Ok(Self::ClawHub),
            _ => Err(ExtensionError::InvalidRequest(format!("未知技能空间：{value}"))),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Mine => "mine",
            Self::TjuaeHub => "tjuae-hub",
            Self::SkillHub => "skillhub",
            Self::ClawHub => "clawhub",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSkill {
    pub id: String,
    pub space: SkillSpace,
    pub slug: String,
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub categories: Vec<String>,
    pub icon_url: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogPage {
    pub items: Vec<CatalogSkill>,
    pub total: u64,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFile {
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFileContent {
    pub path: String,
    pub content: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFileDiff {
    pub path: String,
    pub status: String,
    pub binary: bool,
    pub base_content: Option<String>,
    pub target_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSecurityReport {
    pub provider: String,
    pub status: String,
    pub label: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogDetail {
    pub skill: CatalogSkill,
    pub readme: String,
    pub files: Vec<CatalogFile>,
    pub versions: Vec<String>,
    pub security_reports: Vec<CatalogSecurityReport>,
}

pub async fn list_catalog(
    root: &Path,
    space: SkillSpace,
    query: &str,
    sort: &str,
    cursor: Option<&str>,
    limit: u32,
    _git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<CatalogPage, ExtensionError> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    match space {
        SkillSpace::Mine => list_mine(list_installed_skills(root).await?, query, sort, cursor, limit),
        SkillSpace::TjuaeHub => list_tjuae_hub(query, sort, cursor, limit).await,
        SkillSpace::SkillHub => list_skillhub(query, cursor, limit).await,
        SkillSpace::ClawHub => list_clawhub(query, cursor, limit).await,
    }
}

pub async fn catalog_detail(
    root: &Path,
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    version: Option<&str>,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<CatalogDetail, ExtensionError> {
    match space {
        SkillSpace::Mine => mine_detail(root, slug, version, list_installed_skills(root).await?, git).await,
        SkillSpace::TjuaeHub => tjuae_hub_detail(slug, version).await,
        SkillSpace::SkillHub => skillhub_detail(namespace, slug, version).await,
        SkillSpace::ClawHub => clawhub_detail(namespace, slug, version).await,
    }
}

pub async fn catalog_file_content(
    root: &Path,
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    path: &str,
    version: Option<&str>,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<CatalogFileContent, ExtensionError> {
    validate_catalog_file(path, 0)?;
    let bytes = catalog_file_bytes(root, space, namespace, slug, path, version, git).await?;
    let content = String::from_utf8(bytes)
        .map_err(|_| ExtensionError::SkillImportInvalidSource(format!("技能文件不是 UTF-8 文本：{path}")))?;
    Ok(CatalogFileContent {
        path: path.to_owned(),
        size: content.len() as u64,
        content,
    })
}

async fn catalog_file_bytes(
    root: &Path,
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    path: &str,
    version: Option<&str>,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<Vec<u8>, ExtensionError> {
    validate_catalog_file(path, 0)?;
    let namespace = resolve_transport_namespace(space, namespace, slug).await?;
    match space {
        SkillSpace::Mine => read_mine_catalog_file_bytes(root, slug, path, version, git).await,
        SkillSpace::TjuaeHub => {
            if let Some(skills_root) =
                crate::skill_storage::resolve_tjuae_hub_worktree().map(|root| root.join("skills"))
            {
                let directory = skills_root.join(slug);
                if let Ok(skill) = crate::load_installed_skill(&directory).await
                    && version.is_none_or(|requested| requested == skill.version)
                {
                    return read_local_catalog_file_bytes(&skills_root, slug, path).await;
                }
            }
            fetch_tjuae_hub_file_bytes(slug, path, version).await
        }
        SkillSpace::SkillHub | SkillSpace::ClawHub => {
            fetch_external_file_bytes(space, &namespace, slug, path, version).await
        }
    }
}

pub async fn compare_catalog_versions(
    root: &Path,
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    base: &str,
    target: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<Vec<CatalogFileDiff>, ExtensionError> {
    if base == target {
        return Err(ExtensionError::InvalidRequest("请选择两个不同版本进行比较".to_owned()));
    }
    let transport_namespace = resolve_transport_namespace(space, namespace, slug).await?;
    let base_detail = catalog_detail(root, space, &transport_namespace, slug, Some(base), git.clone()).await?;
    let target_detail = catalog_detail(root, space, &transport_namespace, slug, Some(target), git.clone()).await?;
    for version in [base, target] {
        if !base_detail.versions.iter().any(|available| available == version) {
            return Err(ExtensionError::InvalidVersion {
                version: version.to_owned(),
                reason: "该来源的技能没有这个版本".into(),
            });
        }
    }
    let paths = base_detail
        .files
        .iter()
        .map(|file| file.path.clone())
        .chain(target_detail.files.iter().map(|file| file.path.clone()))
        .collect::<BTreeSet<_>>();
    let mut diffs = Vec::new();
    for path in paths {
        let base_content = if base_detail.files.iter().any(|file| file.path == *path) {
            catalog_file_content(root, space, &transport_namespace, slug, &path, Some(base), git.clone())
                .await
                .ok()
                .map(|file| file.content)
        } else {
            None
        };
        let target_content = if target_detail.files.iter().any(|file| file.path == *path) {
            catalog_file_content(
                root,
                space,
                &transport_namespace,
                slug,
                &path,
                Some(target),
                git.clone(),
            )
            .await
            .ok()
            .map(|file| file.content)
        } else {
            None
        };
        if base_content == target_content {
            continue;
        }
        let status = match (&base_content, &target_content) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            _ => "modified",
        };
        diffs.push(CatalogFileDiff {
            path,
            status: status.into(),
            binary: false,
            base_content,
            target_content,
        });
    }
    Ok(diffs)
}

/// Materialize one explicit provider version into “我的技能”. This is a copy,
/// not a provider installation: the result is an independent pure package and
/// receives one local Git baseline only after every file has been verified.
pub async fn copy_catalog_version_to_mine(
    root: &Path,
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    version: &str,
    target_slug: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    if space == SkillSpace::Mine {
        return Err(ExtensionError::InvalidRequest("该技能已在“我的技能”中".to_owned()));
    }
    let target = root.join(target_slug);
    if target.exists() {
        return Err(ExtensionError::InvalidRequest(format!("技能 {target_slug} 已存在")));
    }
    tokio::fs::create_dir_all(root).await?;
    let staging = unique_staging_path(root, target_slug, "copy");
    let result = async {
        write_catalog_snapshot(
            &staging,
            CatalogSnapshotRequest {
                mine_root: root,
                space,
                namespace,
                slug,
                version,
                target_slug,
            },
            git.clone(),
        )
        .await?;
        tokio::fs::rename(&staging, &target).await?;
        normalize_skill_workspace(
            &target,
            target_slug,
            git,
            &format!(
                "chore(skill): 复制 {source}/{namespace}/{slug}@{version}",
                source = space.id()
            ),
        )
        .await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }
    result
}

/// Return the runtime directory for an enabled source/version. Remote sources
/// are cached privately and never become “我的技能”.
pub async fn ensure_runtime_snapshot(
    mine_root: &Path,
    cache_root: &Path,
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    version: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<PathBuf, ExtensionError> {
    if space == SkillSpace::Mine {
        let skill = crate::load_installed_skill(&mine_root.join(slug)).await?;
        if skill.version != version {
            return Err(ExtensionError::InvalidVersion {
                version: version.to_owned(),
                reason: "“我的技能”当前只有工作副本版本".to_owned(),
            });
        }
        return Ok(skill.path);
    }

    let target = runtime_snapshot_path(cache_root, space, namespace, slug, version);
    if let Ok(skill) = load_skill_from_directory(&target, slug).await
        && skill.version == version
    {
        return Ok(skill.path);
    }
    tokio::fs::create_dir_all(target.parent().unwrap_or(cache_root)).await?;
    let staging = unique_staging_path(target.parent().unwrap_or(cache_root), slug, "runtime");
    let result = async {
        write_catalog_snapshot(
            &staging,
            CatalogSnapshotRequest {
                mine_root,
                space,
                namespace,
                slug,
                version,
                target_slug: slug,
            },
            git,
        )
        .await?;
        match tokio::fs::rename(&staging, &target).await {
            Ok(()) => Ok(target.clone()),
            Err(_error) if target.is_dir() => {
                let _ = tokio::fs::remove_dir_all(&staging).await;
                Ok(target.clone())
            }
            Err(error) => Err(ExtensionError::Io(error)),
        }
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    result
}

pub fn runtime_skill_path(
    mine_root: &Path,
    cache_root: &Path,
    source: &str,
    namespace: &str,
    slug: &str,
    version: &str,
) -> Result<PathBuf, ExtensionError> {
    let space = SkillSpace::parse(source)?;
    Ok(if space == SkillSpace::Mine {
        mine_root.join(slug)
    } else {
        runtime_snapshot_path(cache_root, space, namespace, slug, version)
    })
}

fn runtime_snapshot_path(root: &Path, space: SkillSpace, namespace: &str, slug: &str, version: &str) -> PathBuf {
    let identity = format!("{}\u{1f}{namespace}\u{1f}{slug}\u{1f}{version}", space.id());
    let digest = hex::encode(Sha256::digest(identity.as_bytes()));
    root.join(space.id()).join(&digest[..2]).join(digest)
}

fn unique_staging_path(parent: &Path, slug: &str, operation: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(".{operation}-{slug}-{nonce}"))
}

struct CatalogSnapshotRequest<'a> {
    mine_root: &'a Path,
    space: SkillSpace,
    namespace: &'a str,
    slug: &'a str,
    version: &'a str,
    target_slug: &'a str,
}

async fn write_catalog_snapshot(
    target: &Path,
    request: CatalogSnapshotRequest<'_>,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    if target.exists() {
        tokio::fs::remove_dir_all(target).await?;
    }
    let transport_namespace = resolve_transport_namespace(request.space, request.namespace, request.slug).await?;
    let detail = catalog_detail(
        request.mine_root,
        request.space,
        &transport_namespace,
        request.slug,
        Some(request.version),
        git.clone(),
    )
    .await?;
    if detail.files.len() > MAX_FILE_COUNT {
        return Err(ExtensionError::InvalidRequest("技能文件数量超过 2000".to_owned()));
    }
    if request.space == SkillSpace::TjuaeHub {
        let index = tjuae_hub_index().await?;
        let entry = index
            .skills
            .iter()
            .find(|entry| entry.id == request.slug)
            .ok_or_else(|| ExtensionError::SkillNotFound(request.slug.to_owned()))?;
        let selected = entry
            .version(request.version)
            .ok_or_else(|| ExtensionError::InvalidVersion {
                version: request.version.to_owned(),
                reason: "TjuaeHub 索引中没有这个版本".to_owned(),
            })?;
        git.materialize_repository_path(&index.repository, &selected.revision, &entry.path, target)
            .await
            .map_err(ExtensionError::Internal)?;
        let mut total = 0_u64;
        for file in &detail.files {
            validate_catalog_file(&file.path, file.size)?;
            let bytes = tokio::fs::read(target.join(Path::new(&file.path))).await?;
            total = total.saturating_add(bytes.len() as u64);
            if total > MAX_PACKAGE_SIZE {
                return Err(ExtensionError::InvalidRequest("技能包超过 20 MB".to_owned()));
            }
            if let Some(expected) = file.sha256.as_deref() {
                let actual = hex::encode(Sha256::digest(&bytes));
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(ExtensionError::InvalidRequest(format!(
                        "技能文件校验失败：{}",
                        file.path
                    )));
                }
            }
        }
        return seal_skill_package(target, request.target_slug, request.version, detail.skill.categories).await;
    }
    tokio::fs::create_dir_all(target).await?;
    let mut total = 0_u64;
    for file in &detail.files {
        if file.path == "_meta.json" {
            continue;
        }
        validate_catalog_file(&file.path, file.size)?;
        let bytes = catalog_file_bytes(
            request.mine_root,
            request.space,
            &transport_namespace,
            request.slug,
            &file.path,
            Some(request.version),
            git.clone(),
        )
        .await?;
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_PACKAGE_SIZE {
            return Err(ExtensionError::InvalidRequest("技能包超过 20 MB".to_owned()));
        }
        if let Some(expected) = file.sha256.as_deref() {
            let actual = hex::encode(Sha256::digest(&bytes));
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(ExtensionError::InvalidRequest(format!(
                    "技能文件校验失败：{}",
                    file.path
                )));
            }
        }
        let destination = target.join(Path::new(&file.path));
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(destination, bytes).await?;
    }
    if !target.join(SKILL_ENTRY_FILE).is_file() {
        let entry = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            yaml_scalar(&detail.skill.name),
            yaml_scalar(&detail.skill.description),
            detail.readme
        );
        tokio::fs::write(target.join(SKILL_ENTRY_FILE), entry).await?;
    }
    seal_skill_package(target, request.target_slug, request.version, detail.skill.categories).await
}

pub async fn publish_mine_to_tjuae_hub(
    mine_root: &Path,
    hub_worktree: &Path,
    slug: &str,
    version: &str,
    target_slug: &str,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    let skills_root = hub_worktree.join("skills");
    tokio::fs::create_dir_all(&skills_root).await?;
    let target = skills_root.join(target_slug);
    if target.exists() {
        return Err(ExtensionError::InvalidRequest(format!(
            "TjuaeHub 技能 {target_slug} 已存在"
        )));
    }
    let staging = unique_staging_path(&skills_root, target_slug, "publishing");
    let result = write_catalog_snapshot(
        &staging,
        CatalogSnapshotRequest {
            mine_root,
            space: SkillSpace::Mine,
            namespace: "local",
            slug,
            version,
            target_slug,
        },
        git,
    )
    .await;
    match result {
        Ok(_) => {
            tokio::fs::rename(&staging, &target).await?;
            crate::load_installed_skill(&target).await
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            Err(error)
        }
    }
}

fn list_mine(
    installed: Vec<InstalledSkill>,
    query: &str,
    sort: &str,
    cursor: Option<&str>,
    limit: u32,
) -> Result<CatalogPage, ExtensionError> {
    let mut items = installed
        .into_iter()
        .map(|skill| local_catalog_item(&skill))
        .filter(|skill| matches_query(skill, query))
        .collect::<Vec<_>>();
    sort_catalog(&mut items, sort);
    paginate(items, cursor, limit)
}

async fn list_tjuae_hub(
    query: &str,
    sort: &str,
    cursor: Option<&str>,
    limit: u32,
) -> Result<CatalogPage, ExtensionError> {
    let index = tjuae_hub_index().await?;
    let mut items = Vec::with_capacity(index.skills.len());
    for entry in &index.skills {
        let latest = entry
            .latest()
            .ok_or_else(|| ExtensionError::InvalidRequest(format!("TjuaeHub 技能 {} 缺少最新版本", entry.id)))?;
        items.push(CatalogSkill {
            id: format!("tjuae-hub:{}", entry.id),
            space: SkillSpace::TjuaeHub,
            slug: entry.id.clone(),
            namespace: "official".to_owned(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            version: Some(latest.version.clone()),
            categories: entry.categories.clone(),
            icon_url: None,
            author: Some("TjuaeHub".to_owned()),
        });
    }
    items.retain(|skill| matches_query(skill, query));
    sort_catalog(&mut items, sort);
    paginate(items, cursor, limit)
}

async fn list_skillhub(query: &str, cursor: Option<&str>, limit: u32) -> Result<CatalogPage, ExtensionError> {
    let page = cursor.and_then(|value| value.parse::<u32>().ok()).unwrap_or(1).max(1);
    let mut url = Url::parse(&format!("{SKILLHUB_BASE_URL}/api/skills"))
        .map_err(|error| ExtensionError::Internal(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("page", &page.to_string())
        .append_pair("pageSize", &limit.to_string());
    if !query.trim().is_empty() {
        url.query_pairs_mut().append_pair("keyword", query.trim());
    }
    let envelope: SkillHubEnvelope<SkillHubListData> = get_json(url).await?;
    if envelope.code != 0 {
        return Err(ExtensionError::Internal(format!(
            "SkillHub 返回错误：{}",
            envelope.message.unwrap_or_default()
        )));
    }
    let total = envelope.data.total.unwrap_or(envelope.data.skills.len() as u64);
    let items = envelope
        .data
        .skills
        .into_iter()
        .filter(|item| !item.slug.is_empty())
        .map(skillhub_item)
        .collect::<Vec<_>>();
    Ok(CatalogPage {
        items,
        total,
        next_cursor: ((page as u64) * u64::from(limit) < total).then(|| (page + 1).to_string()),
    })
}

async fn list_clawhub(query: &str, cursor: Option<&str>, limit: u32) -> Result<CatalogPage, ExtensionError> {
    if !query.trim().is_empty() {
        let mut url = Url::parse(&format!("{CLAWHUB_BASE_URL}/api/v1/search"))
            .map_err(|error| ExtensionError::Internal(error.to_string()))?;
        url.query_pairs_mut().append_pair("q", query.trim());
        let data: ClawHubSearchResponse = get_json(url).await?;
        let items = data
            .results
            .into_iter()
            .take(limit as usize)
            .map(clawhub_search_item)
            .collect::<Vec<_>>();
        return Ok(CatalogPage {
            total: items.len() as u64,
            items,
            next_cursor: None,
        });
    }
    let mut url = Url::parse(&format!("{CLAWHUB_BASE_URL}/api/v1/skills"))
        .map_err(|error| ExtensionError::Internal(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("limit", &limit.to_string())
        .append_pair("sort", "downloads");
    if let Some(cursor) = cursor.filter(|value| !value.is_empty()) {
        url.query_pairs_mut().append_pair("cursor", cursor);
    }
    let data: ClawHubListResponse = get_json(url).await?;
    let items = data.items.into_iter().map(clawhub_item).collect::<Vec<_>>();
    let total = items.len() as u64;
    Ok(CatalogPage {
        items,
        total,
        next_cursor: data.next_cursor,
    })
}

struct MineVersionSnapshot {
    manifest: SkillManifest,
    files: Vec<WorkspaceRevisionFile>,
}

async fn mine_version_snapshots(
    directory: &Path,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<Vec<MineVersionSnapshot>, ExtensionError> {
    let commits = git
        .workspace_revision_history(directory, SKILL_PACKAGE_MANIFEST, 256)
        .await
        .map_err(ExtensionError::Internal)?;
    let mut versions = BTreeSet::new();
    let mut snapshots = Vec::new();
    for commit in commits {
        let files = git
            .workspace_revision_files(directory, &commit.revision)
            .await
            .map_err(ExtensionError::Internal)?;
        let Some(manifest_file) = files.iter().find(|file| file.path == SKILL_PACKAGE_MANIFEST) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<SkillManifest>(&manifest_file.content) else {
            continue;
        };
        if versions.insert(manifest.version.clone()) {
            snapshots.push(MineVersionSnapshot { manifest, files });
        }
    }
    Ok(snapshots)
}

fn catalog_files_from_revision(files: &[WorkspaceRevisionFile]) -> Vec<CatalogFile> {
    files
        .iter()
        .map(|file| CatalogFile {
            path: file.path.clone(),
            size: file.size,
            sha256: Some(format!("{:x}", Sha256::digest(&file.content))),
        })
        .collect()
}

async fn mine_detail(
    root: &Path,
    slug: &str,
    requested_version: Option<&str>,
    installed: Vec<InstalledSkill>,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<CatalogDetail, ExtensionError> {
    let skill = installed
        .into_iter()
        .find(|item| item.slug == slug)
        .ok_or_else(|| ExtensionError::SkillNotFound(slug.to_owned()))?;
    let directory = root.join(slug);
    git.ensure_workspace_git(&directory)
        .await
        .map_err(ExtensionError::Internal)?;
    let snapshots = mine_version_snapshots(&directory, git).await?;
    let versions = unique_versions(
        std::iter::once(skill.version.clone())
            .chain(snapshots.iter().map(|snapshot| snapshot.manifest.version.clone())),
    );
    if let Some(version) = requested_version
        && version != skill.version
    {
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.manifest.version == version)
            .ok_or_else(|| ExtensionError::InvalidVersion {
                version: version.to_owned(),
                reason: "这个本地技能没有该版本".to_owned(),
            })?;
        let readme = snapshot
            .files
            .iter()
            .find(|file| file.path == SKILL_ENTRY_FILE)
            .and_then(|file| String::from_utf8(file.content.clone()).ok())
            .ok_or_else(|| ExtensionError::InvalidRequest("技能版本缺少 SKILL.md".to_owned()))?;
        let mut historical = local_catalog_item(&skill);
        historical.version = Some(snapshot.manifest.version.clone());
        historical.categories.clone_from(&snapshot.manifest.categories);
        return Ok(CatalogDetail {
            skill: historical,
            readme: strip_frontmatter(&readme),
            files: catalog_files_from_revision(&snapshot.files),
            versions,
            security_reports: Vec::new(),
        });
    }
    let readme = tokio::fs::read_to_string(directory.join(SKILL_ENTRY_FILE)).await?;
    let files = list_local_files(&directory)?;
    Ok(CatalogDetail {
        skill: local_catalog_item(&skill),
        readme: strip_frontmatter(&readme),
        files,
        versions,
        security_reports: Vec::new(),
    })
}

async fn tjuae_hub_detail(slug: &str, requested_version: Option<&str>) -> Result<CatalogDetail, ExtensionError> {
    let index = tjuae_hub_index().await?;
    let entry = index
        .skills
        .iter()
        .find(|entry| entry.id == slug)
        .ok_or_else(|| ExtensionError::SkillNotFound(slug.to_owned()))?;
    let selected_version = requested_version.unwrap_or(&entry.latest_version);
    if let Some(skills_root) = crate::skill_storage::resolve_tjuae_hub_worktree().map(|root| root.join("skills")) {
        let directory = skills_root.join(slug);
        if let Ok(skill) = crate::load_installed_skill(&directory).await
            && requested_version.is_none_or(|requested| requested == skill.version)
        {
            let readme = tokio::fs::read_to_string(directory.join(SKILL_ENTRY_FILE)).await?;
            return Ok(CatalogDetail {
                skill: CatalogSkill {
                    id: format!("tjuae-hub:{}", entry.id),
                    space: SkillSpace::TjuaeHub,
                    slug: entry.id.clone(),
                    namespace: "official".into(),
                    name: skill.name,
                    description: skill.description,
                    version: Some(skill.version.clone()),
                    categories: skill.categories,
                    icon_url: skill.icon_url,
                    author: Some("TjuaeHub".to_owned()),
                },
                readme: strip_frontmatter(&readme),
                files: list_local_files(&directory)?,
                versions: unique_versions(
                    std::iter::once(skill.version.clone())
                        .chain(entry.versions.iter().map(|version| version.version.clone())),
                ),
                security_reports: vec![CatalogSecurityReport {
                    provider: "TjuaeHub".to_owned(),
                    status: "verified".to_owned(),
                    label: "官方技能仓库已校验".to_owned(),
                    url: None,
                }],
            });
        }
    }
    let selected = entry
        .version(selected_version)
        .ok_or_else(|| ExtensionError::InvalidVersion {
            version: selected_version.to_owned(),
            reason: "TjuaeHub 索引中没有这个版本".into(),
        })?;
    Ok(CatalogDetail {
        skill: CatalogSkill {
            id: format!("tjuae-hub:{}", entry.id),
            space: SkillSpace::TjuaeHub,
            slug: entry.id.clone(),
            namespace: "official".into(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            version: Some(entry.latest_version.clone()),
            categories: entry.categories.clone(),
            icon_url: None,
            author: Some("TjuaeHub".to_owned()),
        },
        readme: selected.readme.clone(),
        files: selected
            .files
            .iter()
            .map(|file| CatalogFile {
                path: file.path.clone(),
                size: file.size,
                sha256: Some(file.sha256.clone()),
            })
            .collect(),
        versions: unique_versions(entry.versions.iter().map(|version| version.version.clone())),
        security_reports: vec![CatalogSecurityReport {
            provider: "TjuaeHub".to_owned(),
            status: "verified".to_owned(),
            label: "官方技能仓库已校验".to_owned(),
            url: None,
        }],
    })
}

async fn skillhub_detail(
    namespace: &str,
    slug: &str,
    requested_version: Option<&str>,
) -> Result<CatalogDetail, ExtensionError> {
    let url = skill_detail_url(SKILLHUB_BASE_URL, slug)?;
    let data: SkillHubDetail = get_json(url).await?;
    if data.skill.slug.is_empty() {
        return Err(ExtensionError::SkillNotFound(slug.to_owned()));
    }
    let version_data = skillhub_versions(slug).await?;
    let versions = unique_versions(version_data.versions.iter().map(|value| value.version.clone()));
    let selected_version = requested_version
        .or_else(|| versions.first().map(String::as_str))
        .ok_or_else(|| ExtensionError::InvalidVersion {
            version: String::new(),
            reason: "SkillHub 未返回版本".into(),
        })?;
    if !versions.iter().any(|version| version == selected_version) {
        return Err(ExtensionError::InvalidVersion {
            version: selected_version.into(),
            reason: "SkillHub 没有这个版本".into(),
        });
    }
    let files = skillhub_files(slug, selected_version).await?;
    let readme = match files.iter().find(|file| file.path == SKILL_ENTRY_FILE) {
        Some(_) => {
            fetch_external_file(
                SkillSpace::SkillHub,
                namespace,
                slug,
                SKILL_ENTRY_FILE,
                Some(selected_version),
            )
            .await?
        }
        None => data
            .skill
            .summary_zh
            .clone()
            .or(data.skill.summary.clone())
            .unwrap_or_default(),
    };
    let version = data
        .latest_version
        .as_ref()
        .and_then(|value| value.version.clone())
        .or_else(|| data.skill.version.clone());
    let list_item = SkillHubListItem {
        slug: data.skill.slug.clone(),
        name: data.skill.display_name.clone(),
        display_name: data.skill.display_name.clone(),
        description: data.skill.summary.clone(),
        description_zh: data.skill.summary_zh.clone(),
        category: data.skill.category.clone(),
        sub_categories: data.skill.sub_categories.clone(),
        icon_url: data.skill.icon_url.clone(),
        owner_name: data.owner.as_ref().and_then(|owner| owner.handle.clone()),
        namespace: Some(version_data.namespace.clone()),
        version: version.clone(),
    };
    let mut item = skillhub_item(list_item);
    item.namespace = version_data.namespace.handle.clone();
    item.author = data
        .owner
        .as_ref()
        .and_then(|owner| owner.display_name.clone().or(owner.handle.clone()));
    let security_reports = data
        .security_reports
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(provider, report)| {
            report.map(|report| CatalogSecurityReport {
                provider,
                status: report.status.clone().unwrap_or_else(|| "unknown".to_owned()),
                label: report
                    .status_text
                    .or(report.status)
                    .unwrap_or_else(|| "未知".to_owned()),
                url: report.report_url,
            })
        })
        .collect();
    Ok(CatalogDetail {
        skill: item,
        readme: strip_frontmatter(&readme),
        files,
        versions,
        security_reports,
    })
}

async fn clawhub_detail(
    namespace: &str,
    slug: &str,
    requested_version: Option<&str>,
) -> Result<CatalogDetail, ExtensionError> {
    let resolved_namespace = if namespace.is_empty() {
        resolve_clawhub_owner(slug).await?
    } else {
        namespace.to_owned()
    };
    let response =
        clawhub_json::<ClawHubDetail>(skill_detail_url(CLAWHUB_BASE_URL, slug)?, slug, &resolved_namespace).await?;
    if response.skill.slug.is_empty() {
        return Err(ExtensionError::SkillNotFound(slug.to_owned()));
    }
    let latest_version = response
        .latest_version
        .as_ref()
        .and_then(|value| value.version.clone())
        .or_else(|| {
            response
                .skill
                .latest_version
                .as_ref()
                .and_then(|value| value.version.clone())
        });
    let version_list = clawhub_versions(slug, &resolved_namespace).await?;
    let versions = unique_versions(version_list.items.iter().map(|item| item.version.clone()));
    let selected_version = requested_version
        .map(str::to_owned)
        .or_else(|| latest_version.clone())
        .or_else(|| versions.first().cloned());
    if requested_version.is_some_and(|requested| !versions.iter().any(|version| version == requested)) {
        return Err(ExtensionError::InvalidVersion {
            version: requested_version.unwrap_or_default().into(),
            reason: "ClawHub 没有这个版本".into(),
        });
    }
    let version_detail = match selected_version.as_deref() {
        Some(version) => clawhub_version(slug, version, &resolved_namespace).await.ok(),
        None => None,
    };
    let files = version_detail
        .as_ref()
        .and_then(|value| value.version.as_ref())
        .map(|value| value.files.clone())
        .unwrap_or_default();
    let readme = if files.iter().any(|file| file.path == SKILL_ENTRY_FILE) {
        fetch_external_file(
            SkillSpace::ClawHub,
            &resolved_namespace,
            slug,
            SKILL_ENTRY_FILE,
            selected_version.as_deref(),
        )
        .await?
    } else {
        response.skill.description.clone().unwrap_or_default()
    };
    let mut item = clawhub_item(response.skill);
    // The popular ClawHub feed does not expose an owner. Keep its public
    // identity namespace empty so card preferences, detail navigation and
    // runtime cache keys remain the same identity. The resolved owner is an
    // internal transport detail only; search results that provide an owner
    // continue to use that explicit namespace.
    item.namespace = namespace.to_owned();
    item.version = latest_version;
    item.author = response
        .owner
        .as_ref()
        .and_then(|owner| owner.display_name.clone().or(owner.handle.clone()));
    let security_reports = version_detail
        .as_ref()
        .and_then(|value| value.version.as_ref())
        .and_then(|value| value.security.as_ref())
        .and_then(|security| security.status.clone().map(|status| (status, security)))
        .map(|(status, security)| {
            vec![CatalogSecurityReport {
                provider: "ClawHub".to_owned(),
                status: status.clone(),
                label: if status.eq_ignore_ascii_case("clean") {
                    "安全扫描通过".to_owned()
                } else {
                    format!("扫描状态：{status}")
                },
                url: security.virustotal_url.clone(),
            }]
        })
        .unwrap_or_default();
    Ok(CatalogDetail {
        skill: item,
        readme: strip_frontmatter(&readme),
        files: files
            .into_iter()
            .map(|file| CatalogFile {
                path: file.path,
                size: file.size,
                sha256: file.sha256,
            })
            .collect(),
        versions,
        security_reports,
    })
}

async fn resolve_clawhub_owner(slug: &str) -> Result<String, ExtensionError> {
    let mut url = Url::parse(&format!("{CLAWHUB_BASE_URL}/api/v1/search"))
        .map_err(|error| ExtensionError::Internal(error.to_string()))?;
    url.query_pairs_mut().append_pair("q", slug);
    let response: ClawHubSearchResponse = get_json(url).await?;
    preferred_clawhub_owner(response.results, slug).ok_or_else(|| ExtensionError::SkillNotFound(slug.to_owned()))
}

/// ClawHub's popular feed omits the owner even though its detail/file API
/// requires one for ambiguous slugs. Keep the public card identity unchanged,
/// but resolve the owner once at the start of an operation and carry it through
/// every transport request.
async fn resolve_transport_namespace(space: SkillSpace, namespace: &str, slug: &str) -> Result<String, ExtensionError> {
    if space == SkillSpace::ClawHub && namespace.is_empty() {
        resolve_clawhub_owner(slug).await
    } else {
        Ok(namespace.to_owned())
    }
}

fn preferred_clawhub_owner(items: Vec<ClawHubSearchItem>, slug: &str) -> Option<String> {
    items
        .into_iter()
        .filter(|item| item.slug == slug)
        .max_by_key(|item| item.downloads.unwrap_or_default())
        .and_then(|item| item.owner.and_then(|owner| owner.handle).or(item.owner_handle))
        .filter(|owner| !owner.trim().is_empty())
}

fn local_catalog_item(skill: &InstalledSkill) -> CatalogSkill {
    CatalogSkill {
        id: format!("mine:{}", skill.slug),
        space: SkillSpace::Mine,
        slug: skill.slug.clone(),
        namespace: "local".to_owned(),
        name: skill.name.clone(),
        description: skill.description.clone(),
        version: Some(skill.version.clone()),
        categories: skill.categories.clone(),
        icon_url: skill.icon_url.clone(),
        author: None,
    }
}

fn skillhub_item(item: SkillHubListItem) -> CatalogSkill {
    CatalogSkill {
        id: format!("skillhub:{}", item.slug),
        space: SkillSpace::SkillHub,
        slug: item.slug.clone(),
        namespace: item
            .namespace
            .as_ref()
            .map(|namespace| namespace.handle.clone())
            .or_else(|| item.owner_name.clone())
            .unwrap_or_default(),
        name: item.name.or(item.display_name).unwrap_or_else(|| item.slug.clone()),
        description: item.description_zh.or(item.description).unwrap_or_default(),
        version: item.version,
        categories: item
            .sub_categories
            .unwrap_or_default()
            .into_iter()
            .filter_map(|category| category.name)
            .chain(item.category)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        icon_url: item.icon_url,
        author: item.owner_name,
    }
}

fn clawhub_item(item: ClawHubListItem) -> CatalogSkill {
    CatalogSkill {
        id: format!("clawhub:{}", item.slug),
        space: SkillSpace::ClawHub,
        slug: item.slug.clone(),
        namespace: String::new(),
        name: item.display_name.clone().unwrap_or_else(|| item.slug.clone()),
        description: item.summary.clone().or(item.description.clone()).unwrap_or_default(),
        version: item.latest_version.as_ref().and_then(|value| value.version.clone()),
        categories: item.topics.unwrap_or_default(),
        icon_url: None,
        author: None,
    }
}

fn clawhub_search_item(item: ClawHubSearchItem) -> CatalogSkill {
    CatalogSkill {
        id: format!("clawhub:{}", item.slug),
        space: SkillSpace::ClawHub,
        slug: item.slug.clone(),
        namespace: item
            .owner
            .as_ref()
            .and_then(|owner| owner.handle.clone())
            .or_else(|| item.owner_handle.clone())
            .unwrap_or_default(),
        name: item.display_name.unwrap_or_else(|| item.slug.clone()),
        description: item.summary.unwrap_or_default(),
        version: None,
        categories: Vec::new(),
        icon_url: item.owner.as_ref().and_then(|owner| owner.image.clone()),
        author: item
            .owner
            .and_then(|owner| owner.display_name.or(owner.handle))
            .or(item.owner_handle),
    }
}

fn matches_query(skill: &CatalogSkill, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || skill.name.to_lowercase().contains(&query)
        || skill.description.to_lowercase().contains(&query)
        || skill
            .categories
            .iter()
            .any(|category| category.to_lowercase().contains(&query))
}

fn sort_catalog(items: &mut [CatalogSkill], _sort: &str) {
    items.sort_by_key(|item| item.name.to_lowercase());
}

fn paginate(items: Vec<CatalogSkill>, cursor: Option<&str>, limit: u32) -> Result<CatalogPage, ExtensionError> {
    let offset = cursor
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| ExtensionError::InvalidRequest("技能目录游标无效".to_owned()))?;
    let total = items.len();
    let page = items.into_iter().skip(offset).take(limit as usize).collect::<Vec<_>>();
    let next = offset + page.len();
    Ok(CatalogPage {
        items: page,
        total: total as u64,
        next_cursor: (next < total).then(|| next.to_string()),
    })
}

fn list_local_files(root: &Path) -> Result<Vec<CatalogFile>, ExtensionError> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<CatalogFile>) -> Result<(), ExtensionError> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_name() == ".git" || entry.file_name() == "node_modules" {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                visit(root, &entry.path(), files)?;
            } else if file_type.is_file() {
                let path = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| ExtensionError::Internal(error.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(CatalogFile {
                    path,
                    size: entry.metadata()?.len(),
                    sha256: None,
                });
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn strip_frontmatter(source: &str) -> String {
    let normalized = source.replace("\r\n", "\n");
    normalized
        .strip_prefix("---\n")
        .and_then(|value| value.split_once("\n---\n").map(|(_, body)| body.trim().to_owned()))
        .unwrap_or_else(|| normalized.trim().to_owned())
}

fn validate_catalog_file(path: &str, size: u64) -> Result<(), ExtensionError> {
    let value = Path::new(path);
    if path.is_empty()
        || path.contains('\\')
        || value.is_absolute()
        || value
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ExtensionError::SkillImportInvalidSource(format!(
            "外部技能文件路径无效：{path}"
        )));
    }
    if size > MAX_FILE_SIZE {
        return Err(ExtensionError::InvalidRequest(format!("技能文件过大：{path}")));
    }
    Ok(())
}

fn yaml_scalar(value: &str) -> String {
    format!("{:?}", value.trim())
}

async fn tjuae_hub_index() -> Result<crate::MarketIndex, ExtensionError> {
    crate::tjuae_hub_index().await
}

fn catalog_client() -> Result<Client, ExtensionError> {
    tjuaeui_runtime::build_http_client(CATALOG_CONNECT_TIMEOUT, CATALOG_REQUEST_TIMEOUT)
        .map_err(|error| ExtensionError::Internal(format!("创建技能目录客户端失败：{error}")))
}

async fn get_json<T: for<'de> Deserialize<'de>>(url: Url) -> Result<T, ExtensionError> {
    let response = catalog_client()?
        .get(url.clone())
        .send()
        .await
        .map_err(|error| ExtensionError::Internal(format!("读取技能目录失败：{error}")))?;
    response_json(response, &url).await
}

async fn response_json<T: for<'de> Deserialize<'de>>(response: Response, url: &Url) -> Result<T, ExtensionError> {
    let response = response
        .error_for_status()
        .map_err(|error| ExtensionError::Internal(format!("技能目录返回错误（{}）：{error}", url.path())))?;
    response
        .json::<T>()
        .await
        .map_err(|error| ExtensionError::Internal(format!("解析技能目录失败（{}）：{error}", url.path())))
}

fn skill_detail_url(base: &str, slug: &str) -> Result<Url, ExtensionError> {
    let mut url = Url::parse(base).map_err(|error| ExtensionError::Internal(error.to_string()))?;
    url.path_segments_mut()
        .map_err(|_| ExtensionError::Internal("技能目录地址不能作为基础地址".to_owned()))?
        .extend(["api", "v1", "skills", slug]);
    Ok(url)
}

async fn skillhub_files(slug: &str, version: &str) -> Result<Vec<CatalogFile>, ExtensionError> {
    let mut url = skill_detail_url(SKILLHUB_BASE_URL, slug)?;
    url.path_segments_mut()
        .map_err(|_| ExtensionError::Internal("SkillHub 地址无效".to_owned()))?
        .push("files");
    url.query_pairs_mut().append_pair("version", version);
    let data: SkillHubFiles = get_json(url).await?;
    Ok(data
        .files
        .into_iter()
        .map(|file| CatalogFile {
            path: file.path,
            size: file.size,
            sha256: file.sha256,
        })
        .collect())
}

async fn skillhub_versions(slug: &str) -> Result<SkillHubVersions, ExtensionError> {
    let mut url = skill_detail_url(SKILLHUB_BASE_URL, slug)?;
    url.path_segments_mut()
        .map_err(|_| ExtensionError::Internal("SkillHub 地址无效".to_owned()))?
        .push("versions");
    get_json(url).await
}

async fn clawhub_versions(slug: &str, namespace: &str) -> Result<ClawHubVersions, ExtensionError> {
    let mut url = skill_detail_url(CLAWHUB_BASE_URL, slug)?;
    url.path_segments_mut()
        .map_err(|_| ExtensionError::Internal("ClawHub 地址无效".to_owned()))?
        .push("versions");
    clawhub_json(url, slug, namespace).await
}

async fn clawhub_version(slug: &str, version: &str, namespace: &str) -> Result<ClawHubVersionDetail, ExtensionError> {
    let mut url = skill_detail_url(CLAWHUB_BASE_URL, slug)?;
    url.path_segments_mut()
        .map_err(|_| ExtensionError::Internal("ClawHub 地址无效".to_owned()))?
        .extend(["versions", version]);
    clawhub_json(url, slug, namespace).await
}

async fn fetch_external_file(
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    path: &str,
    version: Option<&str>,
) -> Result<String, ExtensionError> {
    let bytes = fetch_external_file_bytes(space, namespace, slug, path, version).await?;
    String::from_utf8(bytes)
        .map_err(|_| ExtensionError::SkillImportInvalidSource(format!("技能文件不是 UTF-8 文本：{path}")))
}

async fn fetch_external_file_bytes(
    space: SkillSpace,
    namespace: &str,
    slug: &str,
    path: &str,
    version: Option<&str>,
) -> Result<Vec<u8>, ExtensionError> {
    validate_catalog_file(path, 0)?;
    let base = match space {
        SkillSpace::SkillHub => SKILLHUB_BASE_URL,
        SkillSpace::ClawHub => CLAWHUB_BASE_URL,
        _ => return Err(ExtensionError::InvalidRequest("该空间没有外部文件接口".to_owned())),
    };
    let mut url = skill_detail_url(base, slug)?;
    url.path_segments_mut()
        .map_err(|_| ExtensionError::Internal("技能文件地址无效".to_owned()))?
        .push("file");
    url.query_pairs_mut().append_pair("path", path);
    if let Some(version) = version.filter(|value| !value.is_empty()) {
        url.query_pairs_mut().append_pair("version", version);
    }
    if space == SkillSpace::ClawHub && !namespace.is_empty() {
        url.query_pairs_mut().append_pair("owner", namespace);
    }
    let response = if space == SkillSpace::ClawHub {
        clawhub_response(url.clone(), slug, namespace).await?
    } else {
        catalog_client()?
            .get(url.clone())
            .send()
            .await
            .map_err(|error| ExtensionError::Internal(format!("读取技能文件失败：{error}")))?
    };
    let response = response
        .error_for_status()
        .map_err(|error| ExtensionError::Internal(format!("技能文件返回错误（{}）：{error}", url.path())))?;
    if response.content_length().is_some_and(|length| length > MAX_FILE_SIZE) {
        return Err(ExtensionError::InvalidRequest(format!("技能文件过大：{path}")));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ExtensionError::Internal(format!("读取技能文件失败：{error}")))?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(ExtensionError::InvalidRequest(format!("技能文件过大：{path}")));
    }
    Ok(bytes.to_vec())
}

async fn read_local_catalog_file_bytes(root: &Path, slug: &str, path: &str) -> Result<Vec<u8>, ExtensionError> {
    let installed = list_installed_skills(root).await?;
    if !installed.iter().any(|skill| skill.slug == slug) {
        return Err(ExtensionError::SkillNotFound(slug.to_owned()));
    }
    let workspace = tokio::fs::canonicalize(root.join(slug)).await?;
    let target = tokio::fs::canonicalize(workspace.join(path)).await?;
    if !target.starts_with(&workspace) {
        return Err(ExtensionError::InvalidRequest(format!("技能文件路径越界：{path}")));
    }
    let metadata = tokio::fs::metadata(&target).await?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_SIZE {
        return Err(ExtensionError::InvalidRequest(format!("技能文件不可读取：{path}")));
    }
    tokio::fs::read(target).await.map_err(ExtensionError::from)
}

async fn read_mine_catalog_file_bytes(
    root: &Path,
    slug: &str,
    path: &str,
    requested_version: Option<&str>,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<Vec<u8>, ExtensionError> {
    let installed = list_installed_skills(root).await?;
    let skill = installed
        .iter()
        .find(|skill| skill.slug == slug)
        .ok_or_else(|| ExtensionError::SkillNotFound(slug.to_owned()))?;
    if requested_version.is_none_or(|version| version == skill.version) {
        return read_local_catalog_file_bytes(root, slug, path).await;
    }
    let directory = root.join(slug);
    let selected = mine_version_snapshots(&directory, git)
        .await?
        .into_iter()
        .find(|snapshot| requested_version == Some(snapshot.manifest.version.as_str()))
        .ok_or_else(|| ExtensionError::InvalidVersion {
            version: requested_version.unwrap_or_default().to_owned(),
            reason: "这个本地技能没有该版本".to_owned(),
        })?;
    selected
        .files
        .into_iter()
        .find(|file| file.path == path)
        .map(|file| file.content)
        .ok_or_else(|| ExtensionError::InvalidRequest(format!("该技能版本中不存在文件：{path}")))
}

async fn fetch_tjuae_hub_file_bytes(
    slug: &str,
    path: &str,
    requested_version: Option<&str>,
) -> Result<Vec<u8>, ExtensionError> {
    let index = tjuae_hub_index().await?;
    let entry = index
        .skills
        .iter()
        .find(|entry| entry.id == slug)
        .ok_or_else(|| ExtensionError::SkillNotFound(slug.to_owned()))?;
    let selected_version = requested_version.unwrap_or(&entry.latest_version);
    let selected = entry
        .version(selected_version)
        .ok_or_else(|| ExtensionError::InvalidVersion {
            version: selected_version.to_owned(),
            reason: "TjuaeHub 索引中没有这个版本".into(),
        })?;
    let declared = selected
        .files
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| ExtensionError::InvalidRequest(format!("TjuaeHub 技能中不存在文件：{path}")))?;
    validate_catalog_file(path, declared.size)?;

    let repository = Url::parse(&index.repository)
        .map_err(|error| ExtensionError::InvalidRequest(format!("TjuaeHub 仓库地址无效：{error}")))?;
    if repository.host_str() != Some("github.com") {
        return Err(ExtensionError::InvalidRequest(
            "TjuaeHub 文件仓库必须位于 GitHub".to_owned(),
        ));
    }
    let segments = repository
        .path_segments()
        .map(|segments| segments.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 2 {
        return Err(ExtensionError::InvalidRequest("TjuaeHub 仓库地址格式无效".to_owned()));
    }
    let mut url = Url::parse("https://raw.githubusercontent.com/")
        .map_err(|error| ExtensionError::Internal(error.to_string()))?;
    {
        let mut target = url
            .path_segments_mut()
            .map_err(|_| ExtensionError::Internal("无法构造 TjuaeHub 文件地址".to_owned()))?;
        target.push(segments[0]);
        target.push(segments[1].trim_end_matches(".git"));
        target.push(&selected.revision);
        for part in Path::new(&entry.path).components() {
            if let Component::Normal(value) = part {
                target.push(&value.to_string_lossy());
            }
        }
        for part in Path::new(path).components() {
            if let Component::Normal(value) = part {
                target.push(&value.to_string_lossy());
            }
        }
    }
    let response = catalog_client()?
        .get(url.clone())
        .send()
        .await
        .map_err(|error| ExtensionError::Internal(format!("读取 TjuaeHub 文件失败：{error}")))?;
    let response = response
        .error_for_status()
        .map_err(|error| ExtensionError::Internal(format!("TjuaeHub 文件返回错误（{}）：{error}", url.path())))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ExtensionError::Internal(format!("读取 TjuaeHub 文件失败：{error}")))?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(ExtensionError::InvalidRequest(format!("技能文件过大：{path}")));
    }
    Ok(bytes.to_vec())
}

async fn clawhub_json<T: for<'de> Deserialize<'de>>(
    url: Url,
    slug: &str,
    namespace: &str,
) -> Result<T, ExtensionError> {
    let response = clawhub_response(url.clone(), slug, namespace).await?;
    response_json(response, &url).await
}

async fn clawhub_response(mut url: Url, slug: &str, namespace: &str) -> Result<Response, ExtensionError> {
    let client = catalog_client()?;
    if !namespace.is_empty() && !url.query_pairs().any(|(key, _)| key == "owner") {
        url.query_pairs_mut().append_pair("owner", namespace);
    }
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| ExtensionError::Internal(format!("读取 ClawHub 失败：{error}")))?;
    if response.status() != StatusCode::CONFLICT {
        return Ok(response);
    }
    let conflict: ClawHubConflict = response
        .json()
        .await
        .map_err(|error| ExtensionError::Internal(format!("解析 ClawHub 冲突失败：{error}")))?;
    let owners = conflict
        .matches
        .iter()
        .filter_map(|value| value.owner_handle.as_deref())
        .collect::<Vec<_>>();
    Err(ExtensionError::InvalidRequest(format!(
        "ClawHub 技能 {slug} 名称不唯一，请从搜索结果选择明确作者：{}",
        owners.join("、")
    )))
}

#[derive(Debug, Deserialize)]
struct SkillHubEnvelope<T> {
    code: i32,
    data: T,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillHubListData {
    #[serde(default)]
    skills: Vec<SkillHubListItem>,
    total: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillHubListItem {
    slug: String,
    name: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    #[serde(rename = "description_zh")]
    description_zh: Option<String>,
    category: Option<String>,
    sub_categories: Option<Vec<SkillHubCategory>>,
    icon_url: Option<String>,
    owner_name: Option<String>,
    namespace: Option<SkillHubVersionNamespace>,
    version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillHubCategory {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillHubDetail {
    skill: SkillHubDetailSkill,
    owner: Option<SkillHubOwner>,
    latest_version: Option<SkillHubVersion>,
    security_reports: Option<std::collections::BTreeMap<String, Option<SkillHubSecurity>>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillHubDetailSkill {
    slug: String,
    display_name: Option<String>,
    summary: Option<String>,
    #[serde(rename = "summary_zh")]
    summary_zh: Option<String>,
    category: Option<String>,
    sub_categories: Option<Vec<SkillHubCategory>>,
    icon_url: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillHubOwner {
    handle: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillHubVersion {
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillHubSecurity {
    status: Option<String>,
    status_text: Option<String>,
    report_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillHubFiles {
    #[serde(default)]
    files: Vec<ExternalFile>,
}

#[derive(Debug, Deserialize)]
struct SkillHubVersions {
    namespace: SkillHubVersionNamespace,
    #[serde(default)]
    versions: Vec<SkillHubVersionItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillHubVersionNamespace {
    handle: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillHubVersionItem {
    version: String,
    #[allow(dead_code)]
    created_at: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalFile {
    path: String,
    size: u64,
    sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubListResponse {
    #[serde(default)]
    items: Vec<ClawHubListItem>,
    next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubListItem {
    slug: String,
    display_name: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    topics: Option<Vec<String>>,
    latest_version: Option<ClawHubVersion>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClawHubVersion {
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClawHubSearchResponse {
    #[serde(default)]
    results: Vec<ClawHubSearchItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubSearchItem {
    slug: String,
    display_name: Option<String>,
    summary: Option<String>,
    downloads: Option<u64>,
    owner_handle: Option<String>,
    owner: Option<ClawHubOwner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubDetail {
    skill: ClawHubListItem,
    latest_version: Option<ClawHubVersion>,
    owner: Option<ClawHubOwner>,
}

#[derive(Debug, Deserialize)]
struct ClawHubOwner {
    handle: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    image: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClawHubVersionDetail {
    version: Option<ClawHubVersionData>,
}

#[derive(Debug, Deserialize)]
struct ClawHubVersionData {
    #[serde(default)]
    files: Vec<ExternalFile>,
    security: Option<ClawHubSecurity>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubVersions {
    #[serde(default)]
    items: Vec<ClawHubVersionItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubVersionItem {
    version: String,
    #[allow(dead_code)]
    created_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubSecurity {
    status: Option<String>,
    #[allow(dead_code)]
    has_warnings: Option<bool>,
    virustotal_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClawHubConflict {
    #[serde(default)]
    matches: Vec<ClawHubConflictMatch>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClawHubConflictMatch {
    owner_handle: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RejectGit;

    #[async_trait::async_trait]
    impl WorkspaceGitProvisioner for RejectGit {
        async fn ensure_workspace_git(
            &self,
            _workspace: &Path,
        ) -> Result<tjuaeui_common::WorkspaceGitProvision, String> {
            Err("cache reuse must not provision Git".to_owned())
        }
    }

    struct EmptyHistoryGit;

    #[async_trait::async_trait]
    impl WorkspaceGitProvisioner for EmptyHistoryGit {
        async fn ensure_workspace_git(
            &self,
            workspace: &Path,
        ) -> Result<tjuaeui_common::WorkspaceGitProvision, String> {
            Ok(tjuaeui_common::WorkspaceGitProvision {
                repository_root: workspace.to_string_lossy().into_owned(),
                workspace_path: workspace.to_string_lossy().into_owned(),
                branch: "main".to_owned(),
                head_commit: "test".to_owned(),
            })
        }

        async fn workspace_revision_history(
            &self,
            _workspace: &Path,
            _file_path: &str,
            _limit: usize,
        ) -> Result<Vec<tjuaeui_common::WorkspaceRevisionCommit>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn catalog_file_rejects_escape_and_oversized_input() {
        assert!(validate_catalog_file("../SKILL.md", 1).is_err());
        assert!(validate_catalog_file("folder\\SKILL.md", 1).is_err());
        assert!(validate_catalog_file("SKILL.md", MAX_FILE_SIZE + 1).is_err());
        assert!(validate_catalog_file("references/guide.md", 32).is_ok());
    }

    #[test]
    fn markdown_frontmatter_is_not_rendered_as_content() {
        assert_eq!(strip_frontmatter("---\nname: demo\n---\n# Demo\n"), "# Demo");
        assert_eq!(strip_frontmatter("# Demo\n"), "# Demo");
    }

    #[test]
    fn catalog_sort_names_match_the_ui_contract() {
        let skill = |name: &str| CatalogSkill {
            id: name.into(),
            space: SkillSpace::Mine,
            slug: name.into(),
            namespace: "local".into(),
            name: name.into(),
            description: String::new(),
            version: None,
            categories: vec![],
            icon_url: None,
            author: None,
        };
        let mut items = vec![skill("zeta"), skill("alpha")];
        sort_catalog(&mut items, "name");
        assert_eq!(items[0].name, "alpha");
    }

    #[test]
    fn provider_versions_are_unique_and_keep_provider_order() {
        assert_eq!(
            unique_versions([
                "1.0.3".to_owned(),
                "1.0.1".to_owned(),
                "1.0.2".to_owned(),
                "1.0.1".to_owned(),
                String::new(),
            ]),
            ["1.0.3", "1.0.1", "1.0.2"]
        );
    }

    #[test]
    fn skillhub_live_field_spellings_map_into_the_catalog_boundary() {
        let envelope: SkillHubEnvelope<SkillHubListData> = serde_json::from_str(
            r#"{
                "code": 0,
                "data": {
                    "total": 1,
                    "skills": [{
                        "slug": "demo",
                        "displayName": "Demo",
                        "description_zh": "中文说明",
                        "subCategories": [{"name": "开发"}],
                        "iconUrl": "https://example.com/icon.png",
                        "ownerName": "owner",
                        "updated_at": 123
                    }]
                }
            }"#,
        )
        .unwrap();
        let item = envelope.data.skills.into_iter().next().unwrap();
        assert_eq!(item.description_zh.as_deref(), Some("中文说明"));
        assert_eq!(item.owner_name.as_deref(), Some("owner"));
    }

    #[test]
    fn clawhub_ambiguous_slug_uses_the_most_used_exact_identity() {
        let response: ClawHubSearchResponse = serde_json::from_str(
            r#"{
                "results": [
                    {"slug":"demo","downloads":12,"ownerHandle":"small"},
                    {"slug":"other","downloads":999,"ownerHandle":"wrong"},
                    {"slug":"demo","downloads":42,"owner":{"handle":"preferred"}}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(
            preferred_clawhub_owner(response.results, "demo").as_deref(),
            Some("preferred")
        );
    }

    #[tokio::test]
    async fn runtime_cache_directory_uses_the_public_slug_for_validation() {
        let temp = tempfile::tempdir().unwrap();
        let mine_root = temp.path().join("mine");
        let cache_root = temp.path().join("runtime");
        let target = runtime_snapshot_path(&cache_root, SkillSpace::ClawHub, "", "github", "1.0.0");
        tokio::fs::create_dir_all(&target).await.unwrap();
        tokio::fs::write(
            target.join(SKILL_ENTRY_FILE),
            "---\nname: Github\ndescription: cached skill\n---\n\n# Github\n",
        )
        .await
        .unwrap();
        seal_skill_package(&target, "github", "1.0.0", vec![]).await.unwrap();

        let resolved = ensure_runtime_snapshot(
            &mine_root,
            &cache_root,
            SkillSpace::ClawHub,
            "",
            "github",
            "1.0.0",
            Arc::new(RejectGit),
        )
        .await
        .unwrap();

        assert_eq!(resolved, std::fs::canonicalize(target).unwrap());
    }

    #[tokio::test]
    async fn publishing_mine_creates_one_pure_hub_package() {
        let temp = tempfile::tempdir().unwrap();
        let mine_root = temp.path().join("mine");
        let source = mine_root.join("draft");
        let hub_root = temp.path().join("hub");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(
            source.join(SKILL_ENTRY_FILE),
            "---\nname: Draft\ndescription: public skill\n---\n\n# Draft\n",
        )
        .await
        .unwrap();
        seal_skill_package(&source, "draft", "1.2.3", vec!["development".into()])
            .await
            .unwrap();

        let published = publish_mine_to_tjuae_hub(
            &mine_root,
            &hub_root,
            "draft",
            "1.2.3",
            "published",
            Arc::new(EmptyHistoryGit),
        )
        .await
        .unwrap();

        assert_eq!(published.slug, "published");
        let package = hub_root.join("skills/published");
        let mut entries = std::fs::read_dir(&package)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, vec!["SKILL.md", "_meta.json"]);
        let manifest: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(package.join("_meta.json")).await.unwrap()).unwrap();
        assert_eq!(manifest["id"], "published");
        for forbidden in ["enabled", "autoInject", "source", "namespace", "path", "git"] {
            assert!(manifest.get(forbidden).is_none(), "unexpected public field {forbidden}");
        }
    }
}
