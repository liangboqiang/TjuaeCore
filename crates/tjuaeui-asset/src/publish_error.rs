/// TjuaeHub 资产发布边界错误。
///
/// 发布协议属于 Core 资产域，不能借用应用扩展域错误。稳定的错误分类同时
/// 供 HTTP 边界和发布提供器使用，具体内部错误不会直接泄露给客户端。
#[derive(Debug, thiserror::Error)]
pub enum AssetPublishError {
    #[error("版本“{version}”无效：{reason}")]
    InvalidVersion { version: String, reason: String },

    #[error("找不到资产：{0}")]
    NotFound(String),

    #[error("访问 TjuaeHub 失败：{0}")]
    HubNetwork(String),

    #[error("Hub 资产包完整性校验失败：{0}")]
    HubIntegrity(String),

    #[error("Hub 数据体积为 {actual} 字节，超过上限 {limit} 字节")]
    HubPackageTooLarge { actual: u64, limit: u64 },

    #[error("远程发布前置条件未满足：{0}")]
    HubPublishPrerequisite(String),

    #[error("远程发布失败：{0}")]
    HubPublishFailed(String),

    #[error("远程发布幂等冲突：{0}")]
    HubPublishConflict(String),

    #[error("资产导出被安全策略拒绝：{0}")]
    AssetSanitization(String),

    #[error("请求无效：{0}")]
    InvalidRequest(String),

    #[error("资产发布内部错误：{0}")]
    Internal(String),

    #[error(transparent)]
    Database(#[from] tjuaeui_db::DbError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
