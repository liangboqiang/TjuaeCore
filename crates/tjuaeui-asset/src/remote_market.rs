use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{
    AssetAction, AssetDiffResponse, AssetEditability, AssetFileResponse, AssetKind, AssetOperationResponse,
    AssetOrigin, AssetResolveResponse, AssetScope, AssetSummaryResponse, AssetSyncState, AssetTrust,
    ListMarketAssetsQuery, MarketAssetDescriptor, MarketAssetFileResponse, MarketAssetResponse, MarketAssetStatus,
    MarketCacheResponse, MarketCompatibilityResponse, MarketIndexResponse, MarketPackageDescriptor,
    MarketPackageReviewStatus, MarketPresenceState, ResolveAssetRequest,
};
use tjuaeui_common::now_ms;
use tokio::sync::Mutex;

use crate::{
    AssetCatalogService, AssetDefinitionFile, AssetError, LocalAssetInput, TrackedAssetInput, digest_bytes,
    normalize_relative_path, prepare_definition,
};

pub const MARKET_INDEX_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/hub-index.v2.schema.json";
pub const OFFLINE_SEED_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/offline-seed-manifest.v1.schema.json";
pub const OFFLINE_RESOURCE_MANIFEST_SCHEMA: &str = "tjuae://schemas/hub-offline-resources.v1";
/// Hub 资产定义契约的版本，与 Core 二进制/Cargo 版本独立演进。
pub const TJUAE_ASSET_PROTOCOL_VERSION: &str = "1.0.0";

