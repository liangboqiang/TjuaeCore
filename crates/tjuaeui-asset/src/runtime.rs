use async_trait::async_trait;
use std::collections::BTreeMap;
use tjuaeui_api_types::{AssetConfigurationSchemaDefinition, AssetKind, AssetPublicConfiguration};

use crate::{AssetDefinitionFile, AssetError};

/// A complete, verified Definition handed to the runtime projection boundary.
///
/// `runtime_configuration` 是调用时的本机配置命令，不属于 Definition，
/// 不写入 workspace/catalog，也不会进入发布包。未提供时，运行时实现必须
/// 保留已有 Overlay/偏好。
#[derive(Clone)]
pub struct RuntimeAssetDefinition {
    pub local_asset_id: String,
    pub kind: AssetKind,
    /// Definition/Hub identity. May appear in portable metadata and traces.
    pub portable_runtime_id: String,
    /// Core-only globally unique identity for legacy projection tables/paths.
    /// Must never be serialized into HTTP, Hub packages or traces.
    pub projection_runtime_id: String,
    pub entry_file: String,
    pub workspace_path: std::path::PathBuf,
    pub files: Vec<AssetDefinitionFile>,
    /// Immutable Hub asset identity -> runtime identity, resolved from the
    /// verified pinned market index. Portable Definitions keep remote IDs;
    /// runtime tables must only receive values from this explicit map.
    pub dependency_portable_runtime_ids: BTreeMap<String, String>,
    /// Same immutable dependency identity keys mapped to Core-only projection
    /// IDs. Runtime adapters use this map for exact reads from legacy tables.
    pub dependency_projection_runtime_ids: BTreeMap<String, String>,
    pub runtime_configuration: Option<RuntimeResolvedConfiguration>,
}

/// 仅存在于一次运行时调用内的已解析本机配置。
///
/// 此类型刻意不实现 `Debug`、`Serialize`，避免明文凭据进入日志、HTTP
/// 或持久化投影。具体 adapter 只能在调用期间读取 `secrets`，不得保存。
#[derive(Clone)]
pub struct RuntimeResolvedConfiguration {
    pub configuration: AssetPublicConfiguration,
    /// 已验证的 Definition 配置字段及其显式运行时绑定。该元数据不含用户值。
    pub configuration_schema: AssetConfigurationSchemaDefinition,
    pub secrets: BTreeMap<String, String>,
}

/// 真实会话启动时使用的最窄 JIT 配置解析端口。
///
/// 调用方必须传入会话所属用户和 projector 持久化的原始本地资产 ID。
/// 返回值只允许在本次启动调用栈内使用；不得写入 agent/MCP legacy 表、
/// 日志或 HTTP 响应。
#[async_trait]
pub trait RuntimeAssetConfigurationResolver: Send + Sync {
    async fn resolve(
        &self,
        user_id: &str,
        local_asset_id: &str,
    ) -> Result<Option<RuntimeResolvedConfiguration>, AssetError>;
}

/// A staged runtime change. `apply` may make the staged Definition visible,
/// but the caller will invoke `rollback` whenever the catalog transaction
/// fails. `finalize` only removes recovery material after both sides commit.
#[async_trait]
pub trait RuntimeProjectionTransaction: Send {
    async fn apply(&mut self) -> Result<(), AssetError>;
    async fn rollback(&mut self) -> Result<(), AssetError>;
    async fn finalize(self: Box<Self>);
}

/// Runtime projection port owned by Core.
///
/// The asset repository remains independent of assistant/skill/engine
/// implementations. Production wires a concrete projector in the application
/// composition root; the default implementation fails closed.
#[async_trait]
pub trait AssetRuntimeProjector: Send + Sync {
    /// 使用既有类型校验器验证 Definition 与本机配置，不产生持久化副作用。
    async fn validate(&self, user_id: &str, assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError>;

    /// 在隔离的临时上下文试跑，不写入正式运行时投影。
    async fn try_run(&self, user_id: &str, assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError>;

    async fn prepare_replace(
        &self,
        user_id: &str,
        assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError>;

    async fn prepare_remove(
        &self,
        user_id: &str,
        assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError>;
}

#[derive(Debug, Default)]
pub struct FailClosedRuntimeProjector;

#[async_trait]
impl AssetRuntimeProjector for FailClosedRuntimeProjector {
    async fn validate(&self, _user_id: &str, _assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError> {
        Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_PROJECTOR_NOT_CONFIGURED",
            message: "Core 未配置资产运行时校验器".into(),
        })
    }

    async fn try_run(&self, _user_id: &str, _assets: Vec<RuntimeAssetDefinition>) -> Result<(), AssetError> {
        Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_PROJECTOR_NOT_CONFIGURED",
            message: "Core 未配置资产运行时试跑器".into(),
        })
    }

    async fn prepare_replace(
        &self,
        _user_id: &str,
        _assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_PROJECTOR_NOT_CONFIGURED",
            message: "Core 未配置资产运行时投影器".into(),
        })
    }

    async fn prepare_remove(
        &self,
        _user_id: &str,
        _assets: Vec<RuntimeAssetDefinition>,
    ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
        Err(AssetError::RuntimeProjectionUnsupported {
            code: "RUNTIME_PROJECTOR_NOT_CONFIGURED",
            message: "Core 未配置资产运行时投影器".into(),
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Clone, Default)]
    pub struct RecordingRuntimeProjector {
        pub applied: Arc<AtomicUsize>,
        pub rolled_back: Arc<AtomicUsize>,
        pub finalized: Arc<AtomicUsize>,
        pub fail_apply: bool,
        pub fail_apply_with_rollback_code: bool,
        pub fail_rollback: bool,
    }

    struct RecordingTransaction {
        projector: RecordingRuntimeProjector,
        applied: bool,
    }

    #[async_trait]
    impl AssetRuntimeProjector for RecordingRuntimeProjector {
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
            Ok(Box::new(RecordingTransaction {
                projector: self.clone(),
                applied: false,
            }))
        }

        async fn prepare_remove(
            &self,
            _user_id: &str,
            _assets: Vec<RuntimeAssetDefinition>,
        ) -> Result<Box<dyn RuntimeProjectionTransaction>, AssetError> {
            self.prepare_replace(_user_id, _assets).await
        }
    }

    #[async_trait]
    impl RuntimeProjectionTransaction for RecordingTransaction {
        async fn apply(&mut self) -> Result<(), AssetError> {
            if self.projector.fail_apply_with_rollback_code {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "TEST_RUNTIME_APPLY_ROLLBACK_FAILED",
                    message: "测试投影内部补偿失败".into(),
                });
            }
            if self.projector.fail_apply {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "TEST_RUNTIME_APPLY_FAILED",
                    message: "测试投影失败".into(),
                });
            }
            self.applied = true;
            self.projector.applied.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn rollback(&mut self) -> Result<(), AssetError> {
            if self.projector.fail_rollback {
                return Err(AssetError::RuntimeProjectionFailed {
                    code: "TEST_RUNTIME_ROLLBACK_FAILED",
                    message: "测试回滚失败".into(),
                });
            }
            if self.applied {
                self.projector.rolled_back.fetch_add(1, Ordering::SeqCst);
                self.applied = false;
            }
            Ok(())
        }

        async fn finalize(self: Box<Self>) {
            self.projector.finalized.fetch_add(1, Ordering::SeqCst);
        }
    }
}
