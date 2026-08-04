use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tjuaeui_common::{RuntimeAssetDigestInput, compute_runtime_asset_snapshot_id};

const SKILL_DEFINITION_DOMAIN: &[u8] = b"tjuae-runtime-skill-v1\0";

/// Safe, provider-neutral identity of one runtime asset definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAssetRef {
    pub local_asset_id: String,
    pub kind: String,
    /// Digest of the editable Definition recorded and verified by
    /// AssetCatalog. This is the provenance digest exposed by Trace.
    pub local_definition_digest: String,
    /// Digest of the exact runtime-effective content. For managed skills the
    /// runtime recomputes this value after loading the filesystem tree.
    pub runtime_content_digest: String,
    pub upstream_package: Option<String>,
    pub upstream_asset_id: Option<String>,
    pub upstream_version: Option<String>,
    pub upstream_revision: Option<String>,
}

/// Process-local binding between a safe asset identity and its materialized
/// root. It deliberately cannot be serialized and redacts the root in Debug.
#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeManagedSkillRef {
    pub asset: RuntimeAssetRef,
    pub root: PathBuf,
}

/// Process-local binding from an MCP Definition to the runtime server name
/// used for handshake confirmation. The binding is never persisted to Trace.
#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeManagedMcpRef {
    pub asset: RuntimeAssetRef,
    pub server_name: String,
}

impl fmt::Debug for RuntimeManagedMcpRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeManagedMcpRef")
            .field("asset", &self.asset)
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl fmt::Debug for RuntimeManagedSkillRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeManagedSkillRef")
            .field("asset", &self.asset)
            .field("root", &"<redacted>")
            .finish()
    }
}

/// Exact asset request passed from conversation resolution to an agent
/// factory. `core_assets` are definitions Core itself applies (for example an
/// assistant rule snapshot); `managed_skills` require a runtime load receipt.
#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeAssetLoadRequest {
    pub runtime_snapshot_id: String,
    pub core_assets: Vec<RuntimeAssetRef>,
    pub runtime_assets: Vec<RuntimeAssetRef>,
    pub managed_skills: Vec<RuntimeManagedSkillRef>,
    pub managed_mcps: Vec<RuntimeManagedMcpRef>,
}

impl fmt::Debug for RuntimeAssetLoadRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeAssetLoadRequest")
            .field("runtime_snapshot_id", &self.runtime_snapshot_id)
            .field("core_assets", &self.core_assets)
            .field("runtime_assets", &self.runtime_assets)
            .field("managed_skills", &self.managed_skills)
            .field("managed_mcps", &self.managed_mcps)
            .finish()
    }
}

impl RuntimeAssetLoadRequest {
    pub fn new(
        core_assets: Vec<RuntimeAssetRef>,
        managed_skills: Vec<RuntimeManagedSkillRef>,
    ) -> Result<Option<Self>, RuntimeAssetContractError> {
        Self::new_with_runtime_assets(core_assets, Vec::new(), managed_skills, Vec::new())
    }

    pub fn new_with_runtime_assets(
        mut core_assets: Vec<RuntimeAssetRef>,
        mut runtime_assets: Vec<RuntimeAssetRef>,
        mut managed_skills: Vec<RuntimeManagedSkillRef>,
        mut managed_mcps: Vec<RuntimeManagedMcpRef>,
    ) -> Result<Option<Self>, RuntimeAssetContractError> {
        core_assets.sort_by(runtime_asset_order);
        runtime_assets.sort_by(runtime_asset_order);
        managed_skills.sort_by(|left, right| runtime_asset_order(&left.asset, &right.asset));
        managed_mcps.sort_by(|left, right| runtime_asset_order(&left.asset, &right.asset));
        let assets = core_assets
            .iter()
            .cloned()
            .chain(runtime_assets.iter().cloned())
            .chain(managed_skills.iter().map(|skill| skill.asset.clone()))
            .chain(managed_mcps.iter().map(|mcp| mcp.asset.clone()))
            .collect::<Vec<_>>();
        if assets.is_empty() {
            return Ok(None);
        }
        validate_assets(&assets)?;
        if runtime_assets.iter().any(|asset| asset.kind != "engineAdapter") {
            return Err(RuntimeAssetContractError::UnsupportedCoreAssetKind(
                "runtime-owned asset must be engineAdapter".to_owned(),
            ));
        }
        let mut mcp_server_names = HashSet::new();
        if managed_mcps.iter().any(|mcp| {
            mcp.asset.kind != "mcp"
                || !safe_asset_id(&mcp.server_name)
                || !mcp_server_names.insert(mcp.server_name.as_str())
        }) {
            return Err(RuntimeAssetContractError::UnsafeMcpBinding);
        }
        let runtime_snapshot_id = deterministic_runtime_snapshot_id(&assets)?;
        Ok(Some(Self {
            runtime_snapshot_id,
            core_assets,
            runtime_assets,
            managed_skills,
            managed_mcps,
        }))
    }