const HUB_REPOSITORY: &str = "https://github.com/liangboqiang/TjuaeHub";
const HUB_RAW_BASE: &str = "https://raw.githubusercontent.com/liangboqiang/TjuaeHub";
const HUB_DIST_REF_API: &str = "https://api.github.com/repos/liangboqiang/TjuaeHub/git/ref/heads/dist";
const INDEX_CACHE_FILE: &str = "market-index-v2-cache.json";
const OFFLINE_SEED_DIR_ENV: &str = "TJUAE_HUB_OFFLINE_DIR";
const OFFLINE_SEED_MANIFEST_ENV: &str = "TJUAE_HUB_OFFLINE_MANIFEST";
const OFFLINE_DIST_REF_ENV: &str = "TJUAE_HUB_DIST_REF";
const OFFLINE_DEVELOPMENT_ENV: &str = "TJUAE_HUB_OFFLINE_DEVELOPMENT";
const INDEX_MAX_BYTES: usize = 8 * 1024 * 1024;
const REF_MAX_BYTES: usize = 64 * 1024;
const PACKAGE_MAX_UNPACKED_BYTES: u64 = 50 * 1024 * 1024;
const PACKAGE_MAX_ARCHIVE_BYTES: usize = 50 * 1024 * 1024;
const PACKAGE_MAX_ENTRIES: usize = 4_096;
const FILE_MAX_BYTES: u64 = 50 * 1024 * 1024;
const OFFLINE_MANIFEST_MAX_BYTES: u64 = 256 * 1024;
const OFFLINE_SEED_BUNDLE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const OFFLINE_SEED_MAX_ENTRIES: usize = 4_096;
const CACHE_STALE_AFTER_MS: i64 = 24 * 60 * 60 * 1_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum MarketError {
    #[error("远程市场索引尚未缓存")]
    Unavailable,
    #[error("远程市场索引无效：{0}")]
    Invalid(String),
    #[error("远程市场网络请求失败：{0}")]
    Network(String),
    #[error("远程市场响应超过大小限制")]
    TooLarge { actual: u64, limit: u64 },
    #[error("远程市场提交哈希无效")]
    InvalidCommit,
    #[error(transparent)]
    Asset(#[from] crate::AssetError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawCompatibility {
    tjuae: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawMarketAsset {
    id: String,
    kind: AssetKind,
    runtime_id: String,
    dependencies: Vec<String>,
    display_name: String,
    description: String,
    version: String,
    definition_digest: String,
    entry_file: String,
    package_name: String,
    author: String,
    license: String,
    trust: AssetTrust,
    status: MarketAssetStatus,
    compatibility: RawCompatibility,
    source_revision: String,
    files: Vec<MarketAssetFileResponse>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawMarketMetadata {
    total_packages: u64,
    total_assets: u64,
    generated_by: String,
    repository: String,
    source_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawMarketIndex {
    #[serde(rename = "$schema")]
    schema: String,
    schema_version: u32,
    generated_at: String,
    assets: BTreeMap<String, RawMarketAsset>,
    packages: BTreeMap<String, MarketPackageDescriptor>,
    metadata: RawMarketMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredMarketCache {
    index: RawMarketIndex,
    /// Immutable commit of the generated `dist` branch used for downloads.
    /// This is intentionally distinct from `index.metadata.source_revision`.
    distribution_revision: Option<String>,
    cached_at: i64,
    source_url: String,
    #[serde(default)]
    origin: MarketCacheOrigin,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum MarketCacheOrigin {
    #[default]
    Remote,
    OfflineSeed,
}

#[derive(Debug, Clone)]
struct OfflineSeedSource {
    directory: PathBuf,
    manifest_path: PathBuf,
    dist_ref: Option<String>,
    development: bool,
}

#[derive(Debug, Clone)]
enum OfflineSeedSetting {
    None,
    Source(OfflineSeedSource),
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OfflineResourceFile {
    file_name: String,
    digest: String,
    size: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OfflineResourceSource {
    kind: String,
    repository: String,
    #[serde(default)]
    dist_ref: Option<String>,
    #[serde(default)]
    source_revision: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OfflineResourceManifest {
    #[serde(rename = "$schema")]
    schema: String,
    schema_version: u32,
    source: OfflineResourceSource,
    seed_manifest: OfflineResourceFile,
    bundle: OfflineResourceFile,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OfflineSeedManifest {
    #[serde(rename = "$schema")]
    schema: String,
    schema_version: u32,
    generated_at: String,
    source_revision: String,
    seed_index_digest: String,
    bundle: OfflineResourceFile,
    asset_kinds: Vec<AssetKind>,
    package_names: Vec<String>,
    asset_ids: Vec<String>,
}

#[derive(Debug)]
struct LoadedOfflineSeed {
    stored: StoredMarketCache,
    packages: BTreeMap<String, Vec<u8>>,
}

type OfflineSeedArchiveContents = (Vec<u8>, BTreeMap<String, Vec<u8>>);

#[async_trait]
pub(crate) trait MarketIndexRemote: Send + Sync {
    async fn resolve_dist_head(&self) -> Result<String, MarketError>;
    async fn fetch_index(&self, source_url: &str) -> Result<Vec<u8>, MarketError>;
    async fn fetch_package(&self, source_url: &str) -> Result<Vec<u8>, MarketError>;
}

struct GitHubMarketIndexRemote;

#[async_trait]
impl MarketIndexRemote for GitHubMarketIndexRemote {
    async fn resolve_dist_head(&self) -> Result<String, MarketError> {
        let client = market_http_client()?;
        let expected = validate_exact_url(HUB_DIST_REF_API, HUB_DIST_REF_API, "解析 Hub dist 引用")?;
        let response = client
            .get(expected.clone())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, "TjuaeCore-Market")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| MarketError::Network(error.to_string()))?;
        let bytes = read_bounded_response(response, REF_MAX_BYTES, expected.as_str(), "解析 Hub dist 引用").await?;
        parse_dist_ref(&bytes)
    }

    async fn fetch_index(&self, source_url: &str) -> Result<Vec<u8>, MarketError> {
        let client = market_http_client()?;
        let expected = validate_index_url(source_url)?;
        let response = client
            .get(expected.clone())
            .header(reqwest::header::USER_AGENT, "TjuaeCore-Market")
            .send()
            .await
            .map_err(|error| MarketError::Network(error.to_string()))?;
        read_bounded_response(response, INDEX_MAX_BYTES, expected.as_str(), "读取 Hub 资产索引").await
    }

    async fn fetch_package(&self, source_url: &str) -> Result<Vec<u8>, MarketError> {
        let client = market_http_client()?;
        let expected = validate_package_url(source_url)?;
        let response = client
            .get(expected.clone())
            .header(reqwest::header::USER_AGENT, "TjuaeCore-Market")
            .send()
            .await
            .map_err(|error| MarketError::Network(error.to_string()))?;
        read_bounded_response(
            response,
            PACKAGE_MAX_ARCHIVE_BYTES,
            expected.as_str(),
            "读取 Hub 原子资产包",
        )
        .await
    }
}

/// Core 维护的远程仓库索引。Hub 只提供不可变内容，兼容性与本地关系由 Core 计算。
#[derive(Clone)]
pub struct MarketIndexManager {
    cache_dir: PathBuf,
    asset_protocol_version: String,
    offline_seed: OfflineSeedSetting,
    refresh_lock: Arc<Mutex<()>>,
    package_lock: Arc<Mutex<()>>,
    confirmed_distribution_revision: Arc<RwLock<Option<String>>>,
    remote: Arc<dyn MarketIndexRemote>,
}

impl MarketIndexManager {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::with_remote_and_seed(
            data_dir.into().join("market-cache"),
            TJUAE_ASSET_PROTOCOL_VERSION,
            Arc::new(GitHubMarketIndexRemote),
            offline_seed_from_env(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_remote(
        cache_dir: PathBuf,
        asset_protocol_version: impl Into<String>,
        remote: Arc<dyn MarketIndexRemote>,
    ) -> Self {
        Self::with_remote_and_seed(cache_dir, asset_protocol_version, remote, OfflineSeedSetting::None)
    }

    fn with_remote_and_seed(
        cache_dir: PathBuf,
        asset_protocol_version: impl Into<String>,
        remote: Arc<dyn MarketIndexRemote>,
        offline_seed: OfflineSeedSetting,
    ) -> Self {
        Self {
            cache_dir,
            asset_protocol_version: asset_protocol_version.into(),
            offline_seed,
            refresh_lock: Arc::new(Mutex::new(())),
            package_lock: Arc::new(Mutex::new(())),
            confirmed_distribution_revision: Arc::new(RwLock::new(None)),
            remote,
        }
    }

    pub async fn load_index(
        &self,
        user_id: &str,
        catalog: &AssetCatalogService,
        query: &ListMarketAssetsQuery,
    ) -> Result<MarketIndexResponse, MarketError> {
        if self.uses_development_seed() {
            let stored = self.import_offline_seed()?;
            return self.response_from_index(stored, user_id, catalog, query).await;
        }
        let stored = match self.load_cached_index() {
            Ok(stored) => stored,
            Err(MarketError::Unavailable | MarketError::Invalid(_)) => match self.refresh(None).await {
                Ok(_) => self.load_cached_index()?,
                Err(MarketError::Network(_) | MarketError::Unavailable) => self.import_offline_seed()?,
                Err(error) => return Err(error),
            },
            Err(error) => return Err(error),
        };
        self.response_from_index(stored, user_id, catalog, query).await
    }

    /// 列出 Core 本地仓库，并仅使用本进程已向 Hub 确认过的当前索引计算
    /// 远端关系。单纯命中磁盘缓存、离线种子或网络失败时不猜测远端状态。
    pub async fn list_local_assets(
        &self,
        user_id: &str,
        catalog: &AssetCatalogService,
        kind: Option<AssetKind>,
        scope: Option<AssetScope>,
    ) -> Result<Vec<AssetSummaryResponse>, MarketError> {
        let stored = match self.load_cached_index() {
            Ok(stored) => stored,
            Err(MarketError::Unavailable | MarketError::Invalid(_)) => {
                return catalog
                    .list_with_remote_index(user_id, kind, scope, &BTreeMap::new(), false)
                    .await
                    .map_err(Into::into);
            }
            Err(error) => return Err(error),
        };
        let relation_inputs = self.relation_inputs(&stored.index)?;
        let mut assets = catalog
            .list_with_remote_index(
                user_id,
                kind,
                scope,
                &relation_inputs,
                self.index_is_remote_confirmed(&stored),
            )
            .await
            .map_err(MarketError::from)?;
        apply_remote_lifecycle_to_local_assets(&mut assets, &stored.index.assets);
        Ok(assets)
    }

    pub async fn refresh(
        &self,
        requested_distribution_revision: Option<&str>,
    ) -> Result<MarketCacheResponse, MarketError> {
        let _guard = self.refresh_lock.lock().await;
        if self.uses_development_seed() {
            return self.import_offline_seed().map(|stored| cache_response(&stored));
        }
        let verifies_current_head = requested_distribution_revision.is_none();
        let resolved;
        let distribution_revision = if let Some(value) = requested_distribution_revision {
            validate_commit_sha(value)?;
            value
        } else {
            resolved = match self.remote.resolve_dist_head().await {
                Ok(value) => value,
                Err(error) => {
                    self.clear_remote_confirmation();
                    return Err(error);
                }
            };
            resolved.as_str()
        };
        let source_url = index_url(distribution_revision);

        if let Ok(cached) = self.load_cached_index()
            && cached.distribution_revision.as_deref() == Some(distribution_revision)
        {
            if verifies_current_head {
                self.confirm_distribution_revision(distribution_revision);
            }
            return Ok(cache_response(&cached));
        }

        let bytes = match self.remote.fetch_index(&source_url).await {
            Ok(bytes) => bytes,
            Err(error) => {
                self.clear_remote_confirmation();
                return Err(error);
            }
        };
        let index: RawMarketIndex = match serde_json::from_slice(&bytes) {
            Ok(index) => index,
            Err(error) => {
                self.clear_remote_confirmation();
                return Err(MarketError::Invalid(error.to_string()));
            }
        };
        if let Err(error) = validate_index(&index) {
            self.clear_remote_confirmation();
            return Err(error);
        }
        let stored = StoredMarketCache {
            index,
            distribution_revision: Some(distribution_revision.to_owned()),
            cached_at: now_ms(),
            source_url,
            origin: MarketCacheOrigin::Remote,
        };
        if let Err(error) = self.persist_cache(&stored) {
            self.clear_remote_confirmation();
            return Err(error);
        }
        if verifies_current_head {
            self.confirm_distribution_revision(distribution_revision);
        } else {
            self.clear_remote_confirmation();
        }
        tracing::info!(
            distribution_revision,
            assets = stored.index.assets.len(),
            packages = stored.index.packages.len(),
            "market: refreshed commit-pinned Hub v2 index"
        );
        Ok(cache_response(&stored))
    }

    pub async fn install_asset(
        &self,
        user_id: &str,
        catalog: &AssetCatalogService,
        remote_asset_id: &str,
        idempotency_key: &str,
    ) -> Result<AssetOperationResponse, MarketError> {
        let stored = self.load_or_refresh_cache().await?;
        let remote_available = self.index_is_remote_confirmed(&stored);
        let (asset, package) = resolve_asset_package(&stored.index, remote_asset_id)?;
        let packages = ordered_dependency_packages(&stored.index, package)?;
        let mut inputs = Vec::new();
        for dependency_package in packages {
            let requested = if dependency_package.name == package.name {
                Some(asset)
            } else {
                None
            };
            let members = ordered_package_members(&stored.index, dependency_package, requested)?;
            match installed_package_state(user_id, catalog, dependency_package, &members, remote_available).await? {
                InstalledPackageState::Missing => {
                    for member in members {
                        self.require_compatible(member)?;
                        let files = self
                            .load_verified_asset_files(&stored, dependency_package, member)
                            .await?;
                        inputs.push(tracked_input(member, &stored.index, local_asset_id(&member.id), files));
                    }
                }
                InstalledPackageState::Reusable if dependency_package.name != package.name => {}
                InstalledPackageState::Reusable => {
                    return Err(MarketError::Invalid(format!(
                        "原子包 {} 已完整安装，不能重复安装",
                        package.name
                    )));
                }
            }
        }
        let operation_asset_id = local_asset_id(remote_asset_id);
        catalog
            .install_tracked_closure(user_id, idempotency_key, &operation_asset_id, inputs)
            .await
            .map_err(Into::into)
    }

    pub async fn sync_asset(
        &self,
        user_id: &str,
        catalog: &AssetCatalogService,
        remote_asset_id: &str,
        idempotency_key: &str,
    ) -> Result<AssetOperationResponse, MarketError> {
        let stored = self.load_or_refresh_cache().await?;
        let remote_available = self.index_is_remote_confirmed(&stored);
        let (asset, package) = resolve_asset_package(&stored.index, remote_asset_id)?;
        let target_members = ordered_package_members(&stored.index, package, Some(asset))?;
        let mut sync_inputs = Vec::with_capacity(target_members.len());
        let mut operation_asset_id = None;
        for member in target_members {
            self.require_compatible(member)?;
            let relation = catalog
                .relation_for_remote(user_id, &member.id, &member.definition_digest, true, remote_available)
                .await?
                .ok_or_else(|| {
                    MarketError::Invalid(format!(
                        "原子包 {} 的资产 {} 尚未安装，拒绝同步部分 Bundle",
                        package.name, member.id
                    ))
                })?;
            if member.id == remote_asset_id {
                operation_asset_id = Some(relation.local.local_asset_id.clone());
            }
            let files = self.load_verified_asset_files(&stored, package, member).await?;
            sync_inputs.push(tracked_input(
                member,
                &stored.index,
                relation.local.local_asset_id,
                files,
            ));
        }
        let mut install_inputs = Vec::new();
        for dependency_package in ordered_dependency_packages(&stored.index, package)? {
            if dependency_package.name == package.name {
                continue;
            }
            let members = ordered_package_members(&stored.index, dependency_package, None)?;
            match installed_package_state(user_id, catalog, dependency_package, &members, remote_available).await? {
                InstalledPackageState::Reusable => {}
                InstalledPackageState::Missing => {
                    for member in members {
                        self.require_compatible(member)?;
                        let files = self
                            .load_verified_asset_files(&stored, dependency_package, member)
                            .await?;
                        install_inputs.push(tracked_input(member, &stored.index, local_asset_id(&member.id), files));
                    }
                }
            }
        }
        catalog
            .sync_fast_forward_closure(
                user_id,
                idempotency_key,
                operation_asset_id
                    .as_deref()
                    .ok_or_else(|| MarketError::Invalid("目标资产缺少本地关系".into()))?,
                sync_inputs,
                install_inputs,
            )
            .await
            .map_err(Into::into)
    }

    pub async fn read_asset_file(
        &self,
        remote_asset_id: &str,
        requested_path: &str,
    ) -> Result<AssetFileResponse, MarketError> {
        let stored = self.load_or_refresh_cache().await?;
        let (asset, package) = resolve_asset_package(&stored.index, remote_asset_id)?;
        let path = normalize_relative_path(requested_path)
            .map_err(|_| MarketError::Invalid("远程资产文件路径不安全".into()))?;
        if !asset.files.iter().any(|file| file.path == path) {
            return Err(MarketError::Invalid(format!(
                "远程资产 {remote_asset_id} 不包含文件 {path}"
            )));
        }
        let files = self.load_verified_asset_files(&stored, package, asset).await?;
        let file = files
            .into_iter()
            .find(|file| file.path == path)
            .ok_or_else(|| MarketError::Invalid(format!("远程资产缺少文件 {path}")))?;
        let content = String::from_utf8(file.content).map_err(|_| crate::AssetError::BinaryFile(path.clone()))?;
        let descriptor = asset
            .files
            .iter()
            .find(|value| value.path == path)
            .ok_or_else(|| MarketError::Invalid(format!("远程资产缺少文件清单 {path}")))?;
        Ok(AssetFileResponse {
            asset_id: remote_asset_id.to_owned(),
            path,
            digest: descriptor.digest.clone(),
            media_type: descriptor.media_type.clone(),
            content,
        })
    }

    /// Loads the current commit-pinned remote Definition before comparing.
    /// A missing package cache is fetched and fully verified here, never
    /// represented as an empty remote tree.
    pub async fn diff_local_asset(
        &self,
        user_id: &str,
        catalog: &AssetCatalogService,
        local_asset_id: &str,
    ) -> Result<AssetDiffResponse, MarketError> {
        let input = self.current_remote_input(user_id, catalog, local_asset_id).await?;
        catalog
            .diff_against_remote(user_id, local_asset_id, &input)
            .await
            .map_err(Into::into)
    }

    pub async fn resolve_local_asset(
        &self,
        user_id: &str,
        catalog: &AssetCatalogService,
        local_asset_id: &str,
        request: ResolveAssetRequest,
    ) -> Result<AssetResolveResponse, MarketError> {
        let input = self.current_remote_input(user_id, catalog, local_asset_id).await?;
        catalog
            .resolve_against_remote(user_id, local_asset_id, input, request)
            .await
            .map_err(Into::into)
    }

    async fn current_remote_input(
        &self,
        user_id: &str,
        catalog: &AssetCatalogService,
        local_asset_id: &str,
    ) -> Result<TrackedAssetInput, MarketError> {
        let reference = catalog.tracked_reference(user_id, local_asset_id).await?;
        let stored = self.load_or_refresh_cache().await?;
        let (asset, package) = resolve_asset_package(&stored.index, &reference.remote_asset_id)?;
        self.require_compatible(asset)?;
        let source_revision = &stored.index.metadata.source_revision;
        if package.name != reference.package_name
            || package.source_revision != *source_revision
            || asset.source_revision != *source_revision
        {
            return Err(MarketError::Asset(AssetError::UpstreamMismatch));
        }
        let files = self.load_verified_asset_files(&stored, package, asset).await?;
        Ok(tracked_input(asset, &stored.index, local_asset_id.to_owned(), files))
    }

    async fn load_or_refresh_cache(&self) -> Result<StoredMarketCache, MarketError> {
        if self.uses_development_seed() {
            return self.import_offline_seed();
        }
        match self.load_cached_index() {
            Ok(stored) => Ok(stored),
            Err(MarketError::Unavailable | MarketError::Invalid(_)) => match self.refresh(None).await {
                Ok(_) => self.load_cached_index(),
                Err(MarketError::Network(_) | MarketError::Unavailable) => self.import_offline_seed(),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    fn uses_development_seed(&self) -> bool {
        matches!(&self.offline_seed, OfflineSeedSetting::Source(source) if source.development)
    }

    fn require_compatible(&self, asset: &RawMarketAsset) -> Result<(), MarketError> {
        if asset.status == MarketAssetStatus::Revoked {
            return Err(MarketError::Invalid(format!(
                "资产 {} 已被安全撤销，禁止安装、同步、合并或试跑",
                asset.id
            )));
        }
        let host = Version::parse(&self.asset_protocol_version).map_err(|_| {
            MarketError::Invalid(format!("资产协议版本 {} 不是合法 SemVer", self.asset_protocol_version))
        })?;
        let requirement = VersionReq::parse(&asset.compatibility.tjuae)
            .map_err(|_| MarketError::Invalid(format!("资产 {} 的兼容范围无效", asset.id)))?;
        if requirement.matches(&host) {
            Ok(())
        } else {
            Err(MarketError::Invalid(format!(
                "资产 {} 不兼容当前资产协议 {}",
                asset.id, self.asset_protocol_version
            )))
        }
    }

    async fn load_verified_asset_files(
        &self,
        stored: &StoredMarketCache,
        package: &MarketPackageDescriptor,
        asset: &RawMarketAsset,
    ) -> Result<Vec<AssetDefinitionFile>, MarketError> {
        let _guard = self.package_lock.lock().await;
        let package_cache = self.package_cache_path(&package.archive_integrity)?;
        let mut cached = if package_cache.is_file() {
            Some(std::fs::read(&package_cache)?)
        } else {
            None
        };
        if cached
            .as_ref()
            .is_some_and(|bytes| digest_bytes(bytes) != package.archive_integrity)
        {
            cached = None;
        }
        let bytes = if let Some(bytes) = cached {
            bytes
        } else {
            let bytes = if stored.origin == MarketCacheOrigin::OfflineSeed {
                self.offline_package_bytes(package)?
            } else {
                let distribution_revision = stored
                    .distribution_revision
                    .as_deref()
                    .ok_or_else(|| MarketError::Invalid("远程市场缓存缺少 dist 发布修订".into()))?;
                let url = package_url(distribution_revision, &package.tarball);
                match self.remote.fetch_package(&url).await {
                    Ok(bytes) => bytes,
                    Err(MarketError::Network(_) | MarketError::Unavailable) => {
                        self.clear_remote_confirmation();
                        self.offline_package_bytes(package)?
                    }
                    Err(error) => return Err(error),
                }
            };
            if digest_bytes(&bytes) != package.archive_integrity {
                return Err(MarketError::Invalid(format!(
                    "包 {} 的 ZIP 字节摘要不一致",
                    package.name
                )));
            }
            let files = verify_package_archive(&bytes, package, asset)?;
            if let Some(parent) = package_cache.parent() {
                std::fs::create_dir_all(parent)?;
            }
            atomic_write(&package_cache, &bytes)?;
            return Ok(files);
        };
        verify_package_archive(&bytes, package, asset)
    }

    fn package_cache_path(&self, archive_integrity: &str) -> Result<PathBuf, MarketError> {
        validate_integrity(archive_integrity)?;
        let digest = archive_integrity
            .strip_prefix("sha256-")
            .ok_or_else(|| MarketError::Invalid("包摘要无效".into()))?;
        Ok(self.cache_dir.join("packages").join(format!("{digest}.zip")))
    }

    async fn response_from_index(
        &self,
        stored: StoredMarketCache,
        user_id: &str,
        catalog: &AssetCatalogService,
        query: &ListMarketAssetsQuery,
    ) -> Result<MarketIndexResponse, MarketError> {
        // An offline seed or an expired cache is safe for browsing and local
        // execution, but it is not proof that the remote repository is still
        // current. Never present a tracked local asset as synchronized when
        // Core could not establish current remote availability.
        let remote_available = self.index_is_remote_confirmed(&stored);
        let host = Version::parse(&self.asset_protocol_version).map_err(|_| {
            MarketError::Invalid(format!("资产协议版本 {} 不是合法 SemVer", self.asset_protocol_version))
        })?;
        let search = query.search.as_deref().map(str::trim).filter(|value| !value.is_empty());
        let search = search.map(|value| value.to_lowercase());
        let mut descriptors = Vec::new();
        let mut relation_inputs = BTreeMap::new();

        for raw in stored.index.assets.values() {
            let requirement = VersionReq::parse(&raw.compatibility.tjuae)
                .map_err(|_| MarketError::Invalid(format!("资产 {} 的 compatibility.tjuae 无效", raw.id)))?;
            let protocol_compatible = requirement.matches(&host);
            let compatible = protocol_compatible;
            relation_inputs.insert(raw.id.clone(), (raw.definition_digest.clone(), compatible));
            if query.kind.is_some_and(|kind| kind != raw.kind) || !matches_search(raw, search.as_deref()) {
                continue;
            }
            descriptors.push(MarketAssetDescriptor {
                id: raw.id.clone(),
                kind: raw.kind,
                runtime_id: raw.runtime_id.clone(),
                dependencies: raw.dependencies.clone(),
                display_name: raw.display_name.clone(),
                description: raw.description.clone(),
                version: raw.version.clone(),
                definition_digest: raw.definition_digest.clone(),
                entry_file: raw.entry_file.clone(),
                package_name: raw.package_name.clone(),
                author: raw.author.clone(),
                license: raw.license.clone(),
                trust: raw.trust,
                status: raw.status,
                compatibility: MarketCompatibilityResponse {
                    compatible,
                    tjuae: raw.compatibility.tjuae.clone(),
                    reason_code: if !protocol_compatible {
                        Some("TJUAE_VERSION_UNSUPPORTED".to_owned())
                    } else {
                        None
                    },
                },
                source_revision: raw.source_revision.clone(),
                files: raw.files.clone(),
                tags: raw.tags.clone(),
            });
        }

        let mut relations = catalog
            .relations_for_remotes(user_id, &relation_inputs, remote_available)
            .await?;
        let assets = descriptors
            .into_iter()
            .map(|asset| {
                if let Some(mut relation) = relations.remove(&asset.id) {
                    if asset.status == MarketAssetStatus::Revoked {
                        relation.sync_state = AssetSyncState::Revoked;
                        retain_revoked_actions(&mut relation.allowed_actions);
                    }
                    MarketAssetResponse {
                        asset,
                        presence_state: MarketPresenceState::Installed,
                        sync_state: Some(relation.sync_state),
                        allowed_actions: relation.allowed_actions,
                        local: Some(relation.local),
                    }
                } else {
                    let compatible = asset.compatibility.compatible && asset.status != MarketAssetStatus::Revoked;
                    MarketAssetResponse {
                        asset,
                        presence_state: MarketPresenceState::NotInstalled,
                        sync_state: None,
                        allowed_actions: if compatible {
                            vec![AssetAction::View, AssetAction::Install]
                        } else {
                            vec![AssetAction::View]
                        },
                        local: None,
                    }
                }
            })
            .collect();

        let cache = cache_response(&stored);
        Ok(MarketIndexResponse {
            schema_version: stored.index.schema_version,
            generated_at: stored.index.generated_at,
            assets,
            packages: stored.index.packages.into_values().collect(),
            cache,
        })
    }

    fn relation_inputs(&self, index: &RawMarketIndex) -> Result<BTreeMap<String, (String, bool)>, MarketError> {
        let host = Version::parse(&self.asset_protocol_version).map_err(|_| {
            MarketError::Invalid(format!("资产协议版本 {} 不是合法 SemVer", self.asset_protocol_version))
        })?;
        let mut inputs = BTreeMap::new();
        for asset in index.assets.values() {
            let requirement = VersionReq::parse(&asset.compatibility.tjuae)
                .map_err(|_| MarketError::Invalid(format!("资产 {} 的 compatibility.tjuae 无效", asset.id)))?;
            let compatible = requirement.matches(&host);
            inputs.insert(asset.id.clone(), (asset.definition_digest.clone(), compatible));
        }
        Ok(inputs)
    }

    fn index_is_remote_confirmed(&self, stored: &StoredMarketCache) -> bool {
        stored.origin == MarketCacheOrigin::Remote
            && stored.distribution_revision.as_deref().is_some_and(|revision| {
                self.confirmed_distribution_revision
                    .read()
                    .is_ok_and(|confirmed| confirmed.as_deref() == Some(revision))
            })
    }

    fn confirm_distribution_revision(&self, distribution_revision: &str) {
        if let Ok(mut confirmed) = self.confirmed_distribution_revision.write() {
            *confirmed = Some(distribution_revision.to_owned());
        }
    }

    fn clear_remote_confirmation(&self) {
        if let Ok(mut confirmed) = self.confirmed_distribution_revision.write() {
            *confirmed = None;
        }
    }

    fn index_file_path(&self) -> PathBuf {
        self.cache_dir.join(INDEX_CACHE_FILE)
    }

    fn load_cached_index(&self) -> Result<StoredMarketCache, MarketError> {
        let path = self.index_file_path();
        let backup = backup_path(&path);
        let mut found = false;
        let mut last_error = None;
        for candidate in [&path, &backup] {
            if !candidate.is_file() {
                continue;
            }
            found = true;
            let loaded = (|| {
                let bytes = std::fs::read(candidate)?;
                let stored: StoredMarketCache =
                    serde_json::from_slice(&bytes).map_err(|error| MarketError::Invalid(error.to_string()))?;
                if let Some(distribution_revision) = stored.distribution_revision.as_deref() {
                    validate_commit_sha(distribution_revision)?;
                }
                validate_index(&stored.index)?;
                let expected_source = match stored.origin {
                    MarketCacheOrigin::Remote => index_url(
                        stored
                            .distribution_revision
                            .as_deref()
                            .ok_or_else(|| MarketError::Invalid("远程市场缓存缺少 dist 发布修订".into()))?,
                    ),
                    MarketCacheOrigin::OfflineSeed => offline_seed_url(&stored.index.metadata.source_revision),
                };
                if stored.source_url != expected_source {
                    return Err(MarketError::Invalid("缓存来源与市场缓存类型不一致".into()));
                }
                Ok::<_, MarketError>(stored)
            })();
            match loaded {
                Ok(stored) => return Ok(stored),
                Err(error) => last_error = Some(error),
            }
        }
        if found {
            Err(last_error.unwrap_or_else(|| MarketError::Invalid("远程市场缓存损坏".into())))
        } else {
            Err(MarketError::Unavailable)
        }
    }

    fn persist_cache(&self, stored: &StoredMarketCache) -> Result<(), MarketError> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let bytes = serde_json::to_vec_pretty(stored)?;
        atomic_write(&self.index_file_path(), &bytes)
    }

    fn import_offline_seed(&self) -> Result<StoredMarketCache, MarketError> {
        let loaded = self.load_offline_seed_bundle()?;
        for (package_name, bytes) in &loaded.packages {
            let package = loaded
                .stored
                .index
                .packages
                .get(package_name)
                .ok_or_else(|| MarketError::Invalid(format!("离线种子缺少包描述 {package_name}")))?;
            let destination = self.package_cache_path(&package.archive_integrity)?;
            if destination.is_file()
                && std::fs::read(&destination).is_ok_and(|cached| digest_bytes(&cached) == package.archive_integrity)
            {
                continue;
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            atomic_write(&destination, bytes)?;
        }
        self.persist_cache(&loaded.stored)?;
        tracing::info!(
            source_revision = loaded.stored.index.metadata.source_revision,
            distribution_revision = loaded.stored.distribution_revision.as_deref(),
            assets = loaded.stored.index.assets.len(),
            packages = loaded.stored.index.packages.len(),
            "market: imported verified bundled offline seed"
        );
        Ok(loaded.stored)
    }

    fn offline_package_bytes(&self, package: &MarketPackageDescriptor) -> Result<Vec<u8>, MarketError> {
        let loaded = self.load_offline_seed_bundle()?;
        let offline_package = loaded
            .stored
            .index
            .packages
            .get(&package.name)
            .ok_or(MarketError::Unavailable)?;
        if offline_package.archive_integrity != package.archive_integrity {
            return Err(MarketError::Unavailable);
        }
        loaded
            .packages
            .get(&package.name)
            .cloned()
            .ok_or(MarketError::Unavailable)
    }

    fn load_offline_seed_bundle(&self) -> Result<LoadedOfflineSeed, MarketError> {
        match &self.offline_seed {
            OfflineSeedSetting::None => Err(MarketError::Unavailable),
            OfflineSeedSetting::Invalid(reason) => Err(MarketError::Invalid(format!("离线种子启动配置无效：{reason}"))),
            OfflineSeedSetting::Source(source) => load_offline_seed(source),
        }
    }
}

fn offline_seed_from_env() -> OfflineSeedSetting {
    let directory = std::env::var_os(OFFLINE_SEED_DIR_ENV).filter(|value| !value.is_empty());
    let manifest = std::env::var_os(OFFLINE_SEED_MANIFEST_ENV).filter(|value| !value.is_empty());
    if directory.is_none() && manifest.is_none() {
        return OfflineSeedSetting::None;
    }
    let result = (|| {
        let directory = PathBuf::from(directory.ok_or_else(|| format!("缺少 {OFFLINE_SEED_DIR_ENV}"))?);
        let manifest_path = PathBuf::from(manifest.ok_or_else(|| format!("缺少 {OFFLINE_SEED_MANIFEST_ENV}"))?);
        if !directory.is_absolute() || !manifest_path.is_absolute() {
            return Err("离线种子目录和清单路径必须是绝对路径".into());
        }
        let directory = directory
            .canonicalize()
            .map_err(|error| format!("无法解析离线种子目录：{error}"))?;
        let manifest_path = manifest_path
            .canonicalize()
            .map_err(|error| format!("无法解析离线种子清单：{error}"))?;
        if manifest_path.parent() != Some(directory.as_path())
            || manifest_path.file_name().and_then(|value| value.to_str()) != Some("manifest.json")
        {
            return Err("离线种子清单必须是资源目录直属的 manifest.json".into());
        }
        let dist_ref = std::env::var(OFFLINE_DIST_REF_ENV)
            .ok()
            .filter(|value| !value.is_empty());
        if let Some(value) = &dist_ref {
            validate_commit_sha(value).map_err(|_| format!("{OFFLINE_DIST_REF_ENV} 不是有效提交哈希"))?;
            if value.len() != 40 {
                return Err(format!("{OFFLINE_DIST_REF_ENV} 必须是 GitHub 40 位提交哈希"));
            }
        }
        let development = match std::env::var(OFFLINE_DEVELOPMENT_ENV).ok().as_deref() {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(_) => return Err(format!("{OFFLINE_DEVELOPMENT_ENV} 只能是 0 或 1")),
        };
        if dist_ref.is_none() && !development {
            return Err(format!(
                "固定分发资源必须提供 {OFFLINE_DIST_REF_ENV}；本地 sibling 仅可显式启用开发模式"
            ));
        }
        if dist_ref.is_some() && development {
            return Err("固定 dist 引用与本地开发模式不能同时启用".into());
        }
        Ok(OfflineSeedSource {
            directory,
            manifest_path,
            dist_ref,
            development,
        })
    })();
    match result {
        Ok(source) => OfflineSeedSetting::Source(source),
        Err(reason) => OfflineSeedSetting::Invalid(reason),
    }
}

fn load_offline_seed(source: &OfflineSeedSource) -> Result<LoadedOfflineSeed, MarketError> {
    let directory = source
        .directory
        .canonicalize()
        .map_err(|error| MarketError::Invalid(format!("离线种子目录不可用：{error}")))?;
    let manifest_path = source
        .manifest_path
        .canonicalize()
        .map_err(|error| MarketError::Invalid(format!("离线种子资源清单不可用：{error}")))?;
    if manifest_path.parent() != Some(directory.as_path())
        || manifest_path.file_name().and_then(|value| value.to_str()) != Some("manifest.json")
    {
        return Err(MarketError::Invalid("离线种子资源清单越出固定资源目录".into()));
    }

    let runtime_bytes = read_regular_file_bounded(&manifest_path, OFFLINE_MANIFEST_MAX_BYTES, "离线种子资源清单")?;
    let runtime: OfflineResourceManifest =
        serde_json::from_slice(&runtime_bytes).map_err(|error| MarketError::Invalid(error.to_string()))?;
    validate_offline_resource_manifest(&runtime, source)?;

    let seed_manifest_path = safe_offline_resource_path(&directory, &runtime.seed_manifest.file_name)?;
    let seed_manifest_bytes =
        read_regular_file_bounded(&seed_manifest_path, OFFLINE_MANIFEST_MAX_BYTES, "Hub 离线种子清单")?;
    validate_resource_file_bytes(&seed_manifest_bytes, &runtime.seed_manifest, "Hub 离线种子清单")?;
    let seed: OfflineSeedManifest = serde_json::from_slice(&seed_manifest_bytes)
        .map_err(|error| MarketError::Invalid(format!("Hub 离线种子清单不是有效 JSON：{error}")))?;
    validate_offline_seed_manifest(&seed)?;
    if runtime.bundle.file_name != seed.bundle.file_name
        || runtime.bundle.digest != seed.bundle.digest
        || runtime.bundle.size != seed.bundle.size
    {
        return Err(MarketError::Invalid(
            "资源清单与 Hub 离线种子清单的 bundle 不一致".into(),
        ));
    }
    if runtime.source.kind == "localSibling"
        && runtime.source.source_revision.as_deref() != Some(seed.source_revision.as_str())
    {
        return Err(MarketError::Invalid("本地 sibling 来源修订与离线种子不一致".into()));
    }

    let bundle_path = safe_offline_resource_path(&directory, &seed.bundle.file_name)?;
    let bundle_bytes = read_regular_file_bounded(&bundle_path, OFFLINE_SEED_BUNDLE_MAX_BYTES, "Hub 离线种子 ZIP")?;
    validate_resource_file_bytes(&bundle_bytes, &seed.bundle, "Hub 离线种子 ZIP")?;
    let (seed_index_bytes, packages) = read_offline_seed_archive(&bundle_bytes, &seed)?;
    if digest_bytes(&seed_index_bytes) != seed.seed_index_digest {
        return Err(MarketError::Invalid("seed-index.json 摘要与离线种子清单不一致".into()));
    }
    let index: RawMarketIndex = serde_json::from_slice(&seed_index_bytes)
        .map_err(|error| MarketError::Invalid(format!("seed-index.json 不是有效 JSON：{error}")))?;
    validate_index(&index)?;
    validate_seed_index_matches_manifest(&index, &seed, &packages)?;

    Ok(LoadedOfflineSeed {
        stored: StoredMarketCache {
            index,
            distribution_revision: runtime.source.dist_ref.clone(),
            cached_at: now_ms(),
            source_url: offline_seed_url(&seed.source_revision),
            origin: MarketCacheOrigin::OfflineSeed,
        },
        packages,
    })
}

fn validate_offline_resource_manifest(
    manifest: &OfflineResourceManifest,
    source: &OfflineSeedSource,
) -> Result<(), MarketError> {
    if manifest.schema != OFFLINE_RESOURCE_MANIFEST_SCHEMA || manifest.schema_version != 1 {
        return Err(MarketError::Invalid("离线资源清单 schema 不受支持".into()));
    }
    if normalize_repository_url(&manifest.source.repository) != HUB_REPOSITORY {
        return Err(MarketError::Invalid("离线资源清单仓库来源不受信任".into()));
    }
    validate_resource_file_descriptor(&manifest.seed_manifest, OFFLINE_MANIFEST_MAX_BYTES)?;
    if manifest.seed_manifest.file_name != "seed-manifest.json" {
        return Err(MarketError::Invalid("离线资源清单必须引用 seed-manifest.json".into()));
    }
    validate_resource_file_descriptor(&manifest.bundle, OFFLINE_SEED_BUNDLE_MAX_BYTES)?;

    match manifest.source.kind.as_str() {
        "pinnedDist" => {
            if source.development || manifest.source.source_revision.is_some() {
                return Err(MarketError::Invalid("固定 dist 离线资源不能声明开发来源".into()));
            }
            let manifest_ref = manifest
                .source
                .dist_ref
                .as_deref()
                .ok_or_else(|| MarketError::Invalid("固定 dist 离线资源缺少 distRef".into()))?;
            validate_commit_sha(manifest_ref)?;
            if manifest_ref.len() != 40 || source.dist_ref.as_deref() != Some(manifest_ref) {
                return Err(MarketError::Invalid("启动参数与离线资源 distRef 不一致".into()));
            }
        }
        "localSibling" => {
            if !source.development || source.dist_ref.is_some() || manifest.source.dist_ref.is_some() {
                return Err(MarketError::Invalid(
                    "本地 sibling 离线资源只能在显式开发模式使用".into(),
                ));
            }
            validate_commit_sha(
                manifest
                    .source
                    .source_revision
                    .as_deref()
                    .ok_or_else(|| MarketError::Invalid("本地 sibling 来源缺少 sourceRevision".into()))?,
            )?;
        }
        _ => return Err(MarketError::Invalid("离线资源来源类型不受支持".into())),
    }
    Ok(())
}

fn validate_offline_seed_manifest(seed: &OfflineSeedManifest) -> Result<(), MarketError> {
    if seed.schema != OFFLINE_SEED_SCHEMA_URL || seed.schema_version != 1 {
        return Err(MarketError::Invalid("Hub 离线种子清单 schema 不受支持".into()));
    }
    chrono::DateTime::parse_from_rfc3339(&seed.generated_at)
        .map_err(|_| MarketError::Invalid("离线种子 generatedAt 不是 RFC 3339 时间".into()))?;
    validate_commit_sha(&seed.source_revision)?;
    validate_integrity(&seed.seed_index_digest)?;
    validate_resource_file_descriptor(&seed.bundle, OFFLINE_SEED_BUNDLE_MAX_BYTES)?;
    let bundle_digest = seed
        .bundle
        .digest
        .strip_prefix("sha256-")
        .ok_or_else(|| MarketError::Invalid("离线种子 bundle 摘要无效".into()))?;
    if seed.bundle.file_name != format!("tjuae-seed-{bundle_digest}.zip") {
        return Err(MarketError::Invalid("离线种子文件名与摘要不一致".into()));
    }
    if seed.asset_kinds
        != [
            AssetKind::Assistant,
            AssetKind::EngineAdapter,
            AssetKind::Mcp,
            AssetKind::Skill,
        ]
    {
        return Err(MarketError::Invalid("离线种子必须确定性覆盖全部四类原子资产".into()));
    }
    validate_sorted_unique(&seed.package_names, "packageNames")?;
    validate_sorted_unique(&seed.asset_ids, "assetIds")?;
    if seed.package_names.is_empty() || seed.asset_ids.is_empty() {
        return Err(MarketError::Invalid("离线种子必须包含官方资产和原子包".into()));
    }
    for package_name in &seed.package_names {
        validate_package_name(package_name)?;
    }
    Ok(())
}

fn validate_resource_file_descriptor(descriptor: &OfflineResourceFile, limit: u64) -> Result<(), MarketError> {
    validate_integrity(&descriptor.digest)?;
    if descriptor.file_name.is_empty()
        || descriptor.file_name.contains(['/', '\\', ':'])
        || descriptor.file_name.contains("..")
    {
        return Err(MarketError::Invalid("离线资源文件名不安全".into()));
    }
    if descriptor.size == 0 || descriptor.size > limit {
        return Err(MarketError::TooLarge {
            actual: descriptor.size,
            limit,
        });
    }
    Ok(())
}

fn validate_resource_file_bytes(
    bytes: &[u8],
    descriptor: &OfflineResourceFile,
    label: &str,
) -> Result<(), MarketError> {
    if bytes.len() as u64 != descriptor.size {
        return Err(MarketError::Invalid(format!("{label} 文件大小与清单不一致")));
    }
    if digest_bytes(bytes) != descriptor.digest {
        return Err(MarketError::Invalid(format!("{label} 摘要与清单不一致")));
    }
    Ok(())
}

fn safe_offline_resource_path(directory: &Path, file_name: &str) -> Result<PathBuf, MarketError> {
    if file_name.is_empty() || file_name.contains(['/', '\\', ':']) || file_name.contains("..") {
        return Err(MarketError::Invalid("离线资源文件名不安全".into()));
    }
    let path = directory.join(file_name);
    let canonical = path
        .canonicalize()
        .map_err(|error| MarketError::Invalid(format!("离线资源文件不可用：{error}")))?;
    if canonical.parent() != Some(directory) {
        return Err(MarketError::Invalid("离线资源文件越出固定目录".into()));
    }
    Ok(canonical)
}

fn read_regular_file_bounded(path: &Path, limit: u64, label: &str) -> Result<Vec<u8>, MarketError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MarketError::Invalid(format!("{label} 必须是普通文件")));
    }
    if metadata.len() > limit {
        return Err(MarketError::TooLarge {
            actual: metadata.len(),
            limit,
        });
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(MarketError::Invalid(format!("{label} 读取期间发生变化")));
    }
    Ok(bytes)
}

fn read_offline_seed_archive(
    bytes: &[u8],
    seed: &OfflineSeedManifest,
) -> Result<OfflineSeedArchiveContents, MarketError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| MarketError::Invalid(format!("离线种子不是有效 ZIP：{error}")))?;
    if archive.len() > OFFLINE_SEED_MAX_ENTRIES {
        return Err(MarketError::Invalid("离线种子 ZIP 条目过多".into()));
    }
    let expected = std::iter::once("seed-index.json".to_owned())
        .chain(seed.package_names.iter().map(|name| format!("packages/{name}.zip")))
        .collect::<BTreeSet<_>>();
    let mut entries = BTreeMap::new();
    let mut seen_case = BTreeSet::new();
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| MarketError::Invalid(format!("读取离线种子 ZIP 失败：{error}")))?;
        let raw_name = entry.name().to_owned();
        if entry.is_dir()
            || raw_name.contains('\\')
            || raw_name.starts_with('/')
            || raw_name.as_bytes().get(1) == Some(&b':')
            || normalize_relative_path(&raw_name).is_err()
            || !expected.contains(&raw_name)
        {
            return Err(MarketError::Invalid(format!(
                "离线种子包含不安全或未声明的 ZIP 路径：{raw_name}"
            )));
        }
        if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(MarketError::Invalid("离线种子不允许符号链接".into()));
        }
        if !seen_case.insert(raw_name.to_lowercase()) {
            return Err(MarketError::Invalid("离线种子包含冲突 ZIP 路径".into()));
        }
        let limit = if raw_name == "seed-index.json" {
            INDEX_MAX_BYTES as u64
        } else {
            PACKAGE_MAX_ARCHIVE_BYTES as u64
        };
        if entry.size() > limit {
            return Err(MarketError::TooLarge {
                actual: entry.size(),
                limit,
            });
        }
        total_size = total_size
            .checked_add(entry.size())
            .ok_or_else(|| MarketError::Invalid("离线种子解压大小溢出".into()))?;
        if total_size > OFFLINE_SEED_BUNDLE_MAX_BYTES {
            return Err(MarketError::TooLarge {
                actual: total_size,
                limit: OFFLINE_SEED_BUNDLE_MAX_BYTES,
            });
        }
        let expected_size = entry.size();
        let mut content = Vec::with_capacity(expected_size as usize);
        (&mut entry)
            .take(limit + 1)
            .read_to_end(&mut content)
            .map_err(MarketError::Io)?;
        if content.len() as u64 != expected_size {
            return Err(MarketError::Invalid(format!("离线种子条目 {raw_name} 解压大小不一致")));
        }
        entries.insert(raw_name, content);
    }
    if entries.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(MarketError::Invalid("离线种子 ZIP 条目集合与清单不一致".into()));
    }
    let seed_index = entries
        .remove("seed-index.json")
        .ok_or_else(|| MarketError::Invalid("离线种子缺少 seed-index.json".into()))?;
    let packages = seed
        .package_names
        .iter()
        .map(|name| {
            entries
                .remove(&format!("packages/{name}.zip"))
                .map(|bytes| (name.clone(), bytes))
                .ok_or_else(|| MarketError::Invalid(format!("离线种子缺少包 {name}")))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok((seed_index, packages))
}

fn validate_seed_index_matches_manifest(
    index: &RawMarketIndex,
    seed: &OfflineSeedManifest,
    packages: &BTreeMap<String, Vec<u8>>,
) -> Result<(), MarketError> {
    if index.generated_at != seed.generated_at || index.metadata.source_revision != seed.source_revision {
        return Err(MarketError::Invalid("seed-index 元数据与离线种子清单不一致".into()));
    }
    let package_names = index.packages.keys().cloned().collect::<Vec<_>>();
    let asset_ids = index.assets.keys().cloned().collect::<Vec<_>>();
    if package_names != seed.package_names || asset_ids != seed.asset_ids {
        return Err(MarketError::Invalid(
            "seed-index 资产或包集合与离线种子清单不一致".into(),
        ));
    }
    let mut actual_kinds = index
        .assets
        .values()
        .map(|asset| kind_segment(asset.kind).to_owned())
        .collect::<Vec<_>>();
    actual_kinds.sort();
    actual_kinds.dedup();
    if actual_kinds != ["assistant", "engineAdapter", "mcp", "skill"] {
        return Err(MarketError::Invalid("seed-index 未精确覆盖全部四类官方原子资产".into()));
    }
    for (asset_id, asset) in &index.assets {
        if asset.trust != AssetTrust::Official || asset.status == MarketAssetStatus::Revoked {
            return Err(MarketError::Invalid(format!(
                "离线种子资产 {asset_id} 未通过四类官方资产策略"
            )));
        }
    }
    for (package_name, package) in &index.packages {
        let bytes = packages
            .get(package_name)
            .ok_or_else(|| MarketError::Invalid(format!("离线种子缺少包字节 {package_name}")))?;
        for asset_id in &package.asset_ids {
            let asset = index
                .assets
                .get(asset_id)
                .ok_or_else(|| MarketError::Invalid(format!("离线种子包 {package_name} 缺少资产 {asset_id}")))?;
            verify_package_archive(bytes, package, asset)?;
        }
    }
    Ok(())
}

fn validate_sorted_unique(values: &[String], label: &str) -> Result<(), MarketError> {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted != values {
        return Err(MarketError::Invalid(format!("{label} 必须唯一且按字典序排列")));
    }
    Ok(())
}

fn resolve_asset_package<'a>(
    index: &'a RawMarketIndex,
    remote_asset_id: &str,
) -> Result<(&'a RawMarketAsset, &'a MarketPackageDescriptor), MarketError> {
    let asset = index
        .assets
        .get(remote_asset_id)
        .ok_or_else(|| MarketError::Invalid(format!("远程资产 {remote_asset_id} 不存在")))?;
    let package = index
        .packages
        .get(&asset.package_name)
        .ok_or_else(|| MarketError::Invalid(format!("资产 {remote_asset_id} 缺少原子包")))?;
    if !package.asset_ids.iter().any(|value| value == remote_asset_id) {
        return Err(MarketError::Invalid(format!(
            "资产 {remote_asset_id} 与原子包交叉引用不一致"
        )));
    }
    Ok((asset, package))
}

fn retain_revoked_actions(actions: &mut Vec<AssetAction>) {
    actions.retain(|action| {
        matches!(
            action,
            AssetAction::View
                | AssetAction::Validate
                | AssetAction::Uninstall
                | AssetAction::ViewDiff
                | AssetAction::Detach
        )
    });
}

fn apply_remote_lifecycle_to_local_assets(
    assets: &mut [AssetSummaryResponse],
    remote_assets: &BTreeMap<String, RawMarketAsset>,
) {
    for asset in assets {
        let Some(remote_asset_id) = asset
            .upstream
            .as_ref()
            .map(|upstream| upstream.remote_asset_id.as_str())
        else {
            continue;
        };
        if remote_assets
            .get(remote_asset_id)
            .is_some_and(|remote| remote.status == MarketAssetStatus::Revoked)
        {
            asset.sync_state = Some(AssetSyncState::Revoked);
            retain_revoked_actions(&mut asset.allowed_actions);
        }
    }
}

fn ordered_package_members<'a>(
    index: &'a RawMarketIndex,
    package: &'a MarketPackageDescriptor,
    requested: Option<&'a RawMarketAsset>,
) -> Result<Vec<&'a RawMarketAsset>, MarketError> {
    if !package.atomic || package.asset_ids.is_empty() {
        return Err(MarketError::Invalid(format!("包 {} 不是有效原子包", package.name)));
    }
    let mut ordered_ids = Vec::with_capacity(package.asset_ids.len());
    if let Some(requested) = requested {
        ordered_ids.push(requested.id.as_str());
    }
    ordered_ids.extend(
        package
            .asset_ids
            .iter()
            .map(String::as_str)
            .filter(|asset_id| requested.is_none_or(|requested| *asset_id != requested.id)),
    );
    ordered_ids
        .into_iter()
        .map(|asset_id| {
            let asset = index
                .assets
                .get(asset_id)
                .ok_or_else(|| MarketError::Invalid(format!("原子包 {} 缺少资产 {asset_id}", package.name)))?;
            if asset.package_name != package.name
                || asset.version != package.version
                || asset.source_revision != package.source_revision
            {
                return Err(MarketError::Invalid(format!(
                    "原子包 {} 的成员 {} 来源不一致",
                    package.name, asset.id
                )));
            }
            Ok(asset)
        })
        .collect()
}

fn ordered_dependency_packages<'a>(
    index: &'a RawMarketIndex,
    target: &'a MarketPackageDescriptor,
) -> Result<Vec<&'a MarketPackageDescriptor>, MarketError> {
    fn visit(
        name: &str,
        index: &RawMarketIndex,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) -> Result<(), MarketError> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name.to_owned()) {
            return Err(MarketError::Invalid(format!(
                "资产与原子包的组合依赖图包含循环：{name}"
            )));
        }
        let package = index
            .packages
            .get(name)
            .ok_or_else(|| MarketError::Invalid(format!("依赖原子包 {name} 不存在")))?;
        for dependency_name in package.dependencies.keys() {
            visit(dependency_name, index, visiting, visited, ordered)?;
        }
        for asset_id in &package.asset_ids {
            let asset = index
                .assets
                .get(asset_id)
                .ok_or_else(|| MarketError::Invalid(format!("原子包 {name} 缺少资产 {asset_id}")))?;
            for dependency_id in &asset.dependencies {
                let dependency = index
                    .assets
                    .get(dependency_id)
                    .ok_or_else(|| MarketError::Invalid(format!("资产 {asset_id} 的依赖 {dependency_id} 不存在")))?;
                if dependency.package_name != name {
                    visit(&dependency.package_name, index, visiting, visited, ordered)?;
                }
            }
        }
        visiting.remove(name);
        visited.insert(name.to_owned());
        ordered.push(name.to_owned());
        Ok(())
    }

    let mut ordered = Vec::new();
    visit(
        &target.name,
        index,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        &mut ordered,
    )?;
    ordered
        .into_iter()
        .map(|name| {
            index
                .packages
                .get(&name)
                .ok_or_else(|| MarketError::Invalid(format!("依赖原子包 {name} 不存在")))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstalledPackageState {
    Missing,
    Reusable,
}

async fn installed_package_state(
    user_id: &str,
    catalog: &AssetCatalogService,
    package: &MarketPackageDescriptor,
    members: &[&RawMarketAsset],
    remote_available: bool,
) -> Result<InstalledPackageState, MarketError> {
    let mut installed = 0_usize;
    for asset in members {
        let Some(relation) = catalog
            .relation_for_remote(user_id, &asset.id, &asset.definition_digest, true, remote_available)
            .await?
        else {
            continue;
        };
        installed += 1;
        if relation.sync_state != AssetSyncState::Synced {
            return Err(MarketError::Asset(AssetError::RuntimeProjectionUnsupported {
                code: "ASSET_DEPENDENCY_NOT_REUSABLE",
                message: format!(
                    "依赖资产 {} 当前状态为 {:?}，为避免覆盖本地修改，依赖闭包已停止",
                    asset.id, relation.sync_state
                ),
            }));
        }
        let reference = catalog
            .tracked_reference(user_id, &relation.local.local_asset_id)
            .await?;
        if reference.package_name != package.name
            || reference.remote_asset_id != asset.id
            || reference.version != package.version
            || reference.remote_digest != asset.definition_digest
        {
            return Err(MarketError::Asset(AssetError::RuntimeProjectionUnsupported {
                code: "ASSET_DEPENDENCY_IDENTITY_MISMATCH",
                message: format!("依赖资产 {} 的本地跟踪身份与固定索引不一致", asset.id),
            }));
        }
    }
    match installed {
        0 => Ok(InstalledPackageState::Missing),
        count if count == members.len() => Ok(InstalledPackageState::Reusable),
        _ => Err(MarketError::Asset(AssetError::RuntimeProjectionUnsupported {
            code: "ASSET_DEPENDENCY_PARTIAL_PACKAGE",
            message: format!("依赖原子包 {} 仅安装了部分成员，拒绝继续", package.name),
        })),
    }
}

fn tracked_input(
    asset: &RawMarketAsset,
    index: &RawMarketIndex,
    local_id: String,
    files: Vec<AssetDefinitionFile>,
) -> TrackedAssetInput {
    TrackedAssetInput {
        local: LocalAssetInput {
            id: local_id,
            kind: asset.kind,
            display_name: asset.display_name.clone(),
            description: Some(asset.description.clone()),
            origin: AssetOrigin::Hub,
            trust: asset.trust,
            scope: AssetScope::User,
            editability: AssetEditability::Full,
            entry_file: Some(asset.entry_file.clone()),
            runtime_id: Some(asset.runtime_id.clone()),
            files,
            dependency_runtime_ids: asset
                .dependencies
                .iter()
                .filter_map(|dependency_id| {
                    index
                        .assets
                        .get(dependency_id)
                        .map(|dependency| (dependency_id.clone(), dependency.runtime_id.clone()))
                })
                .collect(),
        },
        package_name: asset.package_name.clone(),
        remote_asset_id: asset.id.clone(),
        version: asset.version.clone(),
        source_revision: asset.source_revision.clone(),
        remote_digest: asset.definition_digest.clone(),
    }
}

fn local_asset_id(remote_asset_id: &str) -> String {
    remote_asset_id.replace('/', ":")
}

fn verify_package_archive(
    bytes: &[u8],
    package: &MarketPackageDescriptor,
    asset: &RawMarketAsset,
) -> Result<Vec<AssetDefinitionFile>, MarketError> {
    if digest_bytes(bytes) != package.archive_integrity {
        return Err(MarketError::Invalid(format!("包 {} 的归档摘要不一致", package.name)));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| MarketError::Invalid(format!("包 {} 不是有效 ZIP：{error}", package.name)))?;
    if archive.len() > PACKAGE_MAX_ENTRIES {
        return Err(MarketError::Invalid(format!("包 {} 的 ZIP 条目过多", package.name)));
    }

    let mut package_files = BTreeMap::<String, Vec<u8>>::new();
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| MarketError::Invalid(format!("读取包 {} 失败：{error}", package.name)))?;
        let raw_name = entry.name().to_owned();
        if raw_name.contains('\\') || raw_name.starts_with('/') || raw_name.as_bytes().get(1) == Some(&b':') {
            return Err(MarketError::Invalid(format!("包 {} 包含不安全 ZIP 路径", package.name)));
        }
        if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(MarketError::Invalid(format!("包 {} 不允许符号链接", package.name)));
        }
        if entry.is_dir() {
            let directory = raw_name.trim_end_matches('/');
            if !directory.is_empty() {
                normalize_relative_path(directory)
                    .map_err(|_| MarketError::Invalid(format!("包 {} 包含不安全目录", package.name)))?;
            }
            continue;
        }
        let path = normalize_relative_path(&raw_name)
            .map_err(|_| MarketError::Invalid(format!("包 {} 包含不安全文件路径", package.name)))?;
        let case_key = path.to_lowercase();
        if package_files.keys().any(|existing| existing.to_lowercase() == case_key) {
            return Err(MarketError::Invalid(format!("包 {} 包含冲突文件路径", package.name)));
        }
        let expected_size = entry.size();
        if expected_size > FILE_MAX_BYTES {
            return Err(MarketError::TooLarge {
                actual: expected_size,
                limit: FILE_MAX_BYTES,
            });
        }
        total_size = total_size
            .checked_add(expected_size)
            .ok_or_else(|| MarketError::Invalid("包大小溢出".into()))?;
        if total_size > PACKAGE_MAX_UNPACKED_BYTES {
            return Err(MarketError::TooLarge {
                actual: total_size,
                limit: PACKAGE_MAX_UNPACKED_BYTES,
            });
        }
        let mut content = Vec::with_capacity(expected_size as usize);
        (&mut entry)
            .take(FILE_MAX_BYTES + 1)
            .read_to_end(&mut content)
            .map_err(MarketError::Io)?;
        if content.len() as u64 != expected_size {
            return Err(MarketError::Invalid(format!(
                "包 {} 的文件 {} 解压大小不一致",
                package.name, path
            )));
        }
        package_files.insert(path, content);
    }
    if total_size != package.unpacked_size {
        return Err(MarketError::Invalid(format!("包 {} 的解压总大小不一致", package.name)));
    }

    let mut content_hash = Sha256::new();
    for (path, content) in &package_files {
        content_hash.update(path.as_bytes());
        content_hash.update(content);
    }
    let actual_package_integrity = format!("sha256-{}", hex::encode(content_hash.finalize()));
    if actual_package_integrity != package.integrity {
        return Err(MarketError::Invalid(format!(
            "包 {} 的规范内容摘要不一致",
            package.name
        )));
    }

    let expected_paths = asset
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    if expected_paths.len() != asset.files.len()
        || expected_paths.len() != package_files.len()
        || package_files.keys().any(|path| !expected_paths.contains(path.as_str()))
    {
        return Err(MarketError::Invalid(format!(
            "资产 {} 的文件清单与原子包不一致",
            asset.id
        )));
    }

    let mut definition = Vec::with_capacity(asset.files.len());
    for expected in &asset.files {
        let content = package_files
            .get(&expected.path)
            .ok_or_else(|| MarketError::Invalid(format!("资产 {} 缺少文件 {}", asset.id, expected.path)))?;
        if content.len() as u64 != expected.size || digest_bytes(content) != expected.digest {
            return Err(MarketError::Invalid(format!(
                "资产 {} 的文件 {} 摘要或大小不一致",
                asset.id, expected.path
            )));
        }
        definition.push(AssetDefinitionFile {
            path: expected.path.clone(),
            content: content.clone(),
        });
    }
    let (_, scanned) = prepare_definition(definition.clone())?;
    if scanned.digest != asset.definition_digest {
        return Err(MarketError::Invalid(format!(
            "资产 {} 的 Definition 摘要不一致",
            asset.id
        )));
    }
    Ok(definition)
}

