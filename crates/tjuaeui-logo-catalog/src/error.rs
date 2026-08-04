/// Static asset-domain error used below the HTTP boundary.
#[derive(Debug)]
pub enum LogoAssetError {
    NotFound(String),
    Forbidden(String),
    Internal(String),
}

impl std::fmt::Display for LogoAssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) => write!(f, "未找到：{message}"),
            Self::Forbidden(message) => write!(f, "禁止访问：{message}"),
            Self::Internal(message) => write!(f, "内部错误：{message}"),
        }
    }
}

impl std::error::Error for LogoAssetError {}
