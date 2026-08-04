use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("找不到资产：{0}")]
    NotFound(String),
    #[error("资产路径不安全：{0}")]
    UnsafePath(String),
    #[error("资产文件不是 UTF-8 文本：{0}")]
    BinaryFile(String),
    #[error("资产文件超过大小限制：{path}")]
    FileTooLarge { path: String, actual: u64, limit: u64 },
    #[error("资产总大小超过限制")]
    TotalTooLarge { actual: u64, limit: u64 },
    #[error("资产内容摘要不一致")]
    DigestMismatch { expected: String, actual: String },
    #[error("资产文件已被其他操作修改")]
    ConcurrentModification,
    #[error("资产存在不能安全自动合并的文件：{0:?}")]
    MergeConflict(Vec<String>),
    #[error("采用远程内容前必须显式确认")]
    DestructiveConfirmationRequired,
    #[error("本地资产包含未同步修改，禁止自动覆盖")]
    LocalChanges,
    #[error("资产缺少可用的 Base 快照，禁止猜测覆盖")]
    MissingBaseSnapshot,
    #[error("资产内容来源当前不可用：{0}")]
    SourceUnavailable(String),
    #[error("资产尚未配置私有 Overlay")]
    OverlayNotConfigured,
    #[error("资产上游不匹配")]
    UpstreamMismatch,
    #[error("资产操作已处于不兼容状态：{0}")]
    InvalidState(String),
    #[error("资产元数据无效：{0}")]
    InvalidMetadata(String),
    #[error("资产运行时不支持：{message}")]
    RuntimeProjectionUnsupported { code: &'static str, message: String },
    #[error("资产运行时投影失败：{message}")]
    RuntimeProjectionFailed { code: &'static str, message: String },
    #[error("原子 Bundle 约束失败：{0}")]
    BundleInvariant(String),
    #[error("资产对象损坏：{0}")]
    CorruptObject(PathBuf),
    #[error(transparent)]
    Database(#[from] tjuaeui_db::DbError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Crypto(#[from] tjuaeui_common::CryptoError),
}
