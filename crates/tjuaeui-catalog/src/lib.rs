#![warn(clippy::disallowed_types)]

//! 技能与助手共用的静态版本目录能力。
//!
//! 本 crate 只处理来源无关的文件身份、GitHub 修订读取、索引缓存和文本比较；
//! 技能/助手的清单语义、用户偏好和激活事务仍归各自领域所有。

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, SystemTime};

use reqwest::Url;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
pub const MAX_CATALOG_FILE_SIZE: u64 = 5 * 1024 * 1024;

static MEMORY_CACHE: LazyLock<RwLock<HashMap<String, CachedJson>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, Clone)]
struct CachedJson {
    fetched_at: SystemTime,
    bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("目录请求无效：{0}")]
    InvalidRequest(String),
    #[error("目录资源不存在：{0}")]
    NotFound(String),
    #[error("目录访问失败：{0}")]
    Transport(String),
    #[error("目录内容无效：{0}")]
    InvalidContent(String),
    #[error("目录文件操作失败：{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogVersion {
    pub version: String,
    pub revision: String,
    pub digest: String,
    pub readme: String,
    pub files: Vec<CatalogFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogProvider {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileDiffStatus {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub status: FileDiffStatus,
    pub binary: bool,
    pub base_content: Option<String>,
    pub target_content: Option<String>,
}

pub fn validate_relative_file(path: &str, size: u64) -> Result<(), CatalogError> {
    let value = Path::new(path);
    if path.is_empty()
        || path.contains('\\')
        || value.is_absolute()
        || value
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CatalogError::InvalidRequest(format!("目录文件路径无效：{path}")));
    }
    if size > MAX_CATALOG_FILE_SIZE {
        return Err(CatalogError::InvalidRequest(format!("目录文件过大：{path}")));
    }
    Ok(())
}

pub async fn read_local_file(root: &Path, path: &str) -> Result<Vec<u8>, CatalogError> {
    validate_relative_file(path, 0)?;
    let root = tokio::fs::canonicalize(root).await?;
    let target = tokio::fs::canonicalize(root.join(path)).await?;
    if !target.starts_with(&root) {
        return Err(CatalogError::InvalidRequest(format!("目录文件路径越界：{path}")));
    }
    let metadata = tokio::fs::metadata(&target).await?;
    if !metadata.is_file() || metadata.len() > MAX_CATALOG_FILE_SIZE {
        return Err(CatalogError::InvalidRequest(format!("目录文件不可读取：{path}")));
    }
    Ok(tokio::fs::read(target).await?)
}

pub async fn load_json<T: DeserializeOwned>(
    configured_url: Option<&str>,
    local_path: Option<&Path>,
    default_url: &str,
    cache_namespace: &str,
) -> Result<T, CatalogError> {
    if let Some(path) = local_path.filter(|path| path.is_file()) {
        let bytes = tokio::fs::read(path).await?;
        return serde_json::from_slice(&bytes).map_err(|error| CatalogError::InvalidContent(error.to_string()));
    }
    let url = configured_url
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(default_url);
    let bytes = fetch_cached(url, cache_namespace).await?;
    serde_json::from_slice(&bytes).map_err(|error| CatalogError::InvalidContent(error.to_string()))
}

pub async fn fetch_github_revision_file(
    repository: &str,
    revision: &str,
    package_path: &str,
    file: &CatalogFile,
) -> Result<Vec<u8>, CatalogError> {
    validate_relative_file(&file.path, file.size)?;
    validate_relative_file(package_path, 0)?;
    if revision.trim().is_empty() {
        return Err(CatalogError::InvalidRequest("目录版本缺少 Git 修订".to_owned()));
    }
    let repository =
        Url::parse(repository).map_err(|error| CatalogError::InvalidRequest(format!("目录仓库地址无效：{error}")))?;
    if repository.host_str() != Some("github.com") {
        return Err(CatalogError::InvalidRequest("目录仓库必须位于 GitHub".to_owned()));
    }
    let segments = repository
        .path_segments()
        .map(|segments| segments.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 2 {
        return Err(CatalogError::InvalidRequest("目录仓库地址格式无效".to_owned()));
    }
    let mut url = Url::parse("https://raw.githubusercontent.com/")
        .map_err(|error| CatalogError::InvalidRequest(error.to_string()))?;
    {
        let mut target = url
            .path_segments_mut()
            .map_err(|_| CatalogError::InvalidRequest("无法构造目录文件地址".to_owned()))?;
        target.push(segments[0]);
        target.push(segments[1].trim_end_matches(".git"));
        target.push(revision);
        for component in Path::new(package_path)
            .components()
            .chain(Path::new(&file.path).components())
        {
            if let Component::Normal(value) = component {
                target.push(&value.to_string_lossy());
            }
        }
    }
    let client = tjuaeui_runtime::build_http_client(CONNECT_TIMEOUT, REQUEST_TIMEOUT)
        .map_err(|error| CatalogError::Transport(error.to_string()))?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| CatalogError::Transport(error.to_string()))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(CatalogError::NotFound(file.path.clone()));
    }
    let bytes = response
        .error_for_status()
        .map_err(|error| CatalogError::Transport(format!("{}：{error}", url.path())))?
        .bytes()
        .await
        .map_err(|error| CatalogError::Transport(error.to_string()))?;
    if bytes.len() as u64 > MAX_CATALOG_FILE_SIZE {
        return Err(CatalogError::InvalidRequest(format!("目录文件过大：{}", file.path)));
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !file.sha256.is_empty() && actual != file.sha256 {
        return Err(CatalogError::InvalidContent(format!(
            "目录文件摘要不匹配：{}",
            file.path
        )));
    }
    Ok(bytes.to_vec())
}

