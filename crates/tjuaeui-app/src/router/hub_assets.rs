use std::sync::Arc;

use async_trait::async_trait;
use tjuaeui_api_types::{AssetContentSource, AssetEditability, AssetKind, HubAssetKind};
use tjuaeui_asset::{
    AssetCatalogService, AssetError, AssetPublishError, AssetTextFile, HubAssetPort, LocalAssetMaterial,
};
use tjuaeui_assistant::asset_definition::{LOCAL_ASSISTANT_ENTRY_FILE, LocalAssistantDefinition};

/// 发布边界只读取 Core 本地资产库。运行时扩展注册表和远程市场均不参与
/// 规范包导出，避免把旧扩展状态误当作本地资产真相。
pub struct AppHubAssetPort {
    catalog: Arc<AssetCatalogService>,
}

impl AppHubAssetPort {
    pub fn new(catalog: Arc<AssetCatalogService>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl HubAssetPort for AppHubAssetPort {
    async fn export_catalog(
        &self,
        user_id: &str,
        asset_kind: HubAssetKind,
        asset_id: &str,
    ) -> Result<LocalAssetMaterial, AssetPublishError> {
        let detail = self.catalog.get(user_id, asset_id).await.map_err(catalog_asset_error)?;
        let expected_kind = match asset_kind {
            HubAssetKind::Assistant => AssetKind::Assistant,
            HubAssetKind::EngineAdapter => AssetKind::EngineAdapter,
            HubAssetKind::Skill => AssetKind::Skill,
            HubAssetKind::Mcp => AssetKind::Mcp,
        };
        if detail.asset.kind != expected_kind {
            return Err(AssetPublishError::InvalidRequest(
                "发布请求的资产类型与 Core 本地资产记录不一致".into(),
            ));
        }
        if detail.asset.editability != AssetEditability::Full {
            return Err(AssetPublishError::HubPublishPrerequisite(
                "只读或仅 Overlay 资产不能发布 Definition".into(),
            ));
        }

        let runtime_id = detail
            .asset
            .runtime_id
            .clone()
            .ok_or_else(|| AssetPublishError::AssetSanitization("本地资产缺少 runtimeId".into()))?;
        let definition_file = match expected_kind {
            AssetKind::Assistant => "assistant.json",
            AssetKind::EngineAdapter => "engine-adapter.json",
            AssetKind::Skill => "SKILL.md",
            AssetKind::Mcp => "mcp.json",
        };
        let mut dependencies = Vec::new();
        let assistant_entry = if expected_kind == AssetKind::Assistant {
            let local = self
                .catalog
                .read_file(user_id, asset_id, LOCAL_ASSISTANT_ENTRY_FILE, AssetContentSource::Local)
                .await
                .map_err(catalog_asset_error)?;
            let local: LocalAssistantDefinition = serde_json::from_str(&local.content)
                .map_err(|error| AssetPublishError::AssetSanitization(format!("本地助手 Definition 无效：{error}")))?;
            let local_dependency_ids = local.local_skill_asset_ids().map(ToOwned::to_owned).collect::<Vec<_>>();
            let remote_dependency_ids = self
                .catalog
                .remote_skill_asset_ids(user_id, &local_dependency_ids)
                .await
                .map_err(catalog_asset_error)?;
            let hub = local
                .to_hub(remote_dependency_ids)
                .map_err(|error| AssetPublishError::HubPublishPrerequisite(error.to_string()))?;
            dependencies = hub.skill_dependencies.clone();
            Some(
                serde_json::to_string_pretty(&hub)
                    .map_err(|error| AssetPublishError::AssetSanitization(error.to_string()))?,
            )
        } else {
            None
        };

        let mut files = Vec::new();
        for file in detail.files {
            if file.path == "tjuae.asset.json"
                || (expected_kind == AssetKind::Assistant && file.path == LOCAL_ASSISTANT_ENTRY_FILE)
            {
                continue;
            }
            if !file.text {
                return Err(AssetPublishError::AssetSanitization(format!(
                    "规范发布暂不接受二进制 Definition 文件：{}",
                    file.path
                )));
            }
            let content = self
                .catalog
                .read_file(user_id, asset_id, &file.path, AssetContentSource::Local)
                .await
                .map_err(catalog_asset_error)?;
            files.push(AssetTextFile {
                path: file.path,
                content: content.content,
            });
        }
        if let Some(content) = assistant_entry {
            files.push(AssetTextFile {
                path: definition_file.into(),
                content,
            });
        }

        Ok(LocalAssetMaterial {
            display_name: detail.asset.display_name,
            description: detail.asset.description.unwrap_or_default(),
            runtime_id,
            definition_file: definition_file.into(),
            dependencies,
            files,
            blocked_fields: Vec::new(),
        })
    }
}

fn catalog_asset_error(error: AssetError) -> AssetPublishError {
    let code = match error {
        AssetError::NotFound(_) => "ASSET_NOT_FOUND",
        AssetError::UnsafePath(_) => "ASSET_UNSAFE_PATH",
        AssetError::BinaryFile(_) => "ASSET_BINARY_FILE",
        AssetError::FileTooLarge { .. } | AssetError::TotalTooLarge { .. } => "ASSET_TOO_LARGE",
        AssetError::DigestMismatch { .. } | AssetError::CorruptObject(_) => "ASSET_INTEGRITY_FAILED",
        AssetError::ConcurrentModification => "ASSET_CONCURRENT_MODIFICATION",
        AssetError::MergeConflict(_) => "ASSET_MERGE_CONFLICT",
        AssetError::DestructiveConfirmationRequired => "ASSET_CONFIRMATION_REQUIRED",
        AssetError::LocalChanges => "ASSET_LOCAL_CHANGES",
        AssetError::MissingBaseSnapshot => "ASSET_BASE_MISSING",
        AssetError::SourceUnavailable(_) => "ASSET_SOURCE_UNAVAILABLE",
        AssetError::OverlayNotConfigured => "ASSET_OVERLAY_NOT_CONFIGURED",
        AssetError::UpstreamMismatch => "ASSET_UPSTREAM_MISMATCH",
        AssetError::RuntimeProjectionUnsupported { code, .. } | AssetError::RuntimeProjectionFailed { code, .. } => {
            code
        }
        AssetError::BundleInvariant(_) => "ASSET_BUNDLE_INVARIANT",
        AssetError::InvalidState(_) | AssetError::InvalidMetadata(_) => "ASSET_INVALID",
        AssetError::Database(_) | AssetError::Io(_) | AssetError::Json(_) | AssetError::Crypto(_) => "ASSET_INTERNAL",
    };
    AssetPublishError::HubPublishPrerequisite(code.into())
}
