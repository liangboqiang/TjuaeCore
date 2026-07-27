#[derive(Debug, thiserror::Error)]
pub enum OfficeError {
    #[error("输入输出错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("快照错误：{0}")]
    Snapshot(String),

    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),

    #[error("转换错误：{0}")]
    Conversion(String),

    #[error("找不到外部工具：{0}")]
    ToolNotFound(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_messages() {
        assert_eq!(
            OfficeError::Conversion("bad data".into()).to_string(),
            "转换错误：bad data"
        );
        assert_eq!(
            OfficeError::ToolNotFound("pandoc".into()).to_string(),
            "找不到外部工具：pandoc"
        );
    }
}
