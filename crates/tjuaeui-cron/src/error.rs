use tjuaeui_conversation::ConversationError;

#[derive(Debug, thiserror::Error)]
pub enum CronError {
    #[error("找不到定时任务：{0}")]
    JobNotFound(String),

    #[error("计划无效：{0}")]
    InvalidSchedule(String),

    #[error("Cron 表达式无效：{0}")]
    InvalidCronExpression(String),

    #[error("执行模式无效：{0}")]
    InvalidExecutionMode(String),

    #[error("created-by 值无效：{0}")]
    InvalidCreatedBy(String),

    #[error("任务状态无效：{0}")]
    InvalidJobStatus(String),

    #[error("时区无效：{0}")]
    InvalidTimezone(String),

    #[error("技能内容无效：{0}")]
    InvalidSkillContent(String),

    #[error("智能体配置无效：{0}")]
    InvalidAgentConfig(String),

    #[error("调度器错误：{0}")]
    Scheduler(String),

    #[error("工作区路径不可用：{0}")]
    WorkspacePathUnavailable(String),

    #[error("执行期间工作区路径不可用：{0}")]
    WorkspacePathRuntimeUnavailable(String),

    #[error(transparent)]
    Conversation(#[from] ConversationError),

    #[error("{0}")]
    Database(#[from] tjuaeui_db::DbError),

    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),
}

impl CronError {
    pub(crate) fn from_conversation_create(error: ConversationError) -> Self {
        match error {
            ConversationError::WorkspacePathUnavailable { path } => Self::WorkspacePathUnavailable(path),
            ConversationError::WorkspacePathRuntimeUnavailable { path } => Self::WorkspacePathRuntimeUnavailable(path),
            other => Self::Scheduler(format!("创建对话失败：{other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_create_preserves_workspace_error_code() {
        let err = CronError::from_conversation_create(ConversationError::WorkspacePathUnavailable {
            path: "/tmp/a b".into(),
        });
        assert!(matches!(err, CronError::WorkspacePathUnavailable(msg) if msg == "/tmp/a b"));
    }

    #[test]
    fn display_messages() {
        assert_eq!(
            CronError::JobNotFound("cron_1".into()).to_string(),
            "找不到定时任务：cron_1"
        );
        assert_eq!(CronError::InvalidSchedule("bad".into()).to_string(), "计划无效：bad");
        assert_eq!(
            CronError::InvalidCronExpression("* *".into()).to_string(),
            "Cron 表达式无效：* *"
        );
    }
}
