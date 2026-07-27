#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("找不到文件：{0}")]
    FileNotFound(String),

    #[error("找不到目录：{0}")]
    DirectoryNotFound(String),

    #[error("URL 无效：{0}")]
    InvalidUrl(String),

    #[error("工具未安装：{0}")]
    ToolNotInstalled(String),

    #[error("命令执行失败：{0}")]
    CommandFailed(String),

    #[error("输入输出错误：{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("语音转文字未启用")]
    Disabled,

    #[error("OpenAI 语音转文字未配置：缺少 API 密钥")]
    OpenaiNotConfigured,

    #[error("Deepgram 语音转文字未配置：缺少 API 密钥")]
    DeepgramNotConfigured,

    #[error("语音转文字请求失败：{0}")]
    RequestFailed(String),

    #[error("语音转文字发生未知错误：{0}")]
    Unknown(String),

    #[error("当前模型或端点不支持流式语音转文字")]
    StreamUnsupported,

    #[error("语音转文字流协议错误：{0}")]
    StreamProtocol(String),
}

impl SttError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Disabled => "STT_DISABLED",
            Self::OpenaiNotConfigured => "STT_OPENAI_NOT_CONFIGURED",
            Self::DeepgramNotConfigured => "STT_DEEPGRAM_NOT_CONFIGURED",
            Self::RequestFailed(_) => "STT_REQUEST_FAILED",
            Self::Unknown(_) => "STT_UNKNOWN",
            Self::StreamUnsupported => "STT_STREAM_UNSUPPORTED",
            Self::StreamProtocol(_) => "STT_STREAM_PROTOCOL",
        }
    }

    pub fn status_code(&self) -> u16 {
        match self {
            Self::Disabled | Self::OpenaiNotConfigured | Self::DeepgramNotConfigured => 400,
            Self::RequestFailed(_) => 502,
            Self::Unknown(_) => 500,
            Self::StreamUnsupported | Self::StreamProtocol(_) => 400,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_error_display_messages() {
        assert_eq!(
            ShellError::FileNotFound("/a.txt".into()).to_string(),
            "找不到文件：/a.txt"
        );
        assert_eq!(
            ShellError::DirectoryNotFound("/dir".into()).to_string(),
            "找不到目录：/dir"
        );
        assert_eq!(ShellError::InvalidUrl("bad".into()).to_string(), "URL 无效：bad");
        assert_eq!(
            ShellError::ToolNotInstalled("code".into()).to_string(),
            "工具未安装：code"
        );
        assert_eq!(
            ShellError::CommandFailed("oops".into()).to_string(),
            "命令执行失败：oops"
        );
    }

    #[test]
    fn stt_error_codes() {
        assert_eq!(SttError::Disabled.error_code(), "STT_DISABLED");
        assert_eq!(SttError::OpenaiNotConfigured.error_code(), "STT_OPENAI_NOT_CONFIGURED");
        assert_eq!(
            SttError::DeepgramNotConfigured.error_code(),
            "STT_DEEPGRAM_NOT_CONFIGURED"
        );
        assert_eq!(SttError::RequestFailed("x".into()).error_code(), "STT_REQUEST_FAILED");
        assert_eq!(SttError::Unknown("x".into()).error_code(), "STT_UNKNOWN");
        assert_eq!(SttError::StreamUnsupported.error_code(), "STT_STREAM_UNSUPPORTED");
        assert_eq!(
            SttError::StreamProtocol("bad frame".into()).error_code(),
            "STT_STREAM_PROTOCOL"
        );
    }

    #[test]
    fn stt_status_codes() {
        assert_eq!(SttError::Disabled.status_code(), 400);
        assert_eq!(SttError::OpenaiNotConfigured.status_code(), 400);
        assert_eq!(SttError::DeepgramNotConfigured.status_code(), 400);
        assert_eq!(SttError::RequestFailed("x".into()).status_code(), 502);
        assert_eq!(SttError::Unknown("x".into()).status_code(), 500);
        assert_eq!(SttError::StreamUnsupported.status_code(), 400);
        assert_eq!(SttError::StreamProtocol("x".into()).status_code(), 400);
    }

    #[test]
    fn stt_error_display_messages() {
        assert_eq!(SttError::Disabled.to_string(), "语音转文字未启用");
        assert_eq!(
            SttError::OpenaiNotConfigured.to_string(),
            "OpenAI 语音转文字未配置：缺少 API 密钥"
        );
        assert_eq!(
            SttError::DeepgramNotConfigured.to_string(),
            "Deepgram 语音转文字未配置：缺少 API 密钥"
        );
        assert_eq!(
            SttError::RequestFailed("timeout".into()).to_string(),
            "语音转文字请求失败：timeout"
        );
        assert_eq!(
            SttError::Unknown("oops".into()).to_string(),
            "语音转文字发生未知错误：oops"
        );
        assert_eq!(
            SttError::StreamUnsupported.to_string(),
            "当前模型或端点不支持流式语音转文字"
        );
        assert_eq!(
            SttError::StreamProtocol("unexpected frame".into()).to_string(),
            "语音转文字流协议错误：unexpected frame"
        );
    }
}
