use tjuaeui_common::CryptoError;
use tjuaeui_db::DbError;

/// Crate-owned error contract for system domain services.
#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error("未找到：{0}")]
    NotFound(String),

    #[error("请求无效：{0}")]
    BadRequest(String),

    #[error("发生冲突：{0}")]
    Conflict(String),

    #[error("内部错误：{0}")]
    Internal(String),

    #[error("上游网关错误：{0}")]
    BadGateway(String),

    #[error("请求超时：{0}")]
    Timeout(String),

    #[error("请求内容无法处理：{0}")]
    UnprocessableEntity(String),
}

impl From<DbError> for SystemError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::NotFound(reason) => Self::NotFound(reason),
            DbError::Conflict(reason) => Self::Conflict(reason),
            DbError::Query(e) => Self::Internal(format!("数据库错误：{e}")),
            DbError::Migration(e) => Self::Internal(format!("数据库迁移错误：{e}")),
            DbError::Init(reason) => Self::Internal(format!("数据库初始化错误：{reason}")),
        }
    }
}

impl From<CryptoError> for SystemError {
    fn from(error: CryptoError) -> Self {
        if error.is_bad_request() {
            Self::BadRequest(error.to_string())
        } else {
            Self::Internal(error.to_string())
        }
    }
}