    pub fn requested_assets(&self) -> Vec<RuntimeAssetRef> {
        let mut assets = self
            .core_assets
            .iter()
            .cloned()
            .chain(self.runtime_assets.iter().cloned())
            .chain(self.managed_skills.iter().map(|skill| skill.asset.clone()))
            .chain(self.managed_mcps.iter().map(|mcp| mcp.asset.clone()))
            .collect::<Vec<_>>();
        assets.sort_by(runtime_asset_order);
        assets
    }
}

/// Runtime-produced receipt. This is the only shape that may be attached to a
/// Trace; request-side values are never treated as proof of loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAssetLoadReceipt {
    pub runtime_snapshot_id: String,
    pub assets: Vec<RuntimeAssetRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBoundaryPhase {
    Resolve,
    Project,
    Spawn,
    Handshake,
    Inject,
    Connect,
}

impl RuntimeBoundaryPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Project => "project",
            Self::Spawn => "spawn",
            Self::Handshake => "handshake",
            Self::Inject => "inject",
            Self::Connect => "connect",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBoundaryStatus {
    Succeeded,
    Failed,
}

/// Safe, process-local lifecycle event emitted by the operation that proves
/// the boundary. Free-form messages and runtime configuration are deliberately
/// absent, so credentials, paths, commands, and protocol payloads cannot leak
/// into Trace through this channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBoundaryEvent {
    pub phase: RuntimeBoundaryPhase,
    pub status: RuntimeBoundaryStatus,
    pub started_at: i64,
    pub ended_at: i64,
    pub asset_kind: Option<String>,
    pub local_asset_id: Option<String>,
    pub error_code: Option<String>,
}

impl RuntimeBoundaryEvent {
    pub fn succeeded(
        phase: RuntimeBoundaryPhase,
        started_at: i64,
        ended_at: i64,
        asset: Option<&RuntimeAssetRef>,
    ) -> Self {
        Self::new(
            phase,
            RuntimeBoundaryStatus::Succeeded,
            started_at,
            ended_at,
            asset,
            None,
        )
    }

    pub fn failed(
        phase: RuntimeBoundaryPhase,
        started_at: i64,
        ended_at: i64,
        asset: Option<&RuntimeAssetRef>,
        error_code: &'static str,
    ) -> Self {
        Self::new(
            phase,
            RuntimeBoundaryStatus::Failed,
            started_at,
            ended_at,
            asset,
            Some(error_code.to_owned()),
        )
    }

    fn new(
        phase: RuntimeBoundaryPhase,
        status: RuntimeBoundaryStatus,
        started_at: i64,
        ended_at: i64,
        asset: Option<&RuntimeAssetRef>,
        error_code: Option<String>,
    ) -> Self {
        Self {
            phase,
            status,
            started_at,
            ended_at: ended_at.max(started_at),
            asset_kind: asset.map(|asset| asset.kind.clone()),
            local_asset_id: asset.map(|asset| asset.local_asset_id.clone()),
            error_code,
        }
    }
}

/// Best-effort reporter shared by a single task build. The callback receives
/// only [`RuntimeBoundaryEvent`], whose closed shape excludes sensitive data.
#[derive(Clone)]
pub struct RuntimeBoundaryReporter {
    callback: Arc<dyn Fn(RuntimeBoundaryEvent) + Send + Sync>,
}

impl RuntimeBoundaryReporter {
    pub fn new(callback: impl Fn(RuntimeBoundaryEvent) + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    pub fn report(&self, event: RuntimeBoundaryEvent) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.callback)(event)));
    }

    pub fn succeeded(
        &self,
        phase: RuntimeBoundaryPhase,
        started_at: i64,
        ended_at: i64,
        asset: Option<&RuntimeAssetRef>,
    ) {
        self.report(RuntimeBoundaryEvent::succeeded(phase, started_at, ended_at, asset));
    }

    pub fn failed(
        &self,
        phase: RuntimeBoundaryPhase,
        started_at: i64,
        ended_at: i64,
        asset: Option<&RuntimeAssetRef>,
        error_code: &'static str,
    ) {
        self.report(RuntimeBoundaryEvent::failed(
            phase, started_at, ended_at, asset, error_code,
        ));
    }
}

impl fmt::Debug for RuntimeBoundaryReporter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuntimeBoundaryReporter(<callback>)")
    }
}

