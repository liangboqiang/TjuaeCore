use serde::{Deserialize, Serialize};

/// Runtime ownership of a locally installed skill.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSourceResponse {
    Managed,
    Cron,
    Asset,
}

/// Single item in the locally available skills list (`GET /api/skills`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillListItemResponse {
    pub name: String,
    pub description: String,
    pub location: String,
    pub is_custom: bool,
    pub source: SkillSourceResponse,
}

/// Request body for assistant-rule reads.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadAssistantRuleRequest {
    pub assistant_id: String,
    #[serde(default)]
    pub locale: Option<String>,
}

/// Request body for `POST /api/skills/materialize-for-agent`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializeSkillsRequest {
    pub conversation_id: String,
    #[serde(default)]
    pub skills: Vec<String>,
}

/// One resolved runtime skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MaterializedSkillRef {
    pub name: String,
    /// Absolute path on disk to the locally installed skill directory.
    pub source_path: String,
}

/// Response for `POST /api/skills/materialize-for-agent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MaterializeSkillsResponse {
    pub skills: Vec<MaterializedSkillRef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skill_list_item_uses_stable_snake_case_contract() {
        let item = SkillListItemResponse {
            name: "review".into(),
            description: "Review".into(),
            location: "/tmp/skills/review".into(),
            is_custom: true,
            source: SkillSourceResponse::Asset,
        };
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["is_custom"], true);
        assert!(value.get("isCustom").is_none());
        assert_eq!(value["source"], "asset");
    }

    #[test]
    fn materialize_request_rejects_removed_enabled_skills_field() {
        let request: MaterializeSkillsRequest = serde_json::from_value(json!({
            "conversation_id": "conv-abc",
            "skills": ["review"]
        }))
        .unwrap();
        assert_eq!(request.skills, vec!["review"]);

        let removed = serde_json::from_value::<MaterializeSkillsRequest>(json!({
            "conversation_id": "conv-abc",
            "enabled_skills": ["review"]
        }));
        assert!(removed.is_err());
    }

    #[test]
    fn materialize_response_uses_snake_case() {
        let response = MaterializeSkillsResponse {
            skills: vec![MaterializedSkillRef {
                name: "review".into(),
                source_path: "/tmp/skills/review".into(),
            }],
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["skills"][0]["source_path"], "/tmp/skills/review");
        assert!(value["skills"][0].get("sourcePath").is_none());
    }

    #[test]
    fn assistant_rule_requests_support_locale() {
        let read: ReadAssistantRuleRequest =
            serde_json::from_value(json!({"assistant_id": "assistant-1", "locale": "zh-CN"})).unwrap();
        assert_eq!(read.locale.as_deref(), Some("zh-CN"));
    }
}
