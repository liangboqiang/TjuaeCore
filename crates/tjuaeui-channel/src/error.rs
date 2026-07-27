/// Channel crate-level errors.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("找不到插件：{0}")]
    PluginNotFound(String),

    #[error("插件类型无效：{0}")]
    InvalidPluginType(String),

    #[error("插件已在运行：{0}")]
    PluginAlreadyRunning(String),

    #[error("插件配置无效：{0}")]
    InvalidConfig(String),

    #[error("插件连接失败：{0}")]
    ConnectionFailed(String),

    #[error("找不到配对码：{0}")]
    PairingNotFound(String),

    #[error("配对码已过期：{0}")]
    PairingExpired(String),

    #[error("配对码已处理：{0}")]
    PairingAlreadyProcessed(String),

    #[error("找不到用户：{0}")]
    UserNotFound(String),

    #[error("用户未获授权：{0}")]
    UserNotAuthorized(String),

    #[error("找不到会话：{0}")]
    SessionNotFound(String),

    #[error("凭据加密失败：{0}")]
    EncryptionFailed(String),

    #[error("凭据解密失败：{0}")]
    DecryptionFailed(String),

    #[error("平台 API 错误：{0}")]
    PlatformApi(String),

    #[error("消息发送失败：{0}")]
    MessageSendFailed(String),

    #[error("{0}")]
    Database(#[from] tjuaeui_db::DbError),

    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_messages() {
        assert_eq!(ChannelError::PluginNotFound("tg".into()).to_string(), "找不到插件：tg");
        assert_eq!(
            ChannelError::PairingExpired("123456".into()).to_string(),
            "配对码已过期：123456"
        );
        assert_eq!(
            ChannelError::InvalidConfig("bad".into()).to_string(),
            "插件配置无效：bad"
        );
    }
}
