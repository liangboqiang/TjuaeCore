//! Pure skill packages and the TjuaeHub static index.
//!
//! A package contains only `SKILL.md`, `_meta.json`, and optional public files.
//! Provider identity, enablement, automatic assistant injection, selected
//! version, Git state, and cache state deliberately live outside the package.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tjuaeui_common::WorkspaceGitProvisioner;
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

use crate::error::ExtensionError;

pub const SKILL_PACKAGE_MANIFEST: &str = "_meta.json";
pub const SKILL_ENTRY_FILE: &str = "SKILL.md";
const SKILL_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/tjuae-skill.v1.schema.json";
const TJUAE_HUB_INDEX_ENV: &str = "TJUAE_HUB_SKILL_INDEX_URL";
const DEFAULT_MARKET_INDEX_URL: &str = "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/dist/skills.json";
const MARKET_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const MARKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const MARKET_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

static MARKET_CACHE: LazyLock<RwLock<HashMap<String, CachedMarketIndex>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static MARKET_REFRESHES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

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
    pub format: String,
    pub format_version: u32,
    pub id: String,
    pub version: String,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub compatibility: BTreeMap<String, serde_json::Value>,
    pub requirements: Vec<String>,
    pub content_hash: String,
    pub extensions: BTreeMap<String, serde_json::Value>,
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
    pub categories: Vec<String>,
    pub tags: Vec<String>,
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
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    pub latest_version: String,
    pub versions: Vec<MarketSkillVersion>,
}

impl MarketSkillEntry {
    pub fn version(&self, version: &str) -> Option<&MarketSkillVersion> {
        self.versions.iter().find(|candidate| candidate.version == version)
    }