/// Validate and translate the TjuaeCLI process-local boundary contract before
/// it reaches the Core reporter. Invalid records are rejected rather than
/// normalized or inferred.
pub fn runtime_boundary_event_from_tjuae_cli(
    record: tjuae_types::runtime_asset::RuntimeBoundaryRecord,
) -> Result<RuntimeBoundaryEvent, &'static str> {
    use tjuae_types::runtime_asset::{
        RUNTIME_BOUNDARY_RECORD_VERSION, RuntimeBoundaryPhase as CliPhase, RuntimeBoundaryStatus as CliStatus,
    };

    if record.version != RUNTIME_BOUNDARY_RECORD_VERSION {
        return Err("TJUAE_RUNTIME_BOUNDARY_VERSION_INVALID");
    }
    if record.started_at_ms < 0 || record.ended_at_ms < record.started_at_ms {
        return Err("TJUAE_RUNTIME_BOUNDARY_TIME_INVALID");
    }
    if record.asset_kind.is_some() != record.local_asset_id.is_some() {
        return Err("TJUAE_RUNTIME_BOUNDARY_ASSET_IDENTITY_INCOMPLETE");
    }
    if let Some(kind) = record.asset_kind.as_deref()
        && (!matches!(kind, "assistant" | "engineAdapter" | "skill" | "mcp") || !safe_asset_id(kind))
    {
        return Err("TJUAE_RUNTIME_BOUNDARY_ASSET_KIND_INVALID");
    }
    if let Some(local_asset_id) = record.local_asset_id.as_deref()
        && !safe_asset_id(local_asset_id)
    {
        return Err("TJUAE_RUNTIME_BOUNDARY_ASSET_ID_INVALID");
    }
    match (&record.status, record.error_code.as_deref()) {
        (CliStatus::Succeeded, None) => {}
        (CliStatus::Failed, Some(code)) if safe_error_code(code) => {}
        _ => return Err("TJUAE_RUNTIME_BOUNDARY_STATUS_INVALID"),
    }

    Ok(RuntimeBoundaryEvent {
        phase: match record.phase {
            CliPhase::Resolve => RuntimeBoundaryPhase::Resolve,
            CliPhase::Project => RuntimeBoundaryPhase::Project,
            CliPhase::Spawn => RuntimeBoundaryPhase::Spawn,
            CliPhase::Handshake => RuntimeBoundaryPhase::Handshake,
            CliPhase::Inject => RuntimeBoundaryPhase::Inject,
            CliPhase::Connect => RuntimeBoundaryPhase::Connect,
        },
        status: match record.status {
            CliStatus::Succeeded => RuntimeBoundaryStatus::Succeeded,
            CliStatus::Failed => RuntimeBoundaryStatus::Failed,
        },
        started_at: record.started_at_ms,
        ended_at: record.ended_at_ms,
        asset_kind: record.asset_kind,
        local_asset_id: record.local_asset_id,
        error_code: record.error_code,
    })
}

/// Explicit port implemented by runtime managers capable of returning an
/// actual load receipt.
pub trait RuntimeAssetReceiptPort {
    fn runtime_asset_receipt(&self) -> Option<RuntimeAssetLoadReceipt>;
}

/// Stable, machine-readable reason for rejecting a turn at the runtime asset
/// receipt boundary. These values are also emitted through the existing agent
/// error `code` field; diagnostics remain human-readable and path-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeAssetFailureReason {
    ReceiptUnsupported,
    ReceiptMissing,
    ReceiptUnexpected,
    ReceiptMismatch,
    ReceiptPersistFailed,
}

impl RuntimeAssetFailureReason {
    pub const fn as_code(self) -> &'static str {
        match self {
            Self::ReceiptUnsupported => "TJUAE_RUNTIME_ASSET_RECEIPT_UNSUPPORTED",
            Self::ReceiptMissing => "TJUAE_RUNTIME_ASSET_RECEIPT_MISSING",
            Self::ReceiptUnexpected => "TJUAE_RUNTIME_ASSET_RECEIPT_UNEXPECTED",
            Self::ReceiptMismatch => "TJUAE_RUNTIME_ASSET_RECEIPT_MISMATCH",
            Self::ReceiptPersistFailed => "TJUAE_RUNTIME_ASSET_RECEIPT_PERSIST_FAILED",
        }
    }
}

