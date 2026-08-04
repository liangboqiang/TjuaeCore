//! Black-box integration tests for retained runtime skill behavior.

use tempfile::TempDir;
use tjuaeui_asset::{materialize_skills_for_agent, resolve_skill_paths};

fn create_skill(base: &std::path::Path, name: &str) {
    let directory = base.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Runtime skill\n---\nBody"),
    )
    .unwrap();
}

#[test]
fn resolve_skill_paths_contains_only_runtime_projection_roots() {
    let data_dir = std::path::Path::new("/tmp/tjuae");
    let paths = resolve_skill_paths(data_dir, data_dir);
    assert_eq!(paths.user_skills_dir, data_dir.join("skills"));
    assert_eq!(paths.cron_skills_dir, data_dir.join("cron").join("skills"));
}

#[tokio::test]
async fn materialize_resolves_requested_runtime_skills_only() {
    let temp = TempDir::new().unwrap();
    let paths = resolve_skill_paths(temp.path(), temp.path());
    create_skill(&paths.user_skills_dir, "review");
    create_skill(&paths.user_skills_dir, "unused");

    let resolved = materialize_skills_for_agent(&paths, "conversation-1", &["review".into()])
        .await
        .unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "review");
    assert_eq!(resolved[0].source_path, paths.user_skills_dir.join("review"));
}

#[tokio::test]
async fn materialize_rejects_conversation_path_traversal() {
    let temp = TempDir::new().unwrap();
    let paths = resolve_skill_paths(temp.path(), temp.path());
    let result = materialize_skills_for_agent(&paths, "../escape", &[]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn materialize_ignores_invalid_or_missing_skill_names() {
    let temp = TempDir::new().unwrap();
    let paths = resolve_skill_paths(temp.path(), temp.path());
    let resolved = materialize_skills_for_agent(&paths, "conversation-1", &["../escape".into(), "missing".into()])
        .await
        .unwrap();
    assert!(resolved.is_empty());
}