    pub fn latest(&self) -> Option<&MarketSkillVersion> {
        self.version(&self.latest_version)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketSkillVersion {
    pub version: String,
    pub revision: String,
    pub digest: String,
    pub readme: String,
    pub files: Vec<MarketSkillFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketSkillFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
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
    normalize_skill_workspace(&target, slug, git, "chore(skill): 初始化技能工作区").await
}

pub async fn tjuae_hub_index() -> Result<MarketIndex, ExtensionError> {
    let configured_url = std::env::var(TJUAE_HUB_INDEX_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let index = if let Some(url) = configured_url {
        fetch_market_index(url.trim()).await?
    } else if let Some(path) = crate::skill_storage::resolve_tjuae_hub_worktree()
        .map(|root| root.join("dist").join("skills.json"))
        .filter(|path| path.is_file())
    {
        let index: MarketIndex = serde_json::from_slice(&tokio::fs::read(path).await?)?;
        validate_market_index(&index)?;
        index
    } else {
        fetch_market_index(DEFAULT_MARKET_INDEX_URL).await?
    };
    if index.market.id != "tjuae-hub" {
        return Err(ExtensionError::ManifestValidation(
            "TjuaeHub 技能索引标识必须是 tjuae-hub".to_owned(),
        ));
    }
    Ok(index)
}

async fn fetch_market_index(url: &str) -> Result<MarketIndex, ExtensionError> {
    if let Some(cached) = MARKET_CACHE.read().await.get(url)
        && cached.fetched_at.elapsed().is_ok_and(|age| age < MARKET_CACHE_TTL)
    {
        return Ok(cached.index.clone());
    }

    let persisted = load_persisted_market_index(url).await;
    if let Some((fetched_at, index)) = persisted {
        cache_market_index(url, fetched_at, index.clone()).await;
        if fetched_at.elapsed().is_ok_and(|age| age >= MARKET_CACHE_TTL) {
            refresh_market_index_in_background(url.to_owned()).await;
        }
        return Ok(index);
    }

    let index = fetch_remote_market_index(url).await?;
    persist_market_index(url, &index).await;
    cache_market_index(url, SystemTime::now(), index.clone()).await;
    Ok(index)
}

async fn refresh_market_index_in_background(url: String) {
    if !MARKET_REFRESHES.lock().await.insert(url.clone()) {
        return;
    }
    tokio::spawn(async move {
        match fetch_remote_market_index(&url).await {
            Ok(index) => {
                persist_market_index(&url, &index).await;
                cache_market_index(&url, SystemTime::now(), index).await;
            }
            Err(error) => warn!(%url, %error, "技能市场后台刷新失败，继续使用最近成功的本地快照"),
        }
        MARKET_REFRESHES.lock().await.remove(&url);
    });
}

async fn fetch_remote_market_index(url: &str) -> Result<MarketIndex, ExtensionError> {
    let client = tjuaeui_runtime::build_http_client(MARKET_CONNECT_TIMEOUT, MARKET_REQUEST_TIMEOUT)
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
    Ok(index)
}

async fn cache_market_index(url: &str, fetched_at: SystemTime, index: MarketIndex) {
    MARKET_CACHE
        .write()
        .await
        .insert(url.to_owned(), CachedMarketIndex { fetched_at, index });
}

fn market_cache_path(url: &str) -> Option<PathBuf> {
    let mut digest = Sha256::new();
    digest.update(url.as_bytes());
    let file_name = format!("{:x}.json", digest.finalize());
    dirs::cache_dir().map(|root| root.join("TjuaeUI").join("skill-markets").join(file_name))
}

async fn load_persisted_market_index(url: &str) -> Option<(SystemTime, MarketIndex)> {
    let path = market_cache_path(url)?;
    read_market_index_cache(&path).await
}

async fn persist_market_index(url: &str, index: &MarketIndex) {
    let Some(path) = market_cache_path(url) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    let result = async {
        tokio::fs::create_dir_all(parent).await?;
        write_market_index_cache(&path, index).await
    }
    .await;
    if let Err(error) = result {
        warn!(path = %path.display(), %error, "保存技能市场本地快照失败");
    }
}

async fn read_market_index_cache(path: &Path) -> Option<(SystemTime, MarketIndex)> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let fetched_at = metadata.modified().ok()?;
    let bytes = tokio::fs::read(path).await.ok()?;
    let index = serde_json::from_slice::<MarketIndex>(&bytes).ok()?;
    if let Err(error) = validate_market_index(&index) {
        warn!(path = %path.display(), %error, "忽略无效的技能市场本地快照");
        return None;
    }
    Some((fetched_at, index))
}

async fn write_market_index_cache(path: &Path, index: &MarketIndex) -> Result<(), std::io::Error> {
    let bytes = serde_json::to_vec(index).map_err(std::io::Error::other)?;
    tokio::fs::write(path, bytes).await
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
        // A directory without the one public manifest is not a skill in the
        // current protocol. Ignore it before validation so unrelated folders
        // and unsupported legacy packages do not emit a warning on every
        // catalog refresh; no legacy manifest is read or migrated here.
        if !entry.path().join(SKILL_PACKAGE_MANIFEST).is_file() {
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
    let slug = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ExtensionError::InvalidSkillPath(directory.display().to_string()))?;
    load_skill_from_directory(&directory, slug).await
}

pub(crate) async fn load_skill_from_directory(
    directory: &Path,
    expected_slug: &str,
) -> Result<InstalledSkill, ExtensionError> {
    let directory = canonical_directory(directory)?;
    let manifest = read_manifest(&directory).await?;
    validate_manifest_identity(&manifest, expected_slug)?;
    verify_workspace_digest(&directory, &manifest.content_hash)?;
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
        categories: manifest.categories,
        tags: manifest.tags,
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

pub async fn delete_installed_skill(root: &Path, slug: &str) -> Result<(), ExtensionError> {
    validate_slug(slug)?;
    let target = root.join(slug);
    if !target.is_dir() {
        return Err(ExtensionError::SkillNotFound(slug.to_owned()));
    }
    tokio::fs::remove_dir_all(target).await?;
    Ok(())
}

pub async fn export_skill_archive(root: &Path, slug: &str, output_path: &Path) -> Result<(), ExtensionError> {
    let skill = load_installed_skill(&root.join(slug)).await?;
    export_skill_directory_archive(&skill.path, output_path).await
}

pub async fn export_skill_directory_archive(directory: &Path, output_path: &Path) -> Result<(), ExtensionError> {
    let skill = load_installed_skill(directory).await?;
    let source = skill.path;
    let output = output_path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), ExtensionError> {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(&output)?;
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let files = archive_files(&source)?;
        for relative in files {
            let name = relative.to_string_lossy().replace('\\', "/");
            archive.start_file(name, options)?;
            std::io::copy(&mut std::fs::File::open(source.join(relative))?, &mut archive)?;
        }
        archive.finish()?;
        Ok(())
    })
    .await
    .map_err(|error| ExtensionError::Internal(error.to_string()))??;
    Ok(())
}

pub async fn import_skill_archive(
    root: &Path,
    archive_path: &Path,
    git: Arc<dyn WorkspaceGitProvisioner>,
) -> Result<InstalledSkill, ExtensionError> {
    let root = root.to_path_buf();
    let archive_path = archive_path.to_path_buf();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ExtensionError::Internal(error.to_string()))?
        .as_nanos();
    let staging = root.join(format!(".importing-{nonce}"));
    tokio::fs::create_dir_all(&staging).await?;
    let result = async {
        let extract_target = staging.clone();
        tokio::task::spawn_blocking(move || extract_skill_archive(&archive_path, &extract_target))
            .await
            .map_err(|error| ExtensionError::Internal(error.to_string()))??;
        let package_root = detect_package_root(&staging)?;
        let manifest = read_manifest(&package_root).await?;
        if package_root == staging {
            // Tjuae exports a rootless archive. Its extraction directory is an
            // implementation-only nonce, so validate the public identity
            // against the manifest and the digest against extracted files. A
            // wrapped third-party archive still has to match its directory.
            validate_manifest_identity(&manifest, &manifest.id)?;
            verify_workspace_digest(&package_root, &manifest.content_hash)?;
        } else {
            validate_manifest(&manifest, &package_root)?;
        }
        let target = root.join(&manifest.id);
        if target.exists() {
            return Err(ExtensionError::InvalidRequest(format!("技能 {} 已存在", manifest.id)));
        }
        rename_installed_directory(&package_root, &target).await?;
        match normalize_skill_workspace(&target, &manifest.id, git, "chore(skill): 导入纯技能包").await {
            Ok(skill) => Ok(skill),
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&target).await;
                Err(error)
            }
        }
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result
}