impl fmt::Display for RuntimeAssetFailureReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_code())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeAssetContractError {
    #[error("runtime asset id is empty or unsafe")]
    UnsafeAssetId,
    #[error("runtime asset kind is unsupported: {0}")]
    UnsupportedKind(String),
    #[error("runtime asset local definition digest must be a lowercase sha256 digest")]
    InvalidLocalDefinitionDigest,
    #[error("runtime asset content digest must be a lowercase sha256 digest")]
    InvalidRuntimeContentDigest,
    #[error("runtime asset upstream identity is unsafe or non-canonical")]
    UnsafeUpstreamIdentity,
    #[error("runtime asset upstream identity must be either complete or absent")]
    IncompleteUpstreamIdentity,
    #[error("runtime asset identity is duplicated: {kind}:{local_asset_id}")]
    DuplicateAsset { kind: String, local_asset_id: String },
    #[error("runtime asset snapshot id does not match the canonical receipt")]
    SnapshotIdMismatch,
    #[error("runtime asset receipt does not match the requested asset set")]
    ReceiptAssetMismatch,
    #[error("Core cannot attest runtime asset kind: {0}")]
    UnsupportedCoreAssetKind(String),
    #[error("runtime skill root is unavailable")]
    RootUnavailable,
    #[error("runtime skill definition tree escapes its root")]
    OutsideRoot,
    #[error("runtime skill definition tree contains an alias or cycle")]
    AliasOrCycle,
    #[error("runtime skill definition tree contains an unsupported entry")]
    UnsupportedEntry,
    #[error("runtime skill definition tree cannot be read")]
    ReadFailed,
    #[error("runtime skill definition path is not valid UTF-8")]
    NonUtf8Path,
    #[error("runtime MCP binding is unsafe, duplicated, or has the wrong kind")]
    UnsafeMcpBinding,
}

pub fn deterministic_runtime_snapshot_id(assets: &[RuntimeAssetRef]) -> Result<String, RuntimeAssetContractError> {
    let mut assets = assets.to_vec();
    assets.sort_by(runtime_asset_order);
    validate_assets(&assets)?;
    Ok(compute_runtime_asset_snapshot_id(
        assets
            .iter()
            .map(|asset| RuntimeAssetDigestInput {
                local_asset_id: &asset.local_asset_id,
                kind: &asset.kind,
                local_definition_digest: &asset.local_definition_digest,
                runtime_content_digest: &asset.runtime_content_digest,
                upstream_package: asset.upstream_package.as_deref(),
                upstream_asset_id: asset.upstream_asset_id.as_deref(),
                upstream_version: asset.upstream_version.as_deref(),
                upstream_revision: asset.upstream_revision.as_deref(),
            })
            .collect(),
    ))
}

/// Validate an actual runtime receipt against the request. The returned
/// receipt is canonicalized and is therefore safe to persist. A changed file
/// between preflight and runtime loading changes the digest and fails closed.
pub fn verify_runtime_asset_receipt(
    request: &RuntimeAssetLoadRequest,
    receipt: RuntimeAssetLoadReceipt,
) -> Result<RuntimeAssetLoadReceipt, RuntimeAssetContractError> {
    let mut actual_assets = receipt.assets;
    actual_assets.sort_by(runtime_asset_order);
    validate_assets(&actual_assets)?;
    if actual_assets != request.requested_assets() {
        return Err(RuntimeAssetContractError::ReceiptAssetMismatch);
    }
    let actual_snapshot_id = deterministic_runtime_snapshot_id(&actual_assets)?;
    if receipt.runtime_snapshot_id != request.runtime_snapshot_id || receipt.runtime_snapshot_id != actual_snapshot_id {
        return Err(RuntimeAssetContractError::SnapshotIdMismatch);
    }
    Ok(RuntimeAssetLoadReceipt {
        runtime_snapshot_id: actual_snapshot_id,
        assets: actual_assets,
    })
}

/// Build a truthful receipt for assets Core applies itself. Managed skills are
/// intentionally rejected because only the runtime can attest that they
/// loaded successfully.
pub fn core_only_runtime_asset_receipt(
    request: &RuntimeAssetLoadRequest,
) -> Result<RuntimeAssetLoadReceipt, RuntimeAssetContractError> {
    if !request.runtime_assets.is_empty() || !request.managed_skills.is_empty() || !request.managed_mcps.is_empty() {
        return Err(RuntimeAssetContractError::ReceiptAssetMismatch);
    }
    if let Some(asset) = request.core_assets.iter().find(|asset| asset.kind != "assistant") {
        return Err(RuntimeAssetContractError::UnsupportedCoreAssetKind(asset.kind.clone()));
    }
    verify_runtime_asset_receipt(
        request,
        RuntimeAssetLoadReceipt {
            runtime_snapshot_id: request.runtime_snapshot_id.clone(),
            assets: request.core_assets.clone(),
        },
    )
}