pub fn compare_text_files(
    base: &BTreeMap<String, Option<String>>,
    target: &BTreeMap<String, Option<String>>,
) -> Vec<FileDiff> {
    base.keys()
        .chain(target.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|path| {
            let base_content = base.get(&path).cloned().flatten();
            let target_content = target.get(&path).cloned().flatten();
            if base_content == target_content {
                return None;
            }
            let status = match (&base_content, &target_content) {
                (None, Some(_)) => FileDiffStatus::Added,
                (Some(_), None) => FileDiffStatus::Deleted,
                _ => FileDiffStatus::Modified,
            };
            let binary = base.get(&path).is_some_and(Option::is_none) || target.get(&path).is_some_and(Option::is_none);
            Some(FileDiff {
                path,
                status,
                binary,
                base_content,
                target_content,
            })
        })
        .collect()
}

async fn fetch_cached(url: &str, namespace: &str) -> Result<Vec<u8>, CatalogError> {
    if let Some(cached) = MEMORY_CACHE
        .read()
        .expect("目录内存缓存锁已损坏")
        .get(url)
        .filter(|cached| cached.fetched_at.elapsed().is_ok_and(|age| age < CACHE_TTL))
    {
        return Ok(cached.bytes.clone());
    }
    let cache_path = cache_path(namespace, url);
    if let Ok(metadata) = tokio::fs::metadata(&cache_path).await
        && metadata
            .modified()
            .ok()
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age < CACHE_TTL)
    {
        let bytes = tokio::fs::read(&cache_path).await?;
        remember(url, bytes.clone());
        return Ok(bytes);
    }
    let client = tjuaeui_runtime::build_http_client(CONNECT_TIMEOUT, REQUEST_TIMEOUT)
        .map_err(|error| CatalogError::Transport(error.to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| CatalogError::Transport(error.to_string()))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| CatalogError::Transport(error.to_string()))?
        .to_vec();
    if let Some(parent) = cache_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&cache_path, &bytes).await?;
    remember(url, bytes.clone());
    Ok(bytes)
}

fn remember(url: &str, bytes: Vec<u8>) {
    MEMORY_CACHE.write().expect("目录内存缓存锁已损坏").insert(
        url.to_owned(),
        CachedJson {
            fetched_at: SystemTime::now(),
            bytes,
        },
    );
}

fn cache_path(namespace: &str, url: &str) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(url.as_bytes()));
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("TjuaeUI")
        .join("catalogs")
        .join(namespace)
        .join(format!("{digest}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escaping_paths() {
        for path in ["", "../secret", "/absolute", "dir\\file"] {
            assert!(validate_relative_file(path, 0).is_err(), "{path}");
        }
        assert!(validate_relative_file("rules/zh-CN.md", 12).is_ok());
    }

    #[test]
    fn compares_added_modified_deleted_and_binary_files() {
        let base = BTreeMap::from([
            ("a.md".to_owned(), Some("old".to_owned())),
            ("gone.md".to_owned(), Some("gone".to_owned())),
            ("image.png".to_owned(), None),
        ]);
        let target = BTreeMap::from([
            ("a.md".to_owned(), Some("new".to_owned())),
            ("new.md".to_owned(), Some("new".to_owned())),
            ("image.png".to_owned(), None),
        ]);
        let diffs = compare_text_files(&base, &target);
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[0].status, FileDiffStatus::Modified);
        assert_eq!(diffs[1].status, FileDiffStatus::Deleted);
        assert_eq!(diffs[2].status, FileDiffStatus::Added);
    }
}
