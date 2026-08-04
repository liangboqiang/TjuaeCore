use crate::error::TeamError;
use crate::service::TeamSessionService;

impl TeamSessionService {
    /// 返回当前用户已经激活的团队助手详情。
    ///
    /// 详情来自用户级目录端口，避免再次按全局投影名称读取助手或猜测后端。
    pub(crate) async fn describe_assistant(
        &self,
        user_id: &str,
        assistant_id: &str,
        _locale: Option<&str>,
    ) -> Result<String, TeamError> {
        let assistant = self.resolve_team_selectable_assistant(user_id, assistant_id).await?;
        let skills = assistant.skills;
        let skills_markdown = if skills.is_empty() {
            "无".to_owned()
        } else {
            skills.join("、")
        };
        let description_markdown = format!(
            "# {}\n\n{}\n\n- 运行后端：{}\n- 技能：{}",
            assistant.name, assistant.description, assistant.backend, skills_markdown
        );

        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "status": "ok",
            "assistant_id": assistant.assistant_id,
            "name": assistant.name,
            "description": assistant.description,
            "description_markdown": description_markdown,
            "skills": skills,
            "example_tasks": [],
            "default_model": assistant.default_model,
        }))?)
    }
}
