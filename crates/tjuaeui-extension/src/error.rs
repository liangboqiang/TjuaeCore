/// Extension system domain errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("Manifest 校验失败：{0}")]
    ManifestValidation(String),

    #[error("扩展名“{name}”使用了保留前缀“{prefix}”")]
    ReservedNamePrefix { name: String, prefix: String },

    #[error("版本“{version}”无效：{reason}")]
    InvalidVersion { version: String, reason: String },

    #[error("环境变量未定义：{0}")]
    UndefinedEnvVariable(String),

    #[error("找不到文件引用：{0}")]
    FileReferenceNotFound(String),

    #[error("检测到路径穿越：{0}")]
    PathTraversal(String),

    #[error("引擎不兼容：扩展“{name}”要求 tjuaeui {required}，当前为 {actual}")]
    EngineIncompatible {
        name: String,
        required: String,
        actual: String,
    },

    #[error("API 版本不兼容：扩展“{name}”要求 API {required}，当前支持 {supported}")]
    ApiVersionIncompatible {
        name: String,
        required: String,
        supported: String,
    },

    #[error("WebUI 路由“{route}”必须位于“/{extension_name}/”命名空间下")]
    InvalidWebuiRouteNamespace { extension_name: String, route: String },

    #[error("WebUI 路由“{route}”使用了保留前缀“{prefix}”")]
    ReservedWebuiRoute { route: String, prefix: String },

    #[error("找不到主题 CSS 文件：{0}")]
    ThemeCssNotFound(String),

    #[error("解析扩展“{extension_name}”的贡献点失败：{reason}")]
    ResolutionFailed { extension_name: String, reason: String },

    #[error("扩展“{extension_name}”的生命周期钩子“{hook}”在 {timeout_secs} 秒后超时")]
    HookTimeout {
        extension_name: String,
        hook: String,
        timeout_secs: u64,
    },

    #[error("扩展“{extension_name}”的生命周期钩子“{hook}”失败：{reason}")]
    HookFailed {
        extension_name: String,
        hook: String,
        reason: String,
    },

    #[error("找不到生命周期钩子脚本：{0}")]
    HookNotFound(String),

    #[error("找不到扩展：{0}")]
    NotFound(String),

    #[error("状态持久化失败：{0}")]
    StatePersistence(String),

    #[error("找不到技能：{0}")]
    SkillNotFound(String),

    #[error("技能路径无效：{0}")]
    InvalidSkillPath(String),

    #[error("技能 frontmatter 无效：{0}")]
    SkillInvalidFrontmatter(String),

    #[error("找不到技能目录：{0}")]
    SkillImportNoSkillFound(String),

    #[error("技能导入来源无效：{0}")]
    SkillImportInvalidSource(String),

    #[error("{0}")]
    Db(#[from] tjuaeui_db::DbError),

    #[error("请求无效：{0}")]
    InvalidRequest(String),

    #[error("扩展内部错误：{0}")]
    Internal(String),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("技能压缩包无效：{0}")]
    Zip(#[from] zip::result::ZipError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_validation_error_display() {
        let err = ExtensionError::ManifestValidation("name is required".into());
        assert_eq!(err.to_string(), "Manifest 校验失败：name is required");
    }

    #[test]
    fn test_reserved_name_prefix_error_display() {
        let err = ExtensionError::ReservedNamePrefix {
            name: "tjuae-test".into(),
            prefix: "tjuae-".into(),
        };
        assert_eq!(err.to_string(), "扩展名“tjuae-test”使用了保留前缀“tjuae-”");
    }

    #[test]
    fn test_invalid_version_error_display() {
        let err = ExtensionError::InvalidVersion {
            version: "not-semver".into(),
            reason: "unexpected character".into(),
        };
        assert_eq!(err.to_string(), "版本“not-semver”无效：unexpected character");
    }

    #[test]
    fn test_undefined_env_variable_error_display() {
        let err = ExtensionError::UndefinedEnvVariable("MY_SECRET".into());
        assert_eq!(err.to_string(), "环境变量未定义：MY_SECRET");
    }

    #[test]
    fn test_file_reference_not_found_error_display() {
        let err = ExtensionError::FileReferenceNotFound("prompts/system.md".into());
        assert_eq!(err.to_string(), "找不到文件引用：prompts/system.md");
    }

    #[test]
    fn test_path_traversal_error_display() {
        let err = ExtensionError::PathTraversal("../../etc/passwd".into());
        assert_eq!(err.to_string(), "检测到路径穿越：../../etc/passwd");
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = ExtensionError::from(io_err);
        assert!(matches!(err, ExtensionError::Io(_)));
    }
}
