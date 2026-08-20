use tjuaeui_db::DbError;

/// Assistant-domain error used below the HTTP boundary.
#[derive(Debug, thiserror::Error)]
pub enum AssistantError {
    #[error("未找到：{0}")]
    NotFound(String),

    #[error("请求无效：{0}")]
    BadRequest(String),

    #[error("无权访问：{0}")]
    Forbidden(String),

    #[error("发生冲突：{0}")]
    Conflict(String),

    #[error("内部错误：{0}")]
    Internal(String),
}

impl From<DbError> for AssistantError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::NotFound(message) => Self::NotFound(message),
            DbError::Conflict(message) => Self::Conflict(message),
            other => Self::Internal(other.to_string()),
        }
    }
}
