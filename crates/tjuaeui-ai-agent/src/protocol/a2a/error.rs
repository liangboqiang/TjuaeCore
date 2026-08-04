#[derive(Debug, thiserror::Error)]
pub(crate) enum A2aProtocolError {
    #[error("Agent Card 不是有效的 JSON：{0}")]
    InvalidJson(String),
    #[error("Agent Card 格式无效：{0}")]
    InvalidCard(String),
    #[error("A2A 协议版本不受支持：{0}")]
    UnsupportedVersion(String),
    #[error("A2A 协议绑定不受支持：{0}")]
    UnsupportedBinding(String),
    #[error("A2A 必需扩展不受支持：{0}")]
    UnsupportedRequiredExtension(String),
    #[error("A2A v0.3 Agent Card 需要显式开启兼容模式")]
    V03CompatibilityRequired,
    #[error("A2A 上游请求失败：{0}")]
    Upstream(String),
}

impl From<a2a::A2AError> for A2aProtocolError {
    fn from(error: a2a::A2AError) -> Self {
        Self::Upstream(error.message)
    }
}