/// 在运行时完成自身握手后，为 Core 已应用的助手定义和运行时已确认的
/// 引擎适配器生成回执。技能与 MCP 必须由能够观察其真实加载/连接结果的
/// 运行时单独确认，不能仅凭启动参数生成回执。
pub fn handshake_runtime_asset_receipt(
    request: &RuntimeAssetLoadRequest,
) -> Result<RuntimeAssetLoadReceipt, RuntimeAssetContractError> {
    if !request.managed_skills.is_empty() || !request.managed_mcps.is_empty() {
        return Err(RuntimeAssetContractError::ReceiptAssetMismatch);
    }
    if let Some(asset) = request.core_assets.iter().find(|asset| asset.kind != "assistant") {
        return Err(RuntimeAssetContractError::UnsupportedCoreAssetKind(asset.kind.clone()));
    }
    if let Some(asset) = request
        .runtime_assets
        .iter()
        .find(|asset| asset.kind != "engineAdapter")
    {
        return Err(RuntimeAssetContractError::UnsupportedCoreAssetKind(asset.kind.clone()));
    }
    verify_runtime_asset_receipt(
        request,
        RuntimeAssetLoadReceipt {
            runtime_snapshot_id: request.runtime_snapshot_id.clone(),
            assets: request.requested_assets(),
        },
    )
}

/// Digest runtime-effective data without retaining its contents.
pub fn digest_runtime_definition(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hash_length_prefixed(&mut hasher, part);
    }
    format!("sha256-{:x}", hasher.finalize())
}

/// Preflight the complete skill tree with the same digest domain and
/// canonical containment rules used by the TjuaeCLI managed-skill contract.
pub async fn digest_runtime_skill_tree(root: &Path) -> Result<String, RuntimeAssetContractError> {
    let canonical_root = tokio::fs::canonicalize(root)
        .await
        .map_err(|_| RuntimeAssetContractError::RootUnavailable)?;
    let metadata = tokio::fs::metadata(&canonical_root)
        .await
        .map_err(|_| RuntimeAssetContractError::RootUnavailable)?;
    if !metadata.is_dir() {
        return Err(RuntimeAssetContractError::RootUnavailable);
    }

    let mut visited_dirs = HashSet::from([canonical_root.clone()]);
    let mut visited_files = HashSet::new();
    let mut files = Vec::new();
    collect_definition_files(
        &canonical_root,
        &canonical_root,
        &mut visited_dirs,
        &mut visited_files,
        &mut files,
    )
    .await?;
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut hasher = Sha256::new();
    hasher.update(SKILL_DEFINITION_DOMAIN);
    for (relative_path, canonical_path) in files {
        let content = tokio::fs::read(canonical_path)
            .await
            .map_err(|_| RuntimeAssetContractError::ReadFailed)?;
        hash_length_prefixed(&mut hasher, relative_path.as_bytes());
        hash_length_prefixed(&mut hasher, &content);
    }
    Ok(format!("sha256-{:x}", hasher.finalize()))
}

type DefinitionFile = (String, PathBuf);

fn collect_definition_files<'a>(
    root: &'a Path,
    directory: &'a Path,
    visited_dirs: &'a mut HashSet<PathBuf>,
    visited_files: &'a mut HashSet<PathBuf>,
    files: &'a mut Vec<DefinitionFile>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), RuntimeAssetContractError>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = tokio::fs::read_dir(directory)
            .await
            .map_err(|_| RuntimeAssetContractError::ReadFailed)?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| RuntimeAssetContractError::ReadFailed)?
        {
            let canonical_path = tokio::fs::canonicalize(entry.path())
                .await
                .map_err(|_| RuntimeAssetContractError::ReadFailed)?;
            if !path_is_within(root, &canonical_path) {
                return Err(RuntimeAssetContractError::OutsideRoot);
            }
            let metadata = tokio::fs::metadata(&canonical_path)
                .await
                .map_err(|_| RuntimeAssetContractError::ReadFailed)?;
            if metadata.is_dir() {
                if !visited_dirs.insert(canonical_path.clone()) {
                    return Err(RuntimeAssetContractError::AliasOrCycle);
                }
                collect_definition_files(root, &canonical_path, visited_dirs, visited_files, files).await?;
            } else if metadata.is_file() {
                if !visited_files.insert(canonical_path.clone()) {
                    return Err(RuntimeAssetContractError::AliasOrCycle);
                }
                files.push((relative_utf8_path(root, &canonical_path)?, canonical_path));
            } else {
                return Err(RuntimeAssetContractError::UnsupportedEntry);
            }
        }
        Ok(())
    })
}