fn matches_search(asset: &RawMarketAsset, search: Option<&str>) -> bool {
    let Some(search) = search else {
        return true;
    };
    asset.id.to_lowercase().contains(search)
        || asset.display_name.to_lowercase().contains(search)
        || asset.description.to_lowercase().contains(search)
        || asset.tags.iter().any(|tag| tag.to_lowercase().contains(search))
}

fn cache_response(stored: &StoredMarketCache) -> MarketCacheResponse {
    MarketCacheResponse {
        distribution_revision: stored.distribution_revision.clone(),
        cached_at: stored.cached_at,
        source_url: stored.source_url.clone(),
        stale: now_ms().saturating_sub(stored.cached_at) > CACHE_STALE_AFTER_MS,
    }
}

fn validate_index(index: &RawMarketIndex) -> Result<(), MarketError> {
    if index.schema != MARKET_INDEX_SCHEMA_URL {
        return Err(MarketError::Invalid("不支持的 $schema".into()));
    }
    if index.schema_version != 2 {
        return Err(MarketError::Invalid(format!(
            "只支持 schemaVersion 2，实际为 {}",
            index.schema_version
        )));
    }
    chrono::DateTime::parse_from_rfc3339(&index.generated_at)
        .map_err(|_| MarketError::Invalid("generatedAt 不是 RFC 3339 时间".into()))?;
    if index.assets.is_empty() || index.packages.is_empty() {
        return Err(MarketError::Invalid("assets 和 packages 不能为空".into()));
    }
    if index.metadata.total_assets != index.assets.len() as u64
        || index.metadata.total_packages != index.packages.len() as u64
    {
        return Err(MarketError::Invalid("metadata 数量与索引对象不一致".into()));
    }
    if index.metadata.generated_by != "Tjuae 资产构建器 v3.0.0"
        || normalize_repository_url(&index.metadata.repository) != HUB_REPOSITORY
    {
        return Err(MarketError::Invalid("metadata 来源不受信任".into()));
    }
    validate_commit_sha(&index.metadata.source_revision)?;

    let mut referenced_assets = BTreeSet::new();
    for (key, package) in &index.packages {
        validate_package_name(key)?;
        if key != &package.name {
            return Err(MarketError::Invalid(format!("包键 {key} 与 name 不一致")));
        }
        if package.review_status != MarketPackageReviewStatus::Approved {
            return Err(MarketError::Invalid(format!(
                "包 {} 尚未通过审核，不能进入可安装索引",
                package.name
            )));
        }
        validate_package(package, &index.metadata.source_revision)?;
        for asset_id in &package.asset_ids {
            if !referenced_assets.insert(asset_id.clone()) {
                return Err(MarketError::Invalid(format!("资产 {asset_id} 被多个包重复引用")));
            }
            let asset = index
                .assets
                .get(asset_id)
                .ok_or_else(|| MarketError::Invalid(format!("包 {} 引用了不存在的资产 {asset_id}", package.name)))?;
            if asset.package_name != package.name {
                return Err(MarketError::Invalid(format!("资产 {asset_id} 与包名交叉引用不一致")));
            }
        }
    }

    let mut runtime_identities = BTreeSet::new();
    for (key, asset) in &index.assets {
        if key != &asset.id {
            return Err(MarketError::Invalid(format!("资产键 {key} 与 id 不一致")));
        }
        validate_asset(asset, &index.metadata.source_revision)?;
        if !runtime_identities.insert((kind_segment(asset.kind), asset.runtime_id.as_str())) {
            return Err(MarketError::Invalid(format!(
                "资产 {} 的运行时身份 {} 与其他资产冲突",
                asset.id, asset.runtime_id
            )));
        }
        if !referenced_assets.contains(key) {
            return Err(MarketError::Invalid(format!("资产 {key} 未归属任何原子包")));
        }
    }
    validate_asset_dependency_graph(&index.assets)?;
    validate_package_dependency_graph(&index.packages)?;
    Ok(())
}

