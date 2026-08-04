use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("找不到团队：{0}")]
    TeamNotFound(String),

    #[error("找不到智能体：{0}")]
    AgentNotFound(String),

    #[error("找不到任务：{0}")]
    TaskNotFound(String),

    #[error("请求无效：{0}")]
    InvalidRequest(String),

    #[error("该操作仅限团队负责人：{0}")]
    LeaderOnly(String),

    #[error("无权访问：{0}")]
    Forbidden(String),

    #[error("找不到会话：{0}")]
    SessionNotFound(String),

    #[error("找不到依赖任务：{0}")]
    BlockedTaskNotFound(String),

    #[error("不允许使用该后端：{0}")]
    BackendNotAllowed(String),

    #[error("智能体名称已被占用：{0}")]
    DuplicateAgentName(String),

    #[error("该对话的团队智能体运行时尚未就绪：{conversation_id}")]
    RuntimeNotReady { conversation_id: String },

    #[error("队员运行时失败：{slot_id}")]
    MemberRuntimeFailed {
        team_id: String,
        slot_id: String,
        conversation_id: String,
        public_reason: String,
    },

    #[error("工作区路径不可用：{0}")]
    WorkspacePathUnavailable(String),

    #[error("执行期间工作区路径不可用：{0}")]
    WorkspacePathRuntimeUnavailable(String),

    #[error("{0}")]
    Database(#[from] tjuaeui_db::DbError),

    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeamPublicError {
    pub code: &'static str,
    pub details: Option<Value>,
}

impl TeamPublicError {
    fn new(code: &'static str, details: Option<Value>) -> Self {
        Self { code, details }
    }
}

pub fn classify_public_error(message: &str) -> Option<TeamPublicError> {
    if matches!(
        message,
        "缺少必填字段：assistant_id"
            | "spawn_agent.assistant_id 为必填项"
            | "当调用者对话未绑定助手时，assistant_id 为必填项"
    ) {
        return Some(TeamPublicError::new(
            "TEAM_ASSISTANT_ID_REQUIRED",
            Some(json!({ "field": "assistant_id" })),
        ));
    }

    if let Some(assistant_id) = message.strip_prefix("找不到预设助手：") {
        return Some(TeamPublicError::new(
            "TEAM_ASSISTANT_NOT_FOUND",
            Some(json!({ "assistant_id": assistant_id })),
        ));
    }

    for field in ["backend", "agent_type", "custom_agent_id"] {
        if message == format!("{field} 不再接受；请使用 assistant_id") {
            return Some(TeamPublicError::new(
                "TEAM_ASSISTANT_FIELD_UNSUPPORTED",
                Some(json!({
                    "field": field,
                    "required_field": "assistant_id",
                })),
            ));
        }
    }

    if message == "model 不再接受；请使用助手配置或 UI 模型选择器" {
        return Some(TeamPublicError::new(
            "TEAM_ASSISTANT_FIELD_UNSUPPORTED",
            Some(json!({
                "field": "model",
                "required_field": "assistant_id",
            })),
        ));
    }

    if message == "team_list_assistants 不接受参数" {
        return Some(TeamPublicError::new(
            "TEAM_TOOL_ARGUMENTS_NOT_ALLOWED",
            Some(json!({
                "tool": "team_list_assistants",
            })),
        ));
    }

    if message == "团队服务不可用" {
        return Some(TeamPublicError::new("TEAM_SERVICE_UNAVAILABLE", None));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_messages() {
        assert_eq!(TeamError::TeamNotFound("t1".into()).to_string(), "找不到团队：t1");
        assert_eq!(TeamError::AgentNotFound("s1".into()).to_string(), "找不到智能体：s1");
        assert_eq!(TeamError::TaskNotFound("tk1".into()).to_string(), "找不到任务：tk1");
    }

    #[test]
    fn classify_public_error_recognizes_branch_assistant_first_failures() {
        let required = classify_public_error("缺少必填字段：assistant_id").expect("classified");
        assert_eq!(required.code, "TEAM_ASSISTANT_ID_REQUIRED");
        assert_eq!(required.details, Some(json!({ "field": "assistant_id" })));

        let assistant = classify_public_error("找不到预设助手：bare:abcd1234").expect("assistant lookup");
        assert_eq!(assistant.code, "TEAM_ASSISTANT_NOT_FOUND");
        assert_eq!(assistant.details, Some(json!({ "assistant_id": "bare:abcd1234" })));

        let legacy = classify_public_error("backend 不再接受；请使用 assistant_id").expect("legacy field");
        assert_eq!(legacy.code, "TEAM_ASSISTANT_FIELD_UNSUPPORTED");
        assert_eq!(
            legacy.details,
            Some(json!({
                "field": "backend",
                "required_field": "assistant_id",
            }))
        );

        let model = classify_public_error("model 不再接受；请使用助手配置或 UI 模型选择器").expect("model field");
        assert_eq!(model.code, "TEAM_ASSISTANT_FIELD_UNSUPPORTED");
        assert_eq!(
            model.details,
            Some(json!({
                "field": "model",
                "required_field": "assistant_id",
            }))
        );
    }
}