fn relative_utf8_path(root: &Path, path: &Path) -> Result<String, RuntimeAssetContractError> {
    path.strip_prefix(root)
        .map_err(|_| RuntimeAssetContractError::OutsideRoot)?
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(str::to_owned)
                .ok_or(RuntimeAssetContractError::NonUtf8Path)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

fn validate_assets(assets: &[RuntimeAssetRef]) -> Result<(), RuntimeAssetContractError> {
    let mut seen = HashSet::new();
    for asset in assets {
        if !safe_asset_id(&asset.local_asset_id) {
            return Err(RuntimeAssetContractError::UnsafeAssetId);
        }
        if !matches!(asset.kind.as_str(), "assistant" | "engineAdapter" | "skill" | "mcp") {
            return Err(RuntimeAssetContractError::UnsupportedKind(asset.kind.clone()));
        }
        if !is_sha256_digest(&asset.local_definition_digest) {
            return Err(RuntimeAssetContractError::InvalidLocalDefinitionDigest);
        }
        if !is_sha256_digest(&asset.runtime_content_digest) {
            return Err(RuntimeAssetContractError::InvalidRuntimeContentDigest);
        }
        validate_optional_upstream_identity(asset.upstream_package.as_deref(), 256, true)?;
        validate_optional_upstream_identity(asset.upstream_asset_id.as_deref(), 256, false)?;
        validate_optional_upstream_identity(asset.upstream_version.as_deref(), 128, false)?;
        validate_optional_upstream_identity(asset.upstream_revision.as_deref(), 256, false)?;
        let upstream_fields = [
            asset.upstream_package.is_some(),
            asset.upstream_asset_id.is_some(),
            asset.upstream_version.is_some(),
            asset.upstream_revision.is_some(),
        ];
        if upstream_fields.iter().any(|present| *present) && !upstream_fields.iter().all(|present| *present) {
            return Err(RuntimeAssetContractError::IncompleteUpstreamIdentity);
        }
        if !seen.insert((asset.kind.clone(), asset.local_asset_id.clone())) {
            return Err(RuntimeAssetContractError::DuplicateAsset {
                kind: asset.kind.clone(),
                local_asset_id: asset.local_asset_id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_optional_upstream_identity(
    value: Option<&str>,
    max_len: usize,
    package: bool,
) -> Result<(), RuntimeAssetContractError> {
    let Some(value) = value else {
        return Ok(());
    };
    let lower = value.to_ascii_lowercase();
    let valid = value == value.trim()
        && !value.is_empty()
        && value.len() <= max_len
        && !value.starts_with(['/', '\\'])
        && !value.contains('\\')
        && !value.contains("..")
        && !value.contains("://")
        && !lower.starts_with("bearer")
        && !lower.starts_with("sk-")
        && !lower.starts_with("ghp_")
        && !lower.contains("token=")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'@' | b'+' | b'/' | b':'))
        && !(value.len() >= 3 && value.as_bytes()[1] == b':' && matches!(value.as_bytes()[2], b'/' | b'\\'))
        && (!package || value.contains('/') || !value.contains(':'));
    if valid {
        Ok(())
    } else {
        Err(RuntimeAssetContractError::UnsafeUpstreamIdentity)
    }
}

fn runtime_asset_order(left: &RuntimeAssetRef, right: &RuntimeAssetRef) -> std::cmp::Ordering {
    (&left.kind, &left.local_asset_id).cmp(&(&right.kind, &right.local_asset_id))
}

fn safe_asset_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'@' | b'+'))
}

fn safe_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256-").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn hash_length_prefixed(hasher: &mut Sha256, value: &[u8]) {
    // Must match the managed-skill receipt algorithm in TjuaeCLI.
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[cfg(not(windows))]
fn path_is_within(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(windows)]
fn path_is_within(root: &Path, path: &Path) -> bool {
    let normalized_root = root.to_string_lossy().to_lowercase();
    let normalized_path = path.to_string_lossy().to_lowercase();
    normalized_path == normalized_root
        || normalized_path
            .strip_prefix(&normalized_root)
            .is_some_and(|suffix| suffix.starts_with(['\\', '/']))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(id: &str, digest_byte: char) -> RuntimeAssetRef {
        RuntimeAssetRef {
            local_asset_id: id.into(),
            kind: "skill".into(),
            local_definition_digest: format!("sha256-{}", "d".repeat(64)),
            runtime_content_digest: format!("sha256-{}", digest_byte.to_string().repeat(64)),
            upstream_package: None,
            upstream_asset_id: None,
            upstream_version: None,
            upstream_revision: None,
        }
    }

    #[test]
    fn snapshot_id_is_stable_across_input_order() {
        let left = deterministic_runtime_snapshot_id(&[asset("b", 'b'), asset("a", 'a')]).unwrap();
        let right = deterministic_runtime_snapshot_id(&[asset("a", 'a'), asset("b", 'b')]).unwrap();
        assert_eq!(left, right);
    }

    #[test]
    fn request_debug_redacts_local_roots() {
        let request = RuntimeAssetLoadRequest::new(
            Vec::new(),
            vec![RuntimeManagedSkillRef {
                asset: asset("private", 'a'),
                root: PathBuf::from(r"C:\Users\example\secret-skill"),
            }],
        )
        .unwrap()
        .unwrap();
        let debug = format!("{request:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("Users"));
        assert!(!debug.contains("secret-skill"));
    }

    #[test]
    fn actual_receipt_is_required_to_match_content_digest() {
        let request = RuntimeAssetLoadRequest::new(
            Vec::new(),
            vec![RuntimeManagedSkillRef {
                asset: asset("review", 'a'),
                root: PathBuf::from("ignored"),
            }],
        )
        .unwrap()
        .unwrap();
        let changed = RuntimeAssetLoadReceipt {
            runtime_snapshot_id: request.runtime_snapshot_id.clone(),
            assets: vec![asset("review", 'b')],
        };
        assert_eq!(
            verify_runtime_asset_receipt(&request, changed),
            Err(RuntimeAssetContractError::ReceiptAssetMismatch)
        );
    }

    #[test]
    fn request_canonicalizes_all_four_runtime_asset_kinds() {
        let mut assistant = asset("assistant", 'a');
        assistant.kind = "assistant".into();
        let mut engine = asset("engine", 'b');
        engine.kind = "engineAdapter".into();
        let skill = asset("skill", 'c');
        let mut mcp = asset("mcp", 'd');
        mcp.kind = "mcp".into();

        let request = RuntimeAssetLoadRequest::new_with_runtime_assets(
            vec![assistant],
            vec![engine],
            vec![RuntimeManagedSkillRef {
                asset: skill,
                root: PathBuf::from("redacted"),
            }],
            vec![RuntimeManagedMcpRef {
                asset: mcp,
                server_name: "docs".into(),
            }],
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            request
                .requested_assets()
                .iter()
                .map(|asset| asset.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["assistant", "engineAdapter", "mcp", "skill"]
        );
        assert_eq!(
            deterministic_runtime_snapshot_id(&request.requested_assets()).unwrap(),
            request.runtime_snapshot_id
        );
    }

    #[test]
    fn request_rejects_unconfirmed_or_ambiguous_mcp_bindings() {
        let mut first = asset("mcp-a", 'a');
        first.kind = "mcp".into();
        let mut second = asset("mcp-b", 'b');
        second.kind = "mcp".into();

        let result = RuntimeAssetLoadRequest::new_with_runtime_assets(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![
                RuntimeManagedMcpRef {
                    asset: first,
                    server_name: "docs".into(),
                },
                RuntimeManagedMcpRef {
                    asset: second,
                    server_name: "docs".into(),
                },
            ],
        );

        assert_eq!(result, Err(RuntimeAssetContractError::UnsafeMcpBinding));
    }

    #[test]
    fn core_only_adapter_refuses_to_attest_managed_skills() {
        let request = RuntimeAssetLoadRequest::new(
            Vec::new(),
            vec![RuntimeManagedSkillRef {
                asset: asset("managed", 'a'),
                root: PathBuf::from("ignored"),
            }],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            core_only_runtime_asset_receipt(&request),
            Err(RuntimeAssetContractError::ReceiptAssetMismatch)
        );
    }

    #[test]
    fn core_only_adapter_refuses_non_assistant_assets_and_noncanonical_upstream() {
        let skill_request = RuntimeAssetLoadRequest::new(vec![asset("not-core-owned", 'a')], Vec::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            core_only_runtime_asset_receipt(&skill_request),
            Err(RuntimeAssetContractError::UnsupportedCoreAssetKind("skill".into()))
        );

        let mut assistant = asset("assistant", 'a');
        assistant.kind = "assistant".into();
        assistant.upstream_version = Some(" 1.0.0 ".into());
        assert_eq!(
            RuntimeAssetLoadRequest::new(vec![assistant], Vec::new()),
            Err(RuntimeAssetContractError::UnsafeUpstreamIdentity)
        );
    }

    #[test]
    fn handshake_receipt_confirms_assistant_and_engine_after_runtime_open() {
        let mut assistant = asset("assistant", 'a');
        assistant.kind = "assistant".into();
        let mut engine = asset("engine", 'b');
        engine.kind = "engineAdapter".into();
        let request =
            RuntimeAssetLoadRequest::new_with_runtime_assets(vec![assistant], vec![engine], Vec::new(), Vec::new())
                .unwrap()
                .unwrap();

        let receipt = handshake_runtime_asset_receipt(&request).unwrap();
        assert_eq!(receipt.runtime_snapshot_id, request.runtime_snapshot_id);
        assert_eq!(receipt.assets, request.requested_assets());
    }

    #[test]
    fn handshake_receipt_never_infers_mcp_connection_from_configuration() {
        let mut mcp = asset("mcp", 'a');
        mcp.kind = "mcp".into();
        let request = RuntimeAssetLoadRequest::new_with_runtime_assets(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![RuntimeManagedMcpRef {
                asset: mcp,
                server_name: "docs".into(),
            }],
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            handshake_runtime_asset_receipt(&request),
            Err(RuntimeAssetContractError::ReceiptAssetMismatch)
        );
    }

    #[tokio::test]
    async fn skill_digest_matches_tjuae_cli_managed_skill_contract() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("SKILL.md"), "# demo\n").unwrap();

        assert_eq!(
            digest_runtime_skill_tree(temp.path()).await.unwrap(),
            "sha256-4ca1c9e24ad0a81ddc478603c6c5ac69a3fc41a9b09327680535065931c805ae"
        );
    }

    #[test]
    fn tjuae_cli_boundary_is_validated_before_forwarding() {
        use tjuae_types::runtime_asset::{
            RuntimeAssetRef as CliAsset, RuntimeBoundaryPhase as CliPhase, RuntimeBoundaryRecord,
        };
        let record = RuntimeBoundaryRecord::succeeded(
            CliPhase::Connect,
            10,
            12,
            Some(&CliAsset {
                local_asset_id: "mcp.docs".into(),
                kind: "mcp".into(),
                local_definition_digest: format!("sha256-{}", "a".repeat(64)),
                runtime_content_digest: format!("sha256-{}", "b".repeat(64)),
                upstream_package: None,
                upstream_asset_id: None,
                upstream_version: None,
                upstream_revision: None,
            }),
        );

        let event = runtime_boundary_event_from_tjuae_cli(record).unwrap();
        assert_eq!(event.phase, RuntimeBoundaryPhase::Connect);
        assert_eq!(event.status, RuntimeBoundaryStatus::Succeeded);
        assert_eq!(event.started_at, 10);
        assert_eq!(event.ended_at, 12);
        assert_eq!(event.asset_kind.as_deref(), Some("mcp"));
        assert_eq!(event.local_asset_id.as_deref(), Some("mcp.docs"));
        assert!(event.error_code.is_none());
    }

    #[test]
    fn tjuae_cli_boundary_rejects_bad_version_time_identity_and_status() {
        use tjuae_types::runtime_asset::{
            RuntimeBoundaryPhase as CliPhase, RuntimeBoundaryRecord, RuntimeBoundaryStatus as CliStatus,
        };
        let base = RuntimeBoundaryRecord {
            version: tjuae_types::runtime_asset::RUNTIME_BOUNDARY_RECORD_VERSION,
            phase: CliPhase::Inject,
            status: CliStatus::Failed,
            started_at_ms: 12,
            ended_at_ms: 10,
            asset_kind: Some("skill".into()),
            local_asset_id: Some("../../secret".into()),
            error_code: None,
        };
        assert_eq!(
            runtime_boundary_event_from_tjuae_cli(base.clone()),
            Err("TJUAE_RUNTIME_BOUNDARY_TIME_INVALID")
        );

        let mut bad_version = base.clone();
        bad_version.version += 1;
        bad_version.ended_at_ms = 12;
        assert_eq!(
            runtime_boundary_event_from_tjuae_cli(bad_version),
            Err("TJUAE_RUNTIME_BOUNDARY_VERSION_INVALID")
        );

        let mut bad_identity = base.clone();
        bad_identity.ended_at_ms = 12;
        bad_identity.error_code = Some("TJUAE_RUNTIME_SKILL_LOAD_FAILED".into());
        assert_eq!(
            runtime_boundary_event_from_tjuae_cli(bad_identity),
            Err("TJUAE_RUNTIME_BOUNDARY_ASSET_ID_INVALID")
        );

        let mut bad_status = base;
        bad_status.ended_at_ms = 12;
        bad_status.local_asset_id = Some("skill.safe".into());
        assert_eq!(
            runtime_boundary_event_from_tjuae_cli(bad_status),
            Err("TJUAE_RUNTIME_BOUNDARY_STATUS_INVALID")
        );
    }

    #[test]
    fn boundary_reporter_is_best_effort_when_callback_panics() {
        let reporter = RuntimeBoundaryReporter::new(|_| panic!("observer failure"));
        reporter.succeeded(RuntimeBoundaryPhase::Spawn, 1, 2, None);
    }
}