fn validate_asset_dependency_graph(assets: &BTreeMap<String, RawMarketAsset>) -> Result<(), MarketError> {
    for asset in assets.values() {
        if !asset.dependencies.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(MarketError::Invalid(format!(
                "资产 {} 的 dependencies 必须去重并按稳定 ID 排序",
                asset.id
            )));
        }
        for dependency_id in &asset.dependencies {
            if dependency_id == &asset.id {
                return Err(MarketError::Invalid(format!("资产 {} 不能依赖自身", asset.id)));
            }
            let dependency = assets
                .get(dependency_id)
                .ok_or_else(|| MarketError::Invalid(format!("资产 {} 依赖不存在的资产 {dependency_id}", asset.id)))?;
            if asset.trust == AssetTrust::Official && dependency.trust != AssetTrust::Official {
                return Err(MarketError::Invalid(format!("官方资产 {} 只能依赖官方资产", asset.id)));
            }
            if asset.kind == AssetKind::Assistant && dependency.kind != AssetKind::Skill {
                return Err(MarketError::Invalid(format!(
                    "助手资产 {} 的依赖必须是技能资产",
                    asset.id
                )));
            }
        }
    }

    fn visit<'a>(
        id: &'a str,
        assets: &'a BTreeMap<String, RawMarketAsset>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
    ) -> Result<(), MarketError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(MarketError::Invalid(format!("资产依赖图包含循环：{id}")));
        }
        let asset = assets
            .get(id)
            .ok_or_else(|| MarketError::Invalid(format!("依赖资产 {id} 不存在")))?;
        for dependency in &asset.dependencies {
            visit(dependency, assets, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in assets.keys() {
        visit(id, assets, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_package_dependency_graph(packages: &BTreeMap<String, MarketPackageDescriptor>) -> Result<(), MarketError> {
    for package in packages.values() {
        for (dependency_name, requirement) in &package.dependencies {
            validate_package_name(dependency_name)?;
            if dependency_name == &package.name {
                return Err(MarketError::Invalid(format!("包 {} 不能依赖自身", package.name)));
            }
            let dependency = packages
                .get(dependency_name)
                .ok_or_else(|| MarketError::Invalid(format!("包 {} 依赖不存在的包 {dependency_name}", package.name)))?;
            let requirement = VersionReq::parse(requirement).map_err(|_| {
                MarketError::Invalid(format!("包 {} 对 {dependency_name} 的版本范围无效", package.name))
            })?;
            let version = Version::parse(&dependency.version)
                .map_err(|_| MarketError::Invalid(format!("依赖包 {dependency_name} 的版本无效")))?;
            if !requirement.matches(&version) {
                return Err(MarketError::Invalid(format!(
                    "包 {} 的依赖 {dependency_name} 不满足版本范围",
                    package.name
                )));
            }
        }
    }
    fn visit(
        name: &str,
        packages: &BTreeMap<String, MarketPackageDescriptor>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), MarketError> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name.to_owned()) {
            return Err(MarketError::Invalid(format!("包依赖图包含循环：{name}")));
        }
        let package = packages
            .get(name)
            .ok_or_else(|| MarketError::Invalid(format!("依赖包 {name} 不存在")))?;
        for dependency in package.dependencies.keys() {
            visit(dependency, packages, visiting, visited)?;
        }
        visiting.remove(name);
        visited.insert(name.to_owned());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for name in packages.keys() {
        visit(name, packages, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_asset(asset: &RawMarketAsset, source_revision: &str) -> Result<(), MarketError> {
    let parts = asset.id.split('/').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[0] != asset.package_name
        || parts[1] != kind_segment(asset.kind)
        || !valid_local_asset_id(parts[2])
    {
        return Err(MarketError::Invalid(format!("资产 ID {} 无效", asset.id)));
    }
    validate_package_name(&asset.package_name)?;
    if asset.runtime_id.is_empty()
        || asset.runtime_id.len() > 128
        || !asset.runtime_id.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte.is_ascii_alphanumeric()
            } else {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            }
        })
    {
        return Err(MarketError::Invalid(format!("资产 {} 的 runtimeId 无效", asset.id)));
    }
    validate_text(&asset.display_name, 1, 128, "displayName", &asset.id)?;
    validate_text(&asset.description, 0, 4096, "description", &asset.id)?;
    validate_text(&asset.author, 1, 128, "author", &asset.id)?;
    validate_text(&asset.license, 1, 128, "license", &asset.id)?;
    Version::parse(&asset.version).map_err(|_| MarketError::Invalid(format!("资产 {} 的 version 无效", asset.id)))?;
    VersionReq::parse(&asset.compatibility.tjuae)
        .map_err(|_| MarketError::Invalid(format!("资产 {} 的 compatibility.tjuae 无效", asset.id)))?;
    validate_integrity(&asset.definition_digest)?;
    validate_commit_sha(&asset.source_revision)?;
    if asset.source_revision != source_revision {
        return Err(MarketError::Invalid(format!("资产 {} 的源码修订不一致", asset.id)));
    }
    let entry = normalize_relative_path(&asset.entry_file)
        .map_err(|_| MarketError::Invalid(format!("资产 {} 的 entryFile 不安全", asset.id)))?;
    match asset.kind {
        AssetKind::EngineAdapter if entry != "engine-adapter.json" => {
            return Err(MarketError::Invalid(format!(
                "资产 {} 的引擎适配器入口必须是 engine-adapter.json",
                asset.id
            )));
        }
        AssetKind::Mcp if entry != "mcp.json" => {
            return Err(MarketError::Invalid(format!(
                "资产 {} 的 MCP 入口必须是 mcp.json",
                asset.id
            )));
        }
        AssetKind::Assistant | AssetKind::EngineAdapter | AssetKind::Skill | AssetKind::Mcp => {}
    }
    if asset.files.is_empty() || asset.files.len() > 2_048 {
        return Err(MarketError::Invalid(format!("资产 {} 的 files 数量无效", asset.id)));
    }
    let mut seen_paths = BTreeSet::new();
    let mut has_entry = false;
    for file in &asset.files {
        let path = normalize_relative_path(&file.path)
            .map_err(|_| MarketError::Invalid(format!("资产 {} 包含不安全文件路径", asset.id)))?;
        if !seen_paths.insert(path.to_lowercase()) {
            return Err(MarketError::Invalid(format!("资产 {} 包含冲突文件路径", asset.id)));
        }
        has_entry |= path == entry;
        validate_integrity(&file.digest)?;
        if file.size > FILE_MAX_BYTES {
            return Err(MarketError::TooLarge {
                actual: file.size,
                limit: FILE_MAX_BYTES,
            });
        }
        validate_text(&file.media_type, 1, 128, "mediaType", &asset.id)?;
    }
    if !has_entry {
        return Err(MarketError::Invalid(format!(
            "资产 {} 的入口文件不在 files 中",
            asset.id
        )));
    }
    let mut tags = BTreeSet::new();
    for tag in &asset.tags {
        validate_text(tag, 1, 64, "tag", &asset.id)?;
        if !tags.insert(tag) {
            return Err(MarketError::Invalid(format!("资产 {} 包含重复标签", asset.id)));
        }
    }
    Ok(())
}

fn validate_package(package: &MarketPackageDescriptor, source_revision: &str) -> Result<(), MarketError> {
    if !package.atomic || package.asset_ids.is_empty() {
        return Err(MarketError::Invalid(format!("包 {} 必须是非空原子包", package.name)));
    }
    Version::parse(&package.version)
        .map_err(|_| MarketError::Invalid(format!("包 {} 的 version 无效", package.name)))?;
    let unique = package.asset_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != package.asset_ids.len() {
        return Err(MarketError::Invalid(format!("包 {} 包含重复资产 ID", package.name)));
    }
    if package.tarball != format!("{}.zip", package.name)
        || package.tarball.contains(['/', '\\', ':'])
        || package.tarball.contains("..")
    {
        return Err(MarketError::Invalid(format!("包 {} 的 tarball 无效", package.name)));
    }
    validate_integrity(&package.integrity)?;
    validate_integrity(&package.archive_integrity)?;
    if package.unpacked_size == 0 || package.unpacked_size > PACKAGE_MAX_UNPACKED_BYTES {
        return Err(MarketError::TooLarge {
            actual: package.unpacked_size,
            limit: PACKAGE_MAX_UNPACKED_BYTES,
        });
    }
    if normalize_repository_url(&package.repository) != HUB_REPOSITORY
        || package.source_path != format!("assets/{}", package.name)
        || package.manifest_path != format!("assets/{}/asset-package.json", package.name)
        || package.source_revision != source_revision
    {
        return Err(MarketError::Invalid(format!("包 {} 的来源信息无效", package.name)));
    }
    validate_commit_sha(&package.source_revision)
}

fn validate_text(value: &str, min_chars: usize, max_chars: usize, field: &str, id: &str) -> Result<(), MarketError> {
    let length = value.chars().count();
    if !(min_chars..=max_chars).contains(&length) {
        return Err(MarketError::Invalid(format!("资产 {id} 的 {field} 长度无效")));
    }
    Ok(())
}

fn validate_package_name(value: &str) -> Result<(), MarketError> {
    if !(12..=96).contains(&value.len()) || !value.starts_with("tjuaeasset-") {
        return Err(MarketError::Invalid(format!("包名 {value} 无效")));
    }
    let suffix = &value["tjuaeasset-".len()..];
    if suffix.is_empty()
        || suffix.starts_with('-')
        || suffix.ends_with('-')
        || suffix.contains("--")
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(MarketError::Invalid(format!("包名 {value} 无效")));
    }
    Ok(())
}

fn valid_local_asset_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 {
        return false;
    }
    let mut previous_separator = false;
    for byte in value.bytes() {
        let separator = matches!(byte, b'.' | b'_' | b':' | b'-');
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || separator) || separator && previous_separator {
            return false;
        }
        previous_separator = separator;
    }
    !previous_separator
}

