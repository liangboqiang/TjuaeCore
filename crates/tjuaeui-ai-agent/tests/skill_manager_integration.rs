//! Integration tests for the skill system.
//!
//! These tests verify the full skill lifecycle:
//! - Skill discovery across multiple directories
//! - Skill index generation
//! - Lazy loading of skill bodies
//! - LOAD_SKILL detection in agent output
//! - System instruction building
//! - First message preparation

use std::fs;
use std::sync::Arc;

use tempfile::TempDir;
use tjuaeui_ai_agent::{
    AcpSkillManager, build_skills_index_text, build_system_instructions, detect_skill_load_request,
    prepare_first_message, prepare_first_message_with_skills_index,
};
use tjuaeui_asset::resolve_skill_paths;

// ---------------------------------------------------------------------------
// 4.0 New API: discover via extension service
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_skills_uses_extension_service_layout() {
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();

    let user_dir = data_dir.join("skills").join("my-skill");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(
        user_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: User skill\n---\nBody",
    )
    .unwrap();

    let paths = Arc::new(resolve_skill_paths(tmp.path(), &data_dir));
    let mgr = AcpSkillManager::new(paths);

    // Local skills are explicit: no selection yields an empty catalog.
    let idx = mgr.discover_skills(None).await;
    assert!(idx.is_empty());

    let enabled = vec!["my-skill".to_string()];
    let idx = mgr.discover_skills(Some(&enabled)).await;
    assert_eq!(
        idx.iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>(),
        ["my-skill"]
    );
}

#[tokio::test]
async fn get_skill_loads_custom_body_via_fs_read() {
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("data");
    let user_skill = data_dir.join("skills").join("mine");
    fs::create_dir_all(&user_skill).unwrap();
    fs::write(
        user_skill.join("SKILL.md"),
        "---\nname: mine\ndescription: Mine\n---\nCustom body here",
    )
    .unwrap();

    let paths = Arc::new(resolve_skill_paths(tmp.path(), &data_dir));
    let mgr = AcpSkillManager::new(paths);
    let enabled = vec!["mine".to_string()];
    let idx = mgr.discover_skills(Some(&enabled)).await;

    let names: Vec<&str> = idx.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"mine"),
        "custom 'mine' should be in index; got {names:?}"
    );

    let skill = mgr.get_skill("mine").await.unwrap();
    assert_eq!(skill.body.as_deref(), Some("Custom body here"));
}

// ---------------------------------------------------------------------------
// 5.2 Skill Index (pure function)
// ---------------------------------------------------------------------------

#[test]
fn build_index_text_contains_load_protocol() {
    let skills = vec![
        tjuaeui_ai_agent::SkillIndex {
            name: "security".into(),
            description: "Security review".into(),
        },
        tjuaeui_ai_agent::SkillIndex {
            name: "tdd".into(),
            description: "Test-driven development".into(),
        },
    ];
    let text = build_skills_index_text(&skills);

    assert!(text.contains("[LOAD_SKILL: skill-name]"));
    assert!(text.contains("- **security**: Security review"));
    assert!(text.contains("- **tdd**: Test-driven development"));
}

// ---------------------------------------------------------------------------
// 5.4 LOAD_SKILL Detection (pure function)
// ---------------------------------------------------------------------------

#[test]
fn detect_single_load_skill_request() {
    let content = "I need to use [LOAD_SKILL: security-review] to check this code.";
    let skills = detect_skill_load_request(content);
    assert_eq!(skills, vec!["security-review"]);
}

#[test]
fn detect_multiple_load_skill_requests() {
    let content = "[LOAD_SKILL: a] then [LOAD_SKILL: b] and [LOAD_SKILL: c]";
    let skills = detect_skill_load_request(content);
    assert_eq!(skills, vec!["a", "b", "c"]);
}

#[test]
fn detect_no_load_skill_in_normal_text() {
    let content = "This is just normal text without any skill requests.";
    let skills = detect_skill_load_request(content);
    assert!(skills.is_empty());
}

#[test]
fn detect_load_skill_handles_whitespace() {
    let content = "[LOAD_SKILL:   spaced-name   ]";
    let skills = detect_skill_load_request(content);
    assert_eq!(skills, vec!["spaced-name"]);
}

// ---------------------------------------------------------------------------
// System instruction and first message builders
// ---------------------------------------------------------------------------

#[test]
fn system_instructions_with_loaded_skills() {
    let skills = vec![tjuaeui_ai_agent::SkillDefinition {
        name: "helper".into(),
        description: "A helper".into(),
        location: std::path::PathBuf::new(),
        source: tjuaeui_asset::SkillSource::Managed,
        body: Some("Complete helper instructions.".into()),
    }];
    let result = build_system_instructions("Base system prompt", &skills);

    assert!(result.starts_with("Base system prompt"));
    assert!(result.contains("## 技能：helper"));
    assert!(result.contains("Complete helper instructions."));
}

#[test]
fn first_message_with_skills_index_for_acp() {
    let skills = vec![tjuaeui_ai_agent::SkillIndex {
        name: "review".into(),
        description: "Code review".into(),
    }];
    let result = prepare_first_message_with_skills_index("Please review my code.", &skills, None);

    assert!(result.contains("[Assistant Rules]"));
    assert!(result.contains("- **review**: Code review"));
    assert!(result.contains("[/Assistant Rules]"));
    assert!(result.ends_with("Please review my code."));
}

#[test]
fn first_message_with_full_skills_for_gemini() {
    let skills = vec![tjuaeui_ai_agent::SkillDefinition {
        name: "debug".into(),
        description: "Debug".into(),
        location: std::path::PathBuf::new(),
        source: tjuaeui_asset::SkillSource::Managed,
        body: Some("Full debug skill content.".into()),
    }];
    let result = prepare_first_message("Hello", &skills, Some("Be helpful."));

    assert!(result.contains("[Assistant Rules]"));
    assert!(result.contains("Be helpful."));
    assert!(result.contains("Full debug skill content."));
    assert!(result.contains("[/Assistant Rules]"));
    assert!(result.ends_with("Hello"));
}

// Local-skill discovery and lazy body loading are covered above.