fn archive_files(root: &Path) -> Result<Vec<PathBuf>, ExtensionError> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), ExtensionError> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(
                name.as_ref(),
                ".git" | "node_modules" | ".DS_Store" | "__MACOSX" | "Thumbs.db"
            ) || name.starts_with("._")
            {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(ExtensionError::SkillImportInvalidSource(format!(
                    "技能包不能包含符号链接：{}",
                    entry.path().display()
                )));
            }
            if kind.is_dir() {
                visit(root, &entry.path(), files)?;
            } else {
                files.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
            }
            if files.len() > 2_000 {
                return Err(ExtensionError::InvalidRequest("技能包文件数量超过 2000".into()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn extract_skill_archive(archive_path: &Path, target: &Path) -> Result<(), ExtensionError> {
    const MAX_PACKAGE_BYTES: u64 = 20 * 1024 * 1024;
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    if archive.len() > 2_000 {
        return Err(ExtensionError::InvalidRequest("技能包文件数量超过 2000".into()));
    }
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let enclosed = file
            .enclosed_name()
            .ok_or_else(|| ExtensionError::SkillImportInvalidSource(format!("技能包路径越界：{}", file.name())))?;
        if file.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(ExtensionError::SkillImportInvalidSource(format!(
                "技能包不能包含符号链接：{}",
                file.name()
            )));
        }
        total = total.saturating_add(file.size());
        if total > MAX_PACKAGE_BYTES {
            return Err(ExtensionError::InvalidRequest("技能包解压后超过 20 MB".into()));
        }
        let destination = target.join(enclosed);
        if file.is_dir() {
            std::fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut file, &mut std::fs::File::create(destination)?)?;
    }
    Ok(())
}

fn detect_package_root(staging: &Path) -> Result<PathBuf, ExtensionError> {
    if staging.join(SKILL_ENTRY_FILE).is_file() && staging.join(SKILL_PACKAGE_MANIFEST).is_file() {
        return Ok(staging.to_path_buf());
    }
    let directories = std::fs::read_dir(staging)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    if directories.len() == 1 {
        let root = directories[0].path();
        if root.join(SKILL_ENTRY_FILE).is_file() && root.join(SKILL_PACKAGE_MANIFEST).is_file() {
            return Ok(root);
        }
    }
    Err(ExtensionError::SkillImportNoSkillFound(staging.display().to_string()))
}

fn local_manifest(slug: &str, version: &str) -> SkillManifest {
    SkillManifest {
        schema: SKILL_SCHEMA_URL.to_owned(),
        format: "agent-skill".to_owned(),
        format_version: 1,
        id: slug.to_owned(),
        version: version.to_owned(),
        categories: Vec::new(),
        tags: Vec::new(),
        compatibility: BTreeMap::new(),
        requirements: Vec::new(),
        content_hash: String::new(),
        extensions: BTreeMap::new(),
    }
}

/// Validate and seal a pure public skill package without adding any runtime
/// preference, provider provenance, Git metadata, or application state.
pub(crate) async fn seal_skill_package(
    directory: &Path,
    slug: &str,
    version: &str,
    categories: Vec<String>,
    tags: Vec<String>,
) -> Result<InstalledSkill, ExtensionError> {
    validate_slug(slug)?;
    if !directory.join(SKILL_ENTRY_FILE).is_file() {
        return Err(ExtensionError::SkillImportNoSkillFound(directory.display().to_string()));
    }
    let version = Version::parse(version)
        .map_err(|error| ExtensionError::InvalidVersion {
            version: version.to_owned(),
            reason: error.to_string(),
        })?
        .to_string();
    let mut manifest = local_manifest(slug, &version);
    manifest.categories = unique_values(categories);
    manifest.tags = unique_values(tags);
    manifest.content_hash = workspace_digest(directory)?;
    write_manifest(&directory.join(SKILL_PACKAGE_MANIFEST), &manifest).await?;
    load_skill_from_directory(directory, slug).await
}

pub(crate) async fn reseal_skill_package(directory: &Path) -> Result<InstalledSkill, ExtensionError> {
    let mut manifest = read_manifest(directory).await?;
    validate_manifest_identity(
        &manifest,
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ExtensionError::InvalidSkillPath(directory.display().to_string()))?,
    )?;
    manifest.content_hash = workspace_digest(directory)?;
    write_manifest(&directory.join(SKILL_PACKAGE_MANIFEST), &manifest).await?;
    load_installed_skill(directory).await
}

