use std::collections::BTreeMap;
use std::fmt::Write;

use crate::error::TeamError;
use crate::ports::TeamAssistantCatalogEntry;
use crate::service::TeamSessionService;

impl TeamSessionService {
    pub(crate) async fn describe_assistant(
        &self,
        assistant_id: &str,
        locale: Option<&str>,
    ) -> Result<String, TeamError> {
        let assistant = self.resolve_team_selectable_assistant(assistant_id).await?;
        render_assistant_description_json(&assistant, locale.unwrap_or("en-US")).map_err(TeamError::from)
    }
}

fn render_assistant_description_json(
    assistant: &TeamAssistantCatalogEntry,
    locale: &str,
) -> Result<String, serde_json::Error> {
    let name = localized_text(&assistant.name_i18n, &assistant.name, locale);
    let description = localized_text(&assistant.description_i18n, &assistant.description, locale);
    let example_tasks = localized_list(
        &assistant.recommended_prompts_i18n,
        &assistant.recommended_prompts,
        locale,
    );
    let description_markdown = render_assistant_description(assistant, locale);

    serde_json::to_string_pretty(&serde_json::json!({
        "status": "ok",
        "assistant_id": assistant.assistant_id,
        "name": name,
        "description": description,
        "description_markdown": description_markdown,
        "skills": assistant.skills,
        "example_tasks": example_tasks,
        "default_model": assistant.model,
    }))
}

fn render_assistant_description(assistant: &TeamAssistantCatalogEntry, locale: &str) -> String {
    let name = localized_text(&assistant.name_i18n, &assistant.name, locale);
    let description = localized_text(&assistant.description_i18n, &assistant.description, locale);
    let example_tasks = localized_list(
        &assistant.recommended_prompts_i18n,
        &assistant.recommended_prompts,
        locale,
    );

    let mut out = String::new();
    let _ = writeln!(out, "# {} (`{}`)", name, assistant.assistant_id);
    let _ = writeln!(out);
    let _ = writeln!(out, "Backend: {}", assistant.backend);
    let _ = writeln!(out);
    let _ = writeln!(out, "## 说明\n{description}\n");
    let _ = writeln!(out, "## 技能");
    render_list(&mut out, &assistant.skills);
    let _ = writeln!(out, "\n## 示例任务");
    render_list(&mut out, &example_tasks);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Use `team_spawn_agent` with `assistant_id=\"{}\"`.",
        assistant.assistant_id
    );
    out.trim_end().to_owned()
}

fn render_list(out: &mut String, items: &[String]) {
    if items.is_empty() {
        let _ = writeln!(out, "- None");
    } else {
        for item in items {
            let _ = writeln!(out, "- {item}");
        }
    }
}

fn localized_text(map: &BTreeMap<String, String>, fallback: &str, locale: &str) -> String {
    map.get(locale)
        .or_else(|| map.get("en-US"))
        .cloned()
        .unwrap_or_else(|| fallback.to_owned())
}

fn localized_list(map: &BTreeMap<String, Vec<String>>, fallback: &[String], locale: &str) -> Vec<String> {
    map.get(locale)
        .or_else(|| map.get("en-US"))
        .cloned()
        .unwrap_or_else(|| fallback.to_vec())
}