fn kind_segment(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Assistant => "assistant",
        AssetKind::EngineAdapter => "engineAdapter",
        AssetKind::Skill => "skill",
        AssetKind::Mcp => "mcp",
    }
}

fn validate_integrity(value: &str) -> Result<(), MarketError> {
    let Some(hex) = value.strip_prefix("sha256-") else {
        return Err(MarketError::Invalid("摘要必须使用 sha256- 前缀".into()));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(MarketError::Invalid("摘要格式无效".into()));
    }
    Ok(())
}

fn validate_commit_sha(value: &str) -> Result<(), MarketError> {
    if matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(MarketError::InvalidCommit)
    }
}

fn normalize_repository_url(value: &str) -> &str {
    value.trim_end_matches('/').trim_end_matches(".git")
}

fn market_http_client() -> Result<reqwest::Client, MarketError> {
    tjuaeui_runtime::build_http_client(CONNECT_TIMEOUT, REQUEST_TIMEOUT).map_err(MarketError::Network)
}

async fn read_bounded_response(
    mut response: reqwest::Response,
    limit: usize,
    expected_url: &str,
    operation: &str,
) -> Result<Vec<u8>, MarketError> {
    if !response.status().is_success() {
        return Err(MarketError::Network(format!(
            "{operation}返回 HTTP {}",
            response.status()
        )));
    }
    validate_exact_url(response.url().as_str(), expected_url, operation)?;
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(MarketError::TooLarge {
            actual: response.content_length().unwrap_or_default(),
            limit: limit as u64,
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| MarketError::Network(error.to_string()))?
    {
        let next = bytes.len().saturating_add(chunk.len());
        if next > limit {
            return Err(MarketError::TooLarge {
                actual: next as u64,
                limit: limit as u64,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn parse_dist_ref(bytes: &[u8]) -> Result<String, MarketError> {
    #[derive(Deserialize)]
    struct RefObject {
        sha: String,
    }
    #[derive(Deserialize)]
    struct RefResponse {
        #[serde(rename = "ref")]
        reference: String,
        object: RefObject,
    }
    let value: RefResponse = serde_json::from_slice(bytes).map_err(|error| MarketError::Invalid(error.to_string()))?;
    if value.reference != "refs/heads/dist" {
        return Err(MarketError::Invalid("GitHub 返回了错误的 Hub 引用".into()));
    }
    validate_commit_sha(&value.object.sha)?;
    Ok(value.object.sha)
}

fn index_url(distribution_revision: &str) -> String {
    format!("{HUB_RAW_BASE}/{distribution_revision}/index.json")
}

fn offline_seed_url(source_revision: &str) -> String {
    format!("tjuae-bundled://hub/{source_revision}/seed-index.json")
}

fn package_url(distribution_revision: &str, tarball: &str) -> String {
    format!("{HUB_RAW_BASE}/{distribution_revision}/{tarball}")
}

fn validate_index_url(value: &str) -> Result<url::Url, MarketError> {
    let parsed = url::Url::parse(value).map_err(|_| MarketError::Network("Hub 索引 URL 无效".into()))?;
    let segments = parsed
        .path_segments()
        .map(|items| items.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 4 || segments[0] != "liangboqiang" || segments[1] != "TjuaeHub" || segments[3] != "index.json"
    {
        return Err(MarketError::Network("Hub 索引 URL 未固定到受信任仓库".into()));
    }
    validate_commit_sha(segments[2])?;
    validate_exact_url(value, &index_url(segments[2]), "读取 Hub 资产索引")
}

fn validate_package_url(value: &str) -> Result<url::Url, MarketError> {
    let parsed = url::Url::parse(value).map_err(|_| MarketError::Network("Hub 资产包 URL 无效".into()))?;
    let segments = parsed
        .path_segments()
        .map(|items| items.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() != 4
        || segments[0] != "liangboqiang"
        || segments[1] != "TjuaeHub"
        || !segments[3].starts_with("tjuaeasset-")
        || !segments[3].ends_with(".zip")
        || segments[3].contains("..")
    {
        return Err(MarketError::Network("Hub 资产包 URL 未固定到受信任仓库".into()));
    }
    validate_commit_sha(segments[2])?;
    validate_exact_url(value, &package_url(segments[2], segments[3]), "读取 Hub 原子资产包")
}

fn validate_exact_url(actual: &str, expected: &str, operation: &str) -> Result<url::Url, MarketError> {
    let exact_authority = actual
        .strip_prefix("https://")
        .and_then(|remainder| remainder.split('/').next());
    let actual = url::Url::parse(actual).map_err(|_| MarketError::Network(format!("{operation}的 URL 无效")))?;
    let expected =
        url::Url::parse(expected).map_err(|_| MarketError::Network(format!("{operation}的预期 URL 无效")))?;
    if exact_authority != expected.host_str()
        || actual.scheme() != "https"
        || actual.host_str() != expected.host_str()
        || actual.port().is_some()
        || !actual.username().is_empty()
        || actual.password().is_some()
        || actual.query().is_some()
        || actual.fragment().is_some()
        || actual.path() != expected.path()
        || actual.as_str() != expected.as_str()
    {
        return Err(MarketError::Network(format!("{operation}离开了固定受信任地址")));
    }
    Ok(actual)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), MarketError> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| MarketError::Invalid("市场缓存文件名无效".into()))?;
    let temp = path.with_file_name(format!(".{file_name}.tmp-{}-{}", std::process::id(), now_ms()));
    let backup = backup_path(path);
    let mut output = std::fs::File::create(&temp)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);

    if path.exists() {
        if backup.exists() {
            std::fs::remove_file(&backup)?;
        }
        std::fs::rename(path, &backup)?;
        match std::fs::rename(&temp, path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&backup);
                Ok(())
            }
            Err(error) => {
                let _ = std::fs::rename(&backup, path);
                let _ = std::fs::remove_file(&temp);
                Err(error.into())
            }
        }
    } else {
        std::fs::rename(&temp, path)?;
        Ok(())
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(INDEX_CACHE_FILE);
    path.with_file_name(format!(".{file_name}.backup"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_support::RecordingRuntimeProjector;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tjuaeui_db::{IAssetRepository, SqliteAssetRepository, init_database_memory};

    const CROSS_REPOSITORY_INDEX_FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/hub-index.v2.cross-repository.json");

    struct MockRemote {
        commit: String,
        bytes: Vec<u8>,
        ref_calls: AtomicUsize,
        index_calls: AtomicUsize,
    }

    #[async_trait]
    impl MarketIndexRemote for MockRemote {
        async fn resolve_dist_head(&self) -> Result<String, MarketError> {
            self.ref_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.commit.clone())
        }

        async fn fetch_index(&self, source_url: &str) -> Result<Vec<u8>, MarketError> {
            assert_eq!(source_url, index_url(&self.commit));
            self.index_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.bytes.clone())
        }

        async fn fetch_package(&self, _source_url: &str) -> Result<Vec<u8>, MarketError> {
            Err(MarketError::Unavailable)
        }
    }

    struct FailingRemote;

    #[async_trait]
    impl MarketIndexRemote for FailingRemote {
        async fn resolve_dist_head(&self) -> Result<String, MarketError> {
            Err(MarketError::Network("测试网络不可用".into()))
        }

        async fn fetch_index(&self, _source_url: &str) -> Result<Vec<u8>, MarketError> {
            Err(MarketError::Network("测试网络不可用".into()))
        }

        async fn fetch_package(&self, _source_url: &str) -> Result<Vec<u8>, MarketError> {
            Err(MarketError::Network("测试网络不可用".into()))
        }
    }

    struct SwitchableRemoteState {
        online: bool,
        commit: String,
        index: Vec<u8>,
        package: Vec<u8>,
    }

    struct SwitchableRemote {
        state: StdMutex<SwitchableRemoteState>,
    }

    impl SwitchableRemote {
        fn replace(&self, online: bool, commit: String, index: Vec<u8>, package: Vec<u8>) {
            *self.state.lock().unwrap() = SwitchableRemoteState {
                online,
                commit,
                index,
                package,
            };
        }

        fn set_online(&self, online: bool) {
            self.state.lock().unwrap().online = online;
        }
    }

    #[async_trait]
    impl MarketIndexRemote for SwitchableRemote {
        async fn resolve_dist_head(&self) -> Result<String, MarketError> {
            let state = self.state.lock().unwrap();
            if state.online {
                Ok(state.commit.clone())
            } else {
                Err(MarketError::Network("测试网络不可用".into()))
            }
        }

        async fn fetch_index(&self, source_url: &str) -> Result<Vec<u8>, MarketError> {
            let state = self.state.lock().unwrap();
            if !state.online {
                return Err(MarketError::Network("测试网络不可用".into()));
            }
            assert_eq!(source_url, index_url(&state.commit));
            Ok(state.index.clone())
        }

        async fn fetch_package(&self, source_url: &str) -> Result<Vec<u8>, MarketError> {
            let state = self.state.lock().unwrap();
            if !state.online {
                return Err(MarketError::Network("测试网络不可用".into()));
            }
            assert_eq!(source_url, package_url(&state.commit, "tjuaeasset-state-demo.zip"));
            Ok(state.package.clone())
        }
    }

    fn switchable_remote_fixture(source_revision: &str, content: &[u8]) -> (Vec<u8>, Vec<u8>, String) {
        let remote_asset_id = "tjuaeasset-state-demo/skill/demo".to_owned();
        let package_name = "tjuaeasset-state-demo".to_owned();
        let path = "skills/demo/SKILL.md";
        let definition = vec![AssetDefinitionFile {
            path: path.into(),
            content: content.to_vec(),
        }];
        let (_, scanned) = prepare_definition(definition).unwrap();
        let package_bytes = test_zip(&[(path, content)]);
        let mut content_hash = Sha256::new();
        content_hash.update(path.as_bytes());
        content_hash.update(content);
        let package = MarketPackageDescriptor {
            name: package_name.clone(),
            version: "1.0.0".into(),
            review_status: MarketPackageReviewStatus::Approved,
            atomic: true,
            asset_ids: vec![remote_asset_id.clone()],
            dependencies: BTreeMap::new(),
            tarball: format!("{package_name}.zip"),
            integrity: format!("sha256-{}", hex::encode(content_hash.finalize())),
            archive_integrity: digest_bytes(&package_bytes),
            unpacked_size: content.len() as u64,
            repository: HUB_REPOSITORY.into(),
            source_path: format!("assets/{package_name}"),
            manifest_path: format!("assets/{package_name}/asset-package.json"),
            source_revision: source_revision.into(),
        };
        let asset = RawMarketAsset {
            id: remote_asset_id.clone(),
            kind: AssetKind::Skill,
            runtime_id: "state-demo".into(),
            dependencies: Vec::new(),
            display_name: "状态演示".into(),
            description: "状态演示".into(),
            version: "1.0.0".into(),
            definition_digest: scanned.digest,
            entry_file: path.into(),
            package_name: package_name.clone(),
            author: "Tjuae".into(),
            license: "Apache-2.0".into(),
            trust: AssetTrust::Official,
            status: MarketAssetStatus::Active,
            compatibility: RawCompatibility { tjuae: "^1.0.0".into() },
            source_revision: source_revision.into(),
            files: vec![MarketAssetFileResponse {
                path: path.into(),
                digest: digest_bytes(content),
                size: content.len() as u64,
                media_type: "text/markdown".into(),
            }],
            tags: Vec::new(),
        };
        let index = RawMarketIndex {
            schema: MARKET_INDEX_SCHEMA_URL.into(),
            schema_version: 2,
            generated_at: "2026-08-02T00:00:00Z".into(),
            assets: BTreeMap::from([(remote_asset_id.clone(), asset)]),
            packages: BTreeMap::from([(package_name, package)]),
            metadata: RawMarketMetadata {
                total_packages: 1,
                total_assets: 1,
                generated_by: "Tjuae 资产构建器 v3.0.0".into(),
                repository: HUB_REPOSITORY.into(),
                source_revision: source_revision.into(),
            },
        };
        (serde_json::to_vec(&index).unwrap(), package_bytes, remote_asset_id)
    }

    fn remote_index_without_target(source_revision: &str) -> Vec<u8> {
        let (bytes, _, target_id) = switchable_remote_fixture(source_revision, b"# unrelated\n");
        let mut index: RawMarketIndex = serde_json::from_slice(&bytes).unwrap();
        let mut asset = index.assets.remove(&target_id).unwrap();
        let mut package = index.packages.remove("tjuaeasset-state-demo").unwrap();
        asset.id = "tjuaeasset-other/skill/other".into();
        asset.runtime_id = "other".into();
        asset.package_name = "tjuaeasset-other".into();
        package.name = "tjuaeasset-other".into();
        package.asset_ids = vec![asset.id.clone()];
        package.tarball = "tjuaeasset-other.zip".into();
        package.source_path = "assets/tjuaeasset-other".into();
        package.manifest_path = "assets/tjuaeasset-other/asset-package.json".into();
        index.assets.insert(asset.id.clone(), asset);
        index.packages.insert(package.name.clone(), package);
        serde_json::to_vec(&index).unwrap()
    }

    #[test]
    fn url_and_commit_validation_are_fail_closed() {
        let sha = "a".repeat(40);
        let expected = index_url(&sha);
        assert!(validate_index_url(&expected).is_ok());
        assert!(validate_index_url(&expected.replace("index.json", "../index.json")).is_err());
        assert!(
            validate_index_url(&expected.replace("raw.githubusercontent.com", "raw.githubusercontent.com:443"))
                .is_err()
        );
        assert!(validate_commit_sha("main").is_err());
        assert!(validate_commit_sha(&"A".repeat(40)).is_err());
    }

    #[test]
    fn cross_repository_fixture_preserves_hub_v2_asset_and_package_contract() {
        let index: RawMarketIndex = serde_json::from_slice(CROSS_REPOSITORY_INDEX_FIXTURE).unwrap();
        validate_index(&index).unwrap();

        let engine_id = "tjuaeasset-contract-engine/engineAdapter/contract-acp";
        let skill_id = "tjuaeasset-contract-skill/skill/contract-helper";
        let engine = index.assets.get(engine_id).unwrap();
        assert_eq!(engine.kind, AssetKind::EngineAdapter);
        assert_eq!(engine.status, MarketAssetStatus::Active);
        assert_eq!(engine.runtime_id, "contract-acp");
        assert_eq!(engine.entry_file, "engine-adapter.json");
        assert_eq!(engine.dependencies, [skill_id]);
        assert_eq!(engine.definition_digest, format!("sha256-{}", "a".repeat(64)));
        assert_eq!(engine.source_revision, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(
            index.assets.get(skill_id).unwrap().status,
            MarketAssetStatus::Deprecated
        );
        assert_eq!(
            engine
                .files
                .iter()
                .map(|file| {
                    (
                        file.path.as_str(),
                        file.digest.as_str(),
                        file.size,
                        file.media_type.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    "asset-package.json",
                    "sha256-dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                    1024,
                    "application/json",
                ),
                (
                    "engine-adapter.json",
                    "sha256-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                    384,
                    "application/json",
                ),
            ]
        );

        let package = index.packages.get("tjuaeasset-contract-engine").unwrap();
        assert_eq!(package.review_status, MarketPackageReviewStatus::Approved);
        assert_eq!(package.asset_ids, [engine_id]);
        assert_eq!(
            package.dependencies,
            BTreeMap::from([("tjuaeasset-contract-skill".to_owned(), "^1.4.0".to_owned())])
        );
        assert_eq!(
            package.integrity,
            "sha256-1111111111111111111111111111111111111111111111111111111111111111"
        );
        assert_eq!(
            package.archive_integrity,
            "sha256-3333333333333333333333333333333333333333333333333333333333333333"
        );
        assert_eq!(package.source_revision, index.metadata.source_revision);
    }

    #[test]
    fn hub_v2_package_dependencies_are_required_in_core_parser() {
        let mut fixture: serde_json::Value = serde_json::from_slice(CROSS_REPOSITORY_INDEX_FIXTURE).unwrap();
        fixture["packages"]["tjuaeasset-contract-engine"]
            .as_object_mut()
            .unwrap()
            .remove("dependencies");

        let error = serde_json::from_value::<RawMarketIndex>(fixture).unwrap_err();
        assert!(error.to_string().contains("dependencies"));
    }

    #[test]
    fn hub_v2_rejects_unknown_or_unapproved_lifecycle_values() {
        let mut unknown_status: serde_json::Value = serde_json::from_slice(CROSS_REPOSITORY_INDEX_FIXTURE).unwrap();
        unknown_status["assets"]["tjuaeasset-contract-engine/engineAdapter/contract-acp"]["status"] =
            serde_json::json!("disabled");
        assert!(serde_json::from_value::<RawMarketIndex>(unknown_status).is_err());

        let mut unapproved: RawMarketIndex = serde_json::from_slice(CROSS_REPOSITORY_INDEX_FIXTURE).unwrap();
        unapproved
            .packages
            .get_mut("tjuaeasset-contract-engine")
            .unwrap()
            .review_status = MarketPackageReviewStatus::UnderReview;
        assert!(validate_index(&unapproved).is_err());
    }

    #[test]
    fn revoked_asset_is_valid_but_cannot_be_installed_or_synced() {
        let mut index: RawMarketIndex = serde_json::from_slice(CROSS_REPOSITORY_INDEX_FIXTURE).unwrap();
        let asset = index
            .assets
            .get_mut("tjuaeasset-contract-skill/skill/contract-helper")
            .unwrap();
        asset.status = MarketAssetStatus::Revoked;
        validate_index(&index).unwrap();

        let manager = MarketIndexManager::with_remote(PathBuf::from("unused"), "1.0.0", Arc::new(FailingRemote));
        let asset = index
            .assets
            .get("tjuaeasset-contract-skill/skill/contract-helper")
            .unwrap();
        assert!(manager.require_compatible(asset).is_err());

        let mut actions = vec![
            AssetAction::View,
            AssetAction::Edit,
            AssetAction::Install,
            AssetAction::Sync,
            AssetAction::Publish,
            AssetAction::TryRun,
            AssetAction::Uninstall,
            AssetAction::Detach,
        ];
        retain_revoked_actions(&mut actions);
        assert_eq!(
            actions,
            vec![AssetAction::View, AssetAction::Uninstall, AssetAction::Detach]
        );
    }

    #[tokio::test]
    async fn first_load_fetches_a_pinned_index_then_uses_cache() {
        let temp = tempfile::tempdir().unwrap();
        let commit = "b".repeat(40);
        let source_revision = "a".repeat(40);
        let remote = Arc::new(MockRemote {
            commit: commit.clone(),
            bytes: test_index_bytes(&source_revision),
            ref_calls: AtomicUsize::new(0),
            index_calls: AtomicUsize::new(0),
        });
        let manager = MarketIndexManager::with_remote(temp.path().join("cache"), "1.2.0", remote.clone());
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"))
            .with_runtime_projector(Arc::new(RecordingRuntimeProjector::default()));

        let first = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap();
        assert_eq!(first.assets.len(), 1);
        assert!(first.assets[0].asset.compatibility.compatible);
        assert_eq!(first.cache.distribution_revision.as_deref(), Some(commit.as_str()));
        let second = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap();
        assert_eq!(second.assets.len(), 1);
        assert_eq!(remote.ref_calls.load(Ordering::SeqCst), 1);
        assert_eq!(remote.index_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn explicit_asset_protocol_version_controls_compatibility_and_actions() {
        let temp = tempfile::tempdir().unwrap();
        let commit = "b".repeat(40);
        let source_revision = "a".repeat(40);
        let remote = Arc::new(MockRemote {
            commit,
            bytes: test_index_bytes(&source_revision),
            ref_calls: AtomicUsize::new(0),
            index_calls: AtomicUsize::new(0),
        });
        let manager = MarketIndexManager::with_remote(temp.path().join("cache"), "0.2.0", remote);
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"));

        let response = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap();
        assert!(!response.assets[0].asset.compatibility.compatible);
        assert_eq!(response.assets[0].sync_state, None);
        assert_eq!(response.assets[0].allowed_actions, vec![AssetAction::View]);
        assert_eq!(TJUAE_ASSET_PROTOCOL_VERSION, "1.0.0");
    }

    #[test]
    fn combined_asset_and_package_dependencies_are_topologically_closed() {
        let revision = "a".repeat(40);
        let mut index: RawMarketIndex = serde_json::from_slice(&test_index_bytes(&revision)).unwrap();
        let dependency_id = "tjuaeasset-demo/skill/demo".to_owned();
        let dependency_package = "tjuaeasset-demo".to_owned();
        let mut target = index.assets.get(&dependency_id).unwrap().clone();
        target.id = "tjuaeasset-target/skill/target".into();
        target.runtime_id = "target".into();
        target.package_name = "tjuaeasset-target".into();
        target.dependencies = vec![dependency_id.clone()];
        let mut target_package = index.packages.get(&dependency_package).unwrap().clone();
        target_package.name = "tjuaeasset-target".into();
        target_package.asset_ids = vec![target.id.clone()];
        target_package.dependencies.clear();
        index.assets.insert(target.id.clone(), target);
        index.packages.insert(target_package.name.clone(), target_package);
        let target_package = index.packages.get("tjuaeasset-target").unwrap();

        let ordered = ordered_dependency_packages(&index, target_package)
            .unwrap()
            .into_iter()
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ordered, vec!["tjuaeasset-demo", "tjuaeasset-target"]);
    }

    #[tokio::test]
    async fn runtime_kinds_allow_definition_install_before_runtime_is_configured() {
        let temp = tempfile::tempdir().unwrap();
        let revision = "a".repeat(40);
        let mut index: RawMarketIndex = serde_json::from_slice(&test_index_bytes(&revision)).unwrap();
        let old_id = "tjuaeasset-demo/skill/demo";
        let mut engine = index.assets.remove(old_id).unwrap();
        engine.id = "tjuaeasset-demo/engineAdapter/demo".into();
        engine.kind = AssetKind::EngineAdapter;
        engine.runtime_id = "demo".into();
        engine.entry_file = "engine-adapter.json".into();
        engine.files[0].path = "engine-adapter.json".into();
        index.packages.get_mut("tjuaeasset-demo").unwrap().asset_ids = vec![engine.id.clone()];
        index.assets.insert(engine.id.clone(), engine);
        let manager = MarketIndexManager::with_remote(
            temp.path().join("cache"),
            TJUAE_ASSET_PROTOCOL_VERSION,
            Arc::new(FailingRemote),
        );
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"))
            .with_runtime_projector(Arc::new(RecordingRuntimeProjector::default()));

        let response = manager
            .response_from_index(
                StoredMarketCache {
                    index,
                    distribution_revision: Some(revision),
                    cached_at: now_ms(),
                    source_url: "test://market".into(),
                    origin: MarketCacheOrigin::Remote,
                },
                "system_default_user",
                &catalog,
                &ListMarketAssetsQuery::default(),
            )
            .await
            .unwrap();

        let asset = &response.assets[0];
        assert!(asset.asset.compatibility.compatible);
        assert_eq!(asset.asset.compatibility.reason_code, None);
        assert_eq!(asset.sync_state, None);
        assert_eq!(asset.allowed_actions, vec![AssetAction::View, AssetAction::Install]);
    }

    #[tokio::test]
    async fn development_seed_is_deterministic_and_never_reads_remote_dist() {
        let temp = tempfile::tempdir().unwrap();
        let resource_dir = temp.path().join("resources");
        let (source, _) = write_offline_fixture(&resource_dir, None);
        let remote = Arc::new(MockRemote {
            commit: "9".repeat(40),
            bytes: b"{invalid".to_vec(),
            ref_calls: AtomicUsize::new(0),
            index_calls: AtomicUsize::new(0),
        });
        let manager = MarketIndexManager::with_remote_and_seed(
            temp.path().join("cache"),
            TJUAE_ASSET_PROTOCOL_VERSION,
            remote.clone(),
            OfflineSeedSetting::Source(source),
        );
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"));

        let response = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap();

        assert_eq!(response.assets.len(), 4);
        assert!(response.cache.source_url.starts_with("tjuae-bundled://hub/"));
        assert_eq!(remote.ref_calls.load(Ordering::SeqCst), 0);
        assert_eq!(remote.index_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn corrupt_cache_and_network_failure_fall_back_to_seed_and_install_offline() {
        let temp = tempfile::tempdir().unwrap();
        let resource_dir = temp.path().join("resources");
        let (mut source, remote_asset_id) = write_offline_fixture(&resource_dir, None);
        pin_offline_fixture(&mut source);
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join(INDEX_CACHE_FILE), b"{corrupt").unwrap();
        let manager = MarketIndexManager::with_remote_and_seed(
            cache_dir.clone(),
            TJUAE_ASSET_PROTOCOL_VERSION,
            Arc::new(FailingRemote),
            OfflineSeedSetting::Source(source),
        );
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let projector = RecordingRuntimeProjector::default();
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"))
            .with_runtime_projector(Arc::new(projector.clone()));

        let response = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap();
        assert_eq!(response.assets.len(), 4);
        let remote_skill = response
            .assets
            .iter()
            .find(|asset| asset.asset.id == remote_asset_id)
            .unwrap();
        assert_eq!(remote_skill.sync_state, None);
        assert_eq!(
            remote_skill.allowed_actions,
            vec![AssetAction::View, AssetAction::Install]
        );
        assert!(remote_skill.asset.compatibility.compatible);
        assert!(response.cache.source_url.starts_with("tjuae-bundled://hub/"));
        assert_eq!(
            std::fs::read_dir(cache_dir.join("packages")).unwrap().count(),
            4,
            "导入离线种子时应预热内容寻址包缓存"
        );

        let operation = manager
            .install_asset("system_default_user", &catalog, &remote_asset_id, "offline-install")
            .await
            .unwrap();
        assert_eq!(operation.state, tjuaeui_api_types::AssetOperationState::Succeeded);

        let offline = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap();
        let installed_skill = offline
            .assets
            .iter()
            .find(|asset| asset.asset.id == remote_asset_id)
            .unwrap();
        assert_eq!(
            installed_skill.sync_state,
            Some(AssetSyncState::RemoteUnknown),
            "已验证的离线内容可以运行，但不能伪装成已确认的远程同步状态"
        );
        let local_asset_id = local_asset_id(&remote_asset_id);
        assert_eq!(
            catalog
                .read_file(
                    "system_default_user",
                    &local_asset_id,
                    "skills/demo/SKILL.md",
                    tjuaeui_api_types::AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "# Demo\n"
        );
        assert_eq!(
            projector.applied.load(Ordering::SeqCst),
            0,
            "安装与离线读取都不能在用户显式启用前创建运行投影"
        );
    }

    #[tokio::test]
    async fn current_hub_observation_drives_synced_unknown_updated_and_removed_states() {
        let temp = tempfile::tempdir().unwrap();
        let source_revision_a = "a".repeat(40);
        let distribution_revision_a = "d".repeat(40);
        let (index_a, package_a, remote_asset_id) = switchable_remote_fixture(&source_revision_a, b"# remote v1\n");
        let remote = Arc::new(SwitchableRemote {
            state: StdMutex::new(SwitchableRemoteState {
                online: true,
                commit: distribution_revision_a,
                index: index_a,
                package: package_a,
            }),
        });
        let manager =
            MarketIndexManager::with_remote(temp.path().join("cache"), TJUAE_ASSET_PROTOCOL_VERSION, remote.clone());
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let projector = RecordingRuntimeProjector::default();
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"))
            .with_runtime_projector(Arc::new(projector.clone()));

        manager.refresh(None).await.unwrap();
        manager
            .install_asset("system_default_user", &catalog, &remote_asset_id, "install-state-demo")
            .await
            .unwrap();
        let installed = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap();
        assert_eq!(installed.assets[0].presence_state, MarketPresenceState::Installed);
        assert_eq!(installed.assets[0].sync_state, Some(AssetSyncState::Synced));

        remote.set_online(false);
        assert!(matches!(manager.refresh(None).await, Err(MarketError::Network(_))));
        let offline = manager
            .list_local_assets("system_default_user", &catalog, None, None)
            .await
            .unwrap();
        assert_eq!(offline[0].sync_state, Some(AssetSyncState::RemoteUnknown));

        let source_revision_b = "b".repeat(40);
        let distribution_revision_b = "e".repeat(40);
        let (index_b, package_b, _) = switchable_remote_fixture(&source_revision_b, b"# remote v2\n");
        remote.replace(true, distribution_revision_b, index_b, package_b);
        manager.refresh(None).await.unwrap();
        let updated = manager
            .list_local_assets("system_default_user", &catalog, None, None)
            .await
            .unwrap();
        assert_eq!(updated[0].sync_state, Some(AssetSyncState::RemoteUpdated));
        let local_asset_id = local_asset_id(&remote_asset_id);
        let remote_input = manager
            .current_remote_input("system_default_user", &catalog, &local_asset_id)
            .await
            .expect("distribution revision and source revision are distinct provenance dimensions");
        assert_eq!(remote_input.source_revision, source_revision_b);
        let diff = manager
            .diff_local_asset("system_default_user", &catalog, &local_asset_id)
            .await
            .expect("B/L/R diff must use the source revision carried by the dist index");
        assert_eq!(diff.sync_state, AssetSyncState::RemoteUpdated);

        let source_revision_c = "c".repeat(40);
        let distribution_revision_c = "f".repeat(40);
        remote.replace(
            true,
            distribution_revision_c,
            remote_index_without_target(&source_revision_c),
            Vec::new(),
        );
        manager.refresh(None).await.unwrap();
        let removed = manager
            .list_local_assets("system_default_user", &catalog, None, None)
            .await
            .unwrap();
        assert_eq!(removed[0].sync_state, Some(AssetSyncState::UpstreamRemoved));
        assert_eq!(
            catalog
                .read_file(
                    "system_default_user",
                    &local_asset_id,
                    "skills/demo/SKILL.md",
                    tjuaeui_api_types::AssetContentSource::Local,
                )
                .await
                .unwrap()
                .content,
            "# remote v1\n"
        );
        assert_eq!(projector.applied.load(Ordering::SeqCst), 0);
        assert_eq!(projector.rolled_back.load(Ordering::SeqCst), 0);
        assert_eq!(projector.finalized.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn tampered_seed_bundle_is_rejected_even_when_network_is_down() {
        let temp = tempfile::tempdir().unwrap();
        let resource_dir = temp.path().join("resources");
        let (source, _) = write_offline_fixture(&resource_dir, None);
        let seed: OfflineSeedManifest =
            serde_json::from_slice(&std::fs::read(resource_dir.join("seed-manifest.json")).unwrap()).unwrap();
        std::fs::write(resource_dir.join(seed.bundle.file_name), b"tampered").unwrap();
        let manager = MarketIndexManager::with_remote_and_seed(
            temp.path().join("cache"),
            TJUAE_ASSET_PROTOCOL_VERSION,
            Arc::new(FailingRemote),
            OfflineSeedSetting::Source(source),
        );
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"));

        let error = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap_err();
        assert!(matches!(error, MarketError::Invalid(_)));
        assert!(error.to_string().contains("大小") || error.to_string().contains("摘要"));
    }

    #[tokio::test]
    async fn seed_bundle_path_traversal_is_rejected_before_cache_write() {
        let temp = tempfile::tempdir().unwrap();
        let resource_dir = temp.path().join("resources");
        let (source, _) = write_offline_fixture(&resource_dir, Some("../outside.zip"));
        let cache_dir = temp.path().join("cache");
        let manager = MarketIndexManager::with_remote_and_seed(
            cache_dir.clone(),
            TJUAE_ASSET_PROTOCOL_VERSION,
            Arc::new(FailingRemote),
            OfflineSeedSetting::Source(source),
        );
        let database = init_database_memory().await.unwrap();
        let repo: Arc<dyn IAssetRepository> = Arc::new(SqliteAssetRepository::new(database.pool().clone()));
        let catalog = AssetCatalogService::new(repo, temp.path().join("data"));

        let error = manager
            .load_index("system_default_user", &catalog, &ListMarketAssetsQuery::default())
            .await
            .unwrap_err();
        assert!(matches!(error, MarketError::Invalid(_)));
        assert!(error.to_string().contains("ZIP 路径"));
        assert!(!cache_dir.join(INDEX_CACHE_FILE).exists());
        assert!(!temp.path().join("outside.zip").exists());
    }

    #[test]
    fn index_rejects_package_asset_cross_reference_mismatch() {
        let source_revision = "a".repeat(40);
        let mut index: RawMarketIndex = serde_json::from_slice(&test_index_bytes(&source_revision)).unwrap();
        index.assets.values_mut().next().unwrap().package_name = "tjuaeasset-other".into();
        assert!(validate_index(&index).is_err());
    }

    #[test]
    fn package_verifier_checks_archive_content_and_definition_digests() {
        let path = "skills/demo/SKILL.md";
        let content = b"# Demo\n".to_vec();
        let definition = vec![AssetDefinitionFile {
            path: path.into(),
            content: content.clone(),
        }];
        let (_, scanned) = prepare_definition(definition.clone()).unwrap();
        let archive = test_zip(&[(path, content.as_slice())]);
        let mut content_hash = Sha256::new();
        content_hash.update(path.as_bytes());
        content_hash.update(&content);
        let package = MarketPackageDescriptor {
            name: "tjuaeasset-demo".into(),
            version: "1.0.0".into(),
            review_status: MarketPackageReviewStatus::Approved,
            atomic: true,
            asset_ids: vec!["tjuaeasset-demo/skill/demo".into()],
            dependencies: BTreeMap::new(),
            tarball: "tjuaeasset-demo.zip".into(),
            integrity: format!("sha256-{}", hex::encode(content_hash.finalize())),
            archive_integrity: digest_bytes(&archive),
            unpacked_size: content.len() as u64,
            repository: HUB_REPOSITORY.into(),
            source_path: "assets/tjuaeasset-demo".into(),
            manifest_path: "assets/tjuaeasset-demo/asset-package.json".into(),
            source_revision: "a".repeat(40),
        };
        let asset = RawMarketAsset {
            id: "tjuaeasset-demo/skill/demo".into(),
            kind: AssetKind::Skill,
            runtime_id: "demo".into(),
            dependencies: Vec::new(),
            display_name: "演示技能".into(),
            description: "演示".into(),
            version: "1.0.0".into(),
            definition_digest: scanned.digest,
            entry_file: path.into(),
            package_name: package.name.clone(),
            author: "Tjuae".into(),
            license: "Apache-2.0".into(),
            trust: AssetTrust::Official,
            status: MarketAssetStatus::Active,
            compatibility: RawCompatibility { tjuae: "^1.0.0".into() },
            source_revision: package.source_revision.clone(),
            files: vec![MarketAssetFileResponse {
                path: path.into(),
                digest: digest_bytes(&content),
                size: content.len() as u64,
                media_type: "text/markdown".into(),
            }],
            tags: Vec::new(),
        };

        assert_eq!(verify_package_archive(&archive, &package, &asset).unwrap(), definition);

        let unsafe_archives = [
            test_zip(&[("../outside.txt", &b"bad"[..])]),
            test_zip(&[("/absolute.txt", &b"bad"[..])]),
            test_zip(&[("C:/absolute.txt", &b"bad"[..])]),
            test_zip(&[("nested\\escape.txt", &b"bad"[..])]),
            test_zip(&[("CON.txt", &b"bad"[..])]),
            test_zip(&[("file.txt:secret", &b"bad"[..])]),
            test_zip(&[("Same.txt", &b"a"[..]), ("same.txt", &b"b"[..])]),
            test_zip_symlink("skills/demo/link", "../outside.txt"),
        ];
        for unsafe_archive in unsafe_archives {
            let mut unsafe_package = package.clone();
            unsafe_package.archive_integrity = digest_bytes(&unsafe_archive);
            assert!(matches!(
                verify_package_archive(&unsafe_archive, &unsafe_package, &asset),
                Err(MarketError::Invalid(_))
            ));
        }

        let too_many_entries = test_zip_with_empty_files(PACKAGE_MAX_ENTRIES + 1);
        let mut too_many_package = package.clone();
        too_many_package.archive_integrity = digest_bytes(&too_many_entries);
        assert!(matches!(
            verify_package_archive(&too_many_entries, &too_many_package, &asset),
            Err(MarketError::Invalid(message)) if message.contains("ZIP 条目过多")
        ));

        let oversized_file = test_zip_with_repeated_bytes(&[("oversized.bin", FILE_MAX_BYTES + 1)]);
        let mut oversized_file_package = package.clone();
        oversized_file_package.archive_integrity = digest_bytes(&oversized_file);
        assert!(matches!(
            verify_package_archive(&oversized_file, &oversized_file_package, &asset),
            Err(MarketError::TooLarge {
                actual,
                limit: FILE_MAX_BYTES
            }) if actual == FILE_MAX_BYTES + 1
        ));

        let half_plus_one = PACKAGE_MAX_UNPACKED_BYTES / 2 + 1;
        let oversized_package =
            test_zip_with_repeated_bytes(&[("first.bin", half_plus_one), ("second.bin", half_plus_one)]);
        let mut oversized_package_descriptor = package;
        oversized_package_descriptor.archive_integrity = digest_bytes(&oversized_package);
        assert!(matches!(
            verify_package_archive(&oversized_package, &oversized_package_descriptor, &asset),
            Err(MarketError::TooLarge {
                actual,
                limit: PACKAGE_MAX_UNPACKED_BYTES
            }) if actual == half_plus_one * 2
        ));
    }

    fn test_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o100644);
        for (path, content) in files {
            writer.start_file(*path, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn test_zip_symlink(path: &str, target: &str) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.add_symlink(path, target, options).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn test_zip_with_empty_files(count: usize) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o100644);
        for index in 0..count {
            writer.start_file(format!("files/{index:04}.txt"), options).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn test_zip_with_repeated_bytes(files: &[(&str, u64)]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o100644);
        let chunk = [0_u8; 64 * 1024];
        for (path, size) in files {
            writer.start_file(*path, options).unwrap();
            let mut remaining = *size;
            while remaining > 0 {
                let bytes_to_write = remaining.min(chunk.len() as u64) as usize;
                writer.write_all(&chunk[..bytes_to_write]).unwrap();
                remaining -= bytes_to_write as u64;
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn write_offline_fixture(directory: &Path, package_entry_override: Option<&str>) -> (OfflineSeedSource, String) {
        std::fs::create_dir_all(directory).unwrap();
        let source_revision = "a".repeat(40);
        let generated_at = "2026-08-01T00:00:00.000Z";
        let fixtures = vec![
            (
                AssetKind::Assistant,
                "tjuaeasset-assistant-demo",
                "assistant-demo",
                "assistant.json",
                vec![
                    (
                        "assistant.json".to_owned(),
                        r#"{"$schema":"https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/assistant-definition.v1.schema.json","schemaVersion":1,"kind":"assistant","runtimeId":"assistant-demo","name":"离线助手","nameI18n":{"zh-CN":"离线助手"},"description":"离线助手夹具","descriptionI18n":{"zh-CN":"离线助手夹具"},"rules":{"zh-CN":"rules/zh-CN.md"},"recommendedPrompts":[],"recommendedPromptsI18n":{},"skillDependencies":[],"avatar":{"type":"emoji","value":"T"}}"#.as_bytes().to_vec(),
                    ),
                    ("rules/zh-CN.md".to_owned(), b"# Rules\n".to_vec()),
                ],
            ),
            (
                AssetKind::EngineAdapter,
                "tjuaeasset-engine-demo",
                "contract-acp",
                "engine-adapter.json",
                vec![(
                    "engine-adapter.json".to_owned(),
                    include_bytes!("../tests/fixtures/engine-adapter-definition.v1.complete.json").to_vec(),
                )],
            ),
            (
                AssetKind::Mcp,
                "tjuaeasset-mcp-demo",
                "contract-mcp",
                "mcp.json",
                vec![(
                    "mcp.json".to_owned(),
                    include_bytes!("../tests/fixtures/mcp-definition.v1.complete.json").to_vec(),
                )],
            ),
            (
                AssetKind::Skill,
                "tjuaeasset-skill-demo",
                "demo",
                "skills/demo/SKILL.md",
                vec![("skills/demo/SKILL.md".to_owned(), b"# Demo\n".to_vec())],
            ),
        ];
        let mut assets = BTreeMap::new();
        let mut packages = BTreeMap::new();
        let mut bundle_entries = Vec::<(String, Vec<u8>)>::new();
        let mut remote_asset_id = String::new();
        for (kind, package_name, runtime_id, entry_file, mut files) in fixtures {
            files.sort_by(|left, right| left.0.cmp(&right.0));
            let definition = files
                .iter()
                .map(|(path, content)| AssetDefinitionFile {
                    path: path.clone(),
                    content: content.clone(),
                })
                .collect::<Vec<_>>();
            let (_, scanned) = prepare_definition(definition).unwrap();
            let file_refs = files
                .iter()
                .map(|(path, content)| (path.as_str(), content.as_slice()))
                .collect::<Vec<_>>();
            let package_bytes = test_zip(&file_refs);
            let mut content_hash = Sha256::new();
            let mut unpacked_size = 0_u64;
            let mut market_files = Vec::new();
            for (path, content) in &files {
                content_hash.update(path.as_bytes());
                content_hash.update(content);
                unpacked_size += content.len() as u64;
                market_files.push(MarketAssetFileResponse {
                    path: path.clone(),
                    digest: digest_bytes(content),
                    size: content.len() as u64,
                    media_type: if path.ends_with(".json") {
                        "application/json".into()
                    } else {
                        "text/markdown".into()
                    },
                });
            }
            let asset_id = format!("{package_name}/{}/{}", kind_segment(kind), runtime_id);
            if kind == AssetKind::Skill {
                remote_asset_id.clone_from(&asset_id);
            }
            packages.insert(
                package_name.to_owned(),
                MarketPackageDescriptor {
                    name: package_name.to_owned(),
                    version: "1.0.0".into(),
                    review_status: MarketPackageReviewStatus::Approved,
                    atomic: true,
                    asset_ids: vec![asset_id.clone()],
                    dependencies: BTreeMap::new(),
                    tarball: format!("{package_name}.zip"),
                    integrity: format!("sha256-{}", hex::encode(content_hash.finalize())),
                    archive_integrity: digest_bytes(&package_bytes),
                    unpacked_size,
                    repository: HUB_REPOSITORY.into(),
                    source_path: format!("assets/{package_name}"),
                    manifest_path: format!("assets/{package_name}/asset-package.json"),
                    source_revision: source_revision.clone(),
                },
            );
            assets.insert(
                asset_id.clone(),
                RawMarketAsset {
                    id: asset_id,
                    kind,
                    runtime_id: runtime_id.into(),
                    dependencies: Vec::new(),
                    display_name: format!("离线 {} 夹具", kind_segment(kind)),
                    description: "离线四类资产演示".into(),
                    version: "1.0.0".into(),
                    definition_digest: scanned.digest,
                    entry_file: entry_file.into(),
                    package_name: package_name.into(),
                    author: "Tjuae".into(),
                    license: "Apache-2.0".into(),
                    trust: AssetTrust::Official,
                    status: MarketAssetStatus::Active,
                    compatibility: RawCompatibility { tjuae: "^1.0.0".into() },
                    source_revision: source_revision.clone(),
                    files: market_files,
                    tags: vec!["demo".into()],
                },
            );
            let package_entry = if kind == AssetKind::Skill {
                package_entry_override
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("packages/{package_name}.zip"))
            } else {
                format!("packages/{package_name}.zip")
            };
            bundle_entries.push((package_entry, package_bytes));
        }
        let index = RawMarketIndex {
            schema: MARKET_INDEX_SCHEMA_URL.into(),
            schema_version: 2,
            generated_at: generated_at.into(),
            assets,
            packages,
            metadata: RawMarketMetadata {
                total_packages: 4,
                total_assets: 4,
                generated_by: "Tjuae 资产构建器 v3.0.0".into(),
                repository: HUB_REPOSITORY.into(),
                source_revision: source_revision.clone(),
            },
        };
        let mut seed_index_bytes = serde_json::to_vec_pretty(&index).unwrap();
        seed_index_bytes.push(b'\n');
        bundle_entries.push(("seed-index.json".into(), seed_index_bytes.clone()));
        bundle_entries.sort_by(|left, right| left.0.cmp(&right.0));
        let bundle_refs = bundle_entries
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_slice()))
            .collect::<Vec<_>>();
        let bundle = test_zip(&bundle_refs);
        let bundle_digest = digest_bytes(&bundle);
        let bundle_file_name = format!("tjuae-seed-{}.zip", bundle_digest.strip_prefix("sha256-").unwrap());
        let package_names = index.packages.keys().cloned().collect::<Vec<_>>();
        let asset_ids = index.assets.keys().cloned().collect::<Vec<_>>();
        let seed_manifest = serde_json::json!({
            "$schema": OFFLINE_SEED_SCHEMA_URL,
            "schemaVersion": 1,
            "generatedAt": generated_at,
            "sourceRevision": source_revision,
            "seedIndexDigest": digest_bytes(&seed_index_bytes),
            "bundle": {
                "fileName": bundle_file_name,
                "digest": bundle_digest,
                "size": bundle.len()
            },
            "assetKinds": ["assistant", "engineAdapter", "mcp", "skill"],
            "packageNames": package_names,
            "assetIds": asset_ids
        });
        let mut seed_manifest_bytes = serde_json::to_vec_pretty(&seed_manifest).unwrap();
        seed_manifest_bytes.push(b'\n');
        let runtime_manifest = serde_json::json!({
            "$schema": OFFLINE_RESOURCE_MANIFEST_SCHEMA,
            "schemaVersion": 1,
            "source": {
                "kind": "localSibling",
                "repository": HUB_REPOSITORY,
                "sourceRevision": source_revision
            },
            "seedManifest": {
                "fileName": "seed-manifest.json",
                "digest": digest_bytes(&seed_manifest_bytes),
                "size": seed_manifest_bytes.len()
            },
            "bundle": seed_manifest["bundle"].clone()
        });
        std::fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&runtime_manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(directory.join("seed-manifest.json"), seed_manifest_bytes).unwrap();
        std::fs::write(directory.join(&bundle_file_name), bundle).unwrap();
        let directory = directory.canonicalize().unwrap();
        (
            OfflineSeedSource {
                manifest_path: directory.join("manifest.json"),
                directory,
                dist_ref: None,
                development: true,
            },
            remote_asset_id,
        )
    }

    fn pin_offline_fixture(source: &mut OfflineSeedSource) {
        let seed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source.directory.join("seed-manifest.json")).unwrap()).unwrap();
        let revision = seed["sourceRevision"].as_str().unwrap().to_owned();
        let mut runtime: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&source.manifest_path).unwrap()).unwrap();
        runtime["source"] = serde_json::json!({
            "kind": "pinnedDist",
            "repository": HUB_REPOSITORY,
            "distRef": revision
        });
        std::fs::write(&source.manifest_path, serde_json::to_vec_pretty(&runtime).unwrap()).unwrap();
        source.dist_ref = Some(revision);
        source.development = false;
    }

    fn test_index_bytes(source_revision: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "$schema": MARKET_INDEX_SCHEMA_URL,
            "schemaVersion": 2,
            "generatedAt": "2026-08-01T00:00:00Z",
            "assets": {
                "tjuaeasset-demo/skill/demo": {
                    "id": "tjuaeasset-demo/skill/demo",
                    "kind": "skill",
                    "runtimeId": "demo",
                    "dependencies": [],
                    "displayName": "演示技能",
                    "description": "演示",
                    "version": "1.0.0",
                    "definitionDigest": format!("sha256-{}", "1".repeat(64)),
                    "entryFile": "skills/demo/SKILL.md",
                    "packageName": "tjuaeasset-demo",
                    "author": "Tjuae",
                    "license": "Apache-2.0",
                    "trust": "official",
                    "status": "active",
                    "compatibility": {"tjuae": "^1.0.0"},
                    "sourceRevision": source_revision,
                    "files": [{
                        "path": "skills/demo/SKILL.md",
                        "digest": format!("sha256-{}", "2".repeat(64)),
                        "size": 8,
                        "mediaType": "text/markdown"
                    }],
                    "tags": ["demo"]
                }
            },
            "packages": {
                "tjuaeasset-demo": {
                    "name": "tjuaeasset-demo",
                    "version": "1.0.0",
                    "reviewStatus": "approved",
                    "atomic": true,
                    "assetIds": ["tjuaeasset-demo/skill/demo"],
                    "dependencies": {},
                    "tarball": "tjuaeasset-demo.zip",
                    "integrity": format!("sha256-{}", "3".repeat(64)),
                    "archiveIntegrity": format!("sha256-{}", "4".repeat(64)),
                    "unpackedSize": 8,
                    "repository": HUB_REPOSITORY,
                    "sourcePath": "assets/tjuaeasset-demo",
                    "manifestPath": "assets/tjuaeasset-demo/asset-package.json",
                    "sourceRevision": source_revision
                }
            },
            "metadata": {
                "totalPackages": 1,
                "totalAssets": 1,
                "generatedBy": "Tjuae 资产构建器 v3.0.0",
                "repository": HUB_REPOSITORY,
                "sourceRevision": source_revision
            }
        }))
        .unwrap()
    }
}
