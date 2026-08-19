use std::sync::Arc;

use async_trait::async_trait;
use tempfile::TempDir;
use tjuaeui_ai_agent::{
    AcpSkillManager, SkillDefinition, SkillIndex, build_skills_index_text, build_system_instructions,
    detect_skill_load_request, prepare_first_message, prepare_first_message_with_skills_index,
};
use tjuaeui_common::{WorkspaceGitProvision, WorkspaceGitProvisioner};
use tjuaeui_extension::{create_skill, resolve_skill_paths};

struct TestGit;

#[async_trait]
impl WorkspaceGitProvisioner for TestGit {
    async fn ensure_workspace_git(&self, workspace: &std::path::Path) -> Result<WorkspaceGitProvision, String> {
        Ok(WorkspaceGitProvision {
            repository_root: workspace.display().to_string(),
            workspace_path: workspace.display().to_string(),
            branch: "main".to_owned(),
            head_commit: "test".to_owned(),
        })
    }

    async fn commit_workspace_snapshot(&self, _workspace: &std::path::Path, _message: &str) -> Result<String, String> {
        Ok("test".to_owned())
    }
}

#[tokio::test]
async fn discovers_only_skills_named_by_the_assistant_snapshot() {
    let temp = TempDir::new().unwrap();
    let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);
    create_skill(
        &temp.path().join("skills"),
        "automatic",
        "automatic",
        "automatic body",
        git.clone(),
    )
    .await
    .unwrap();
    create_skill(
        &temp.path().join("skills"),
        "optional",
        "optional",
        "optional body",
        git,
    )
    .await
    .unwrap();
    let paths = Arc::new(resolve_skill_paths(temp.path(), temp.path()));
    let manager = AcpSkillManager::new(paths);

    let optional = manager.discover_by_names(&["optional".to_owned()]).await;
    assert_eq!(
        optional.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(),
        ["optional"]
    );
    assert_eq!(
        manager.get_skill("optional").await.unwrap().body.as_deref(),
        Some("# optional\n\noptional body")
    );
}

#[test]
fn builders_and_load_protocol_use_slug() {
    let index = vec![SkillIndex {
        name: "review".into(),
        description: "Code review".into(),
    }];
    let index_text = build_skills_index_text(&index);
    assert!(index_text.contains("[LOAD_SKILL: skill-name]"));
    assert!(index_text.contains("- **review**: Code review"));
    assert_eq!(detect_skill_load_request("Use [LOAD_SKILL: review]"), ["review"]);

    let message = prepare_first_message_with_skills_index("Review this", &index, Some("Be concise."));
    assert!(message.contains("Be concise."));
    assert!(message.ends_with("Review this"));
}

#[test]
fn full_skill_body_uses_the_same_definition() {
    let skills = vec![SkillDefinition {
        name: "debug".into(),
        description: "Debug".into(),
        location: std::path::PathBuf::new(),
        body: Some("Full debug instructions.".into()),
    }];
    assert!(build_system_instructions("Base", &skills).contains("Full debug instructions."));
    assert!(prepare_first_message("Hello", &skills, None).ends_with("Hello"));
}