pub(crate) async fn save_skill_manifest_content(
    directory: &Path,
    content: &str,
) -> Result<InstalledSkill, ExtensionError> {
    let mut manifest: SkillManifest = serde_json::from_str(content)?;
    let slug = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ExtensionError::InvalidSkillPath(directory.display().to_string()))?;
    validate_manifest_identity(&manifest, slug)?;
    manifest.schema = SKILL_SCHEMA_URL.to_owned();
    manifest.format = "agent-skill".to_owned();
    manifest.format_version = 1;
    manifest.categories = unique_values(manifest.categories);
    manifest.tags = unique_values(manifest.tags);
    manifest.content_hash = workspace_digest(directory)?;
    write_manifest(&directory.join(SKILL_PACKAGE_MANIFEST), &manifest).await?;
    load_installed_skill(directory).await
}

fn unique_values(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

/// The only normalization/write boundary shared by manual creation, Butler,
/// folder import, Git clone, copy, and market materialization.
pub(crate) async fn normalize_skill_workspace(
    directory: &Path,
    slug: &str,
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
    manifest.format = "agent-skill".to_owned();
    manifest.format_version = 1;
    manifest.id = slug.to_owned();
    if Version::parse(&manifest.version).is_err() {
        manifest.version = "0.1.0".to_owned();
    }
    let mut seen = HashSet::new();
    manifest
        .categories
        .retain(|category| !category.trim().is_empty() && seen.insert(category.clone()));
    manifest.content_hash = workspace_digest(directory)?;
    write_manifest(&directory.join(SKILL_PACKAGE_MANIFEST), &manifest).await?;
    git.ensure_workspace_git(directory)
        .await
        .map_err(ExtensionError::Internal)?;
    git.commit_workspace_snapshot(directory, commit_message)
        .await
        .map_err(ExtensionError::Internal)?;
    load_skill_from_directory(directory, slug).await
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
    )?;
    verify_workspace_digest(directory, &manifest.content_hash)?;
    Ok(())
}

