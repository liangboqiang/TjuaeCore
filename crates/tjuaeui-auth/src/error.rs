/// Authentication-layer errors.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("凭据无效")]
    InvalidCredentials,

    #[error("密码校验失败：{0}")]
    WeakPassword(String),

    #[error("用户名校验失败：{0}")]
    InvalidUsername(String),

    #[error("令牌已过期")]
    TokenExpired,

    #[error("令牌无效：{0}")]
    TokenInvalid(String),

    #[error("令牌已列入黑名单")]
    TokenBlacklisted,

    #[error("请求频率超过限制")]
    RateLimited,

    #[error("密码哈希错误：{0}")]
    HashError(String),
}
