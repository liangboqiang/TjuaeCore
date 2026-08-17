use std::fs;
use std::sync::Arc;

use tempfile::TempDir;
use tjuaeui_ai_agent::{
    AcpSkillManager, SkillDefinition, SkillIndex, build_skills_index_text, build_system_instructions,
    detect_skill_load_request, prepare_first_message, prepare_first_message_with_skills_index,
};
use tjuaeui_extension::resolve_skill_paths;

fn write_skill(root: &std::path::Path, slug: &str, auto_inject: bool, body: &str) {
    let directory = root.join("skills").join(slug);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(".tjuae-skill.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": "https://raw.githubusercontent.com/liangboqiang/TjuaeHub/main/schemas/tjuae-skill.v1.schema.json",
            "schemaVersion": 1,
            "id": slug,
            "version": "1.0.0",
            "categories": ["test"],
            "enabled": true,
            "autoInject": auto_inject,
            "source": { "kind": "local" }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {slug}\ndescription: {slug} description\n---\n{body}"),
    )
    .unwrap();
}

#[tokio::test]
async fn discovers_only_canonical_packages_and_respects_preferences() {
    let temp = TempDir::new().unwrap();
    write_skill(temp.path(), "automatic", true, "automatic body");
    write_skill(temp.path(), "optional", false, "optional body");
    let paths = Arc::new(resolve_skill_paths(temp.path(), temp.path()));
    let manager = AcpSkillManager::new(paths);

    let automatic = manager.discover_skills(None, None).await;
    assert_eq!(
        automatic.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(),
        ["automatic"]
    );

    let optional = manager
        .discover_skills(Some(&["optional".to_owned()]), Some(&["automatic".to_owned()]))
        .await;
    assert_eq!(
        optional.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(),
        ["optional"]
    );
    assert_eq!(
        manager.get_skill("optional").await.unwrap().body.as_deref(),
        Some("optional body")
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