fn validate_manifest_identity(manifest: &SkillManifest, slug: &str) -> Result<(), ExtensionError> {
    if manifest.schema != SKILL_SCHEMA_URL || manifest.format != "agent-skill" || manifest.format_version != 1 {
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
    let mut tags = HashSet::new();
    if manifest.tags.iter().any(|tag| {
        let tag = tag.trim();
        tag.is_empty() || !tags.insert(tag)
    }) {
        return Err(ExtensionError::ManifestValidation("技能标签不能为空或重复".to_owned()));
    }
    let mut requirements = HashSet::new();
    if manifest.requirements.iter().any(|requirement| {
        let requirement = requirement.trim();
        requirement.is_empty() || !requirements.insert(requirement)
    }) {
        return Err(ExtensionError::ManifestValidation("技能依赖不能为空或重复".to_owned()));
    }
    Ok(())
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
        if !ids.insert(&entry.id) || entry.versions.is_empty() || entry.latest().is_none() {
            return Err(ExtensionError::ManifestValidation(format!(
                "技能市场条目 {} 重复、缺少版本或 latestVersion 无效",
                entry.id
            )));
        }
        let mut versions = HashSet::new();
        for version in &entry.versions {
            Version::parse(&version.version).map_err(|error| ExtensionError::InvalidVersion {
                version: version.version.clone(),
                reason: error.to_string(),
            })?;
            if !versions.insert(&version.version)
                || !is_revision(&version.revision)
                || !is_sha256(&version.digest)
                || version.readme.trim().is_empty()
                || version.files.is_empty()
            {
                return Err(ExtensionError::ManifestValidation(format!(
                    "技能市场条目 {} 的版本索引无效",
                    entry.id
                )));
            }
            for file in &version.files {
                validate_relative_file_path(&file.path)?;
                if file.sha256.len() != 64 || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(ExtensionError::ManifestValidation(format!(
                        "技能市场条目 {} 的文件摘要无效",
                        entry.id
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_relative_file_path(path: &str) -> Result<(), ExtensionError> {
    let value = Path::new(path);
    if path.is_empty()
        || path.contains('\\')
        || value.is_absolute()
        || value
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ExtensionError::ManifestValidation(format!("技能文件路径无效：{path}")));
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
            if matches!(
                name.as_ref(),
                ".git" | "node_modules" | ".DS_Store" | "__MACOSX" | "Thumbs.db" | "_meta.json"
            ) || name.starts_with("._")
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::BTreeSet;
    use std::io::Write;
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
    async fn create_and_list_use_one_pure_package_model() {
        let temp = TempDir::new().unwrap();
        let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);
        let created = create_skill(temp.path(), "cron", "cron", "test", git.clone())
            .await
            .unwrap();
        assert!(created.path.join(SKILL_PACKAGE_MANIFEST).is_file());
        assert_eq!(list_installed_skills(temp.path()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn legacy_or_unrelated_directories_are_not_catalog_skills() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join("legacy");
        tokio::fs::create_dir_all(&legacy).await.unwrap();
        tokio::fs::write(legacy.join(SKILL_ENTRY_FILE), "# Legacy\n")
            .await
            .unwrap();
        tokio::fs::write(legacy.join(".tjuae-skill.json"), "{}").await.unwrap();

        assert!(list_installed_skills(temp.path()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn normalization_uses_public_slug_in_an_internal_staging_directory() {
        let temp = TempDir::new().unwrap();
        let staging = temp.path().join(".runtime-portable-nonce");
        tokio::fs::create_dir_all(&staging).await.unwrap();
        tokio::fs::write(
            staging.join(SKILL_ENTRY_FILE),
            "---\nname: portable\ndescription: test\n---\n\n# portable\n",
        )
        .await
        .unwrap();
        let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);

        let skill = normalize_skill_workspace(&staging, "portable", git, "test")
            .await
            .unwrap();

        assert_eq!(skill.slug, "portable");
        assert_eq!(skill.path, std::fs::canonicalize(staging).unwrap());
    }

    #[tokio::test]
    async fn export_contains_only_public_skill_package_data() {
        let temp = TempDir::new().unwrap();
        let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);
        let created = create_skill(temp.path(), "portable", "portable", "test", git)
            .await
            .unwrap();
        std::fs::create_dir_all(created.path.join(".git")).unwrap();
        std::fs::write(created.path.join(".git/config"), "private git state").unwrap();

        let output = temp.path().join("portable.zip");
        export_skill_archive(temp.path(), "portable", &output).await.unwrap();

        let file = std::fs::File::open(output).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names = archive.file_names().map(str::to_owned).collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            BTreeSet::from([SKILL_PACKAGE_MANIFEST.to_owned(), SKILL_ENTRY_FILE.to_owned()])
        );

        let manifest: serde_json::Value =
            serde_json::from_reader(archive.by_name(SKILL_PACKAGE_MANIFEST).unwrap()).unwrap();
        assert!(manifest.get("enabled").is_none());
        assert!(manifest.get("autoInject").is_none());
        assert!(manifest.get("source").is_none());
    }

    #[tokio::test]
    async fn exported_rootless_package_imports_again() {
        let temp = TempDir::new().unwrap();
        let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);
        let created = create_skill(temp.path(), "portable", "portable", "test", git.clone())
            .await
            .unwrap();
        let output = temp.path().join("portable.zip");
        export_skill_archive(temp.path(), "portable", &output).await.unwrap();
        tokio::fs::remove_dir_all(created.path).await.unwrap();

        let imported = import_skill_archive(temp.path(), &output, git).await.unwrap();

        assert_eq!(imported.slug, "portable");
        assert!(imported.path.join(SKILL_PACKAGE_MANIFEST).is_file());
        assert!(imported.path.join(SKILL_ENTRY_FILE).is_file());
    }

    #[tokio::test]
    async fn rejected_import_removes_its_staging_directory() {
        let temp = TempDir::new().unwrap();
        let archive_path = temp.path().join("invalid.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file(SKILL_PACKAGE_MANIFEST, options).unwrap();
        archive.write_all(b"{}").unwrap();
        archive.start_file(SKILL_ENTRY_FILE, options).unwrap();
        archive.write_all(b"# invalid").unwrap();
        archive.finish().unwrap();

        let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);
        assert!(import_skill_archive(temp.path(), &archive_path, git).await.is_err());
        assert!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".importing-"))
        );
    }

    #[test]
    fn digest_matches_market_algorithm() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("a.txt"), b"one").unwrap();
        std::fs::create_dir(temp.path().join("b")).unwrap();
        std::fs::write(temp.path().join("b/c.txt"), b"two").unwrap();
        assert_eq!(workspace_digest(temp.path()).unwrap().len(), 71);
    }

    #[tokio::test]
    async fn persistent_market_cache_round_trips_a_validated_index() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("skills.json");
        let expected = MarketIndex {
            schema: "https://example.com/market.schema.json".to_owned(),
            schema_version: 1,
            market: MarketInfo {
                id: "test-market".to_owned(),
                name: "测试市场".to_owned(),
            },
            repository: "https://github.com/example/skills.git".to_owned(),
            revision: "a".repeat(40),
            skills: vec![MarketSkillEntry {
                id: "example".to_owned(),
                path: "skills/example".to_owned(),
                name: "示例技能".to_owned(),
                description: "用于验证本地快照。".to_owned(),
                categories: vec!["测试".to_owned()],
                tags: vec!["示例".to_owned()],
                latest_version: "1.0.0".to_owned(),
                versions: vec![MarketSkillVersion {
                    version: "1.0.0".to_owned(),
                    revision: "a".repeat(40),
                    digest: format!("sha256-{}", "b".repeat(64)),
                    readme: "# 示例技能".to_owned(),
                    files: vec![
                        MarketSkillFile {
                            path: SKILL_PACKAGE_MANIFEST.to_owned(),
                            size: 128,
                            sha256: "c".repeat(64),
                        },
                        MarketSkillFile {
                            path: SKILL_ENTRY_FILE.to_owned(),
                            size: 256,
                            sha256: "d".repeat(64),
                        },
                    ],
                }],
            }],
        };

        write_market_index_cache(&path, &expected).await.unwrap();
        let (_, actual) = read_market_index_cache(&path).await.unwrap();
        assert_eq!(actual, expected);
    }
}
