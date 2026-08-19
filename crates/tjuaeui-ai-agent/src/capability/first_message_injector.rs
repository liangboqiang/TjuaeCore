//! Shared first-message prefix injection for ACP agents.
//!
//! Takes the conversation's first-message content and produces a new content
//! string that may include an `[Assistant Rules]` block with preset context
//! and a skills index. The shape depends on whether the agent's native CLI
//! can read skills from the workspace directly.

use std::sync::Arc;

use crate::capability::skill_manager::{AcpSkillManager, prepare_first_message_with_skills_index};

/// Configuration for the first-message injector.
pub struct InjectionConfig<'a> {
    /// Preset context (assistant-level system prompt injection).
    pub preset_context: Option<&'a str>,
    /// Resolved skill names (snapshot from `conversation.extra.skills`).
    pub skills: &'a [String],
    /// True iff the agent's native CLI reads skills from the workspace
    /// without needing prompt injection. Derived by callers from
    /// `AcpBackend::native_skills_dirs().is_some()` for ACP.
    pub native_skill_support: bool,
}

/// Produce the content string to send as the first ACP prompt.
///
/// - If `native_skill_support`: **light mode** — only `preset_context`
///   prepended as an `[Assistant Rules]` block (if present). The native CLI
///   handles skill discovery via workspace links.
/// - Else: **heavy mode** — `preset_context` + resolved skills index
///   injected via `prepare_first_message_with_skills_index`.
pub async fn inject_first_message_prefix(
    content: &str,
    manager: &Arc<AcpSkillManager>,
    config: InjectionConfig<'_>,
) -> String {
    if config.native_skill_support {
        return match config.preset_context {
            Some(ctx) if !ctx.is_empty() => {
                format!("[Assistant Rules]\n{ctx}\n[/Assistant Rules]\n\n{content}")
            }
            _ => content.to_string(),
        };
    }

    let skills = manager.discover_by_names(config.skills).await;
    let has_context = config.preset_context.is_some_and(|s| !s.is_empty());
    if skills.is_empty() && !has_context {
        return content.to_string();
    }
    prepare_first_message_with_skills_index(content, &skills, config.preset_context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tempfile::TempDir;
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

        async fn commit_workspace_snapshot(
            &self,
            _workspace: &std::path::Path,
            _message: &str,
        ) -> Result<String, String> {
            Ok("test".to_owned())
        }
    }

    fn test_mgr(base: &std::path::Path) -> Arc<AcpSkillManager> {
        let paths = Arc::new(resolve_skill_paths(base, base));
        AcpSkillManager::new(paths)
    }

    #[tokio::test]
    async fn light_mode_with_preset_context() {
        let tmp = TempDir::new().unwrap();
        let mgr = test_mgr(tmp.path());

        let out = inject_first_message_prefix(
            "Hello",
            &mgr,
            InjectionConfig {
                preset_context: Some("Be concise."),
                skills: &[],
                native_skill_support: true,
            },
        )
        .await;

        assert!(out.contains("[Assistant Rules]"));
        assert!(out.contains("Be concise."));
        assert!(out.ends_with("Hello"));
    }

    #[tokio::test]
    async fn light_mode_empty_context_passes_through() {
        let tmp = TempDir::new().unwrap();
        let mgr = test_mgr(tmp.path());

        let out = inject_first_message_prefix(
            "Hello",
            &mgr,
            InjectionConfig {
                preset_context: None,
                skills: &[],
                native_skill_support: true,
            },
        )
        .await;
        assert_eq!(out, "Hello");
    }

    #[tokio::test]
    async fn heavy_mode_no_skills_no_context_passes_through() {
        let tmp = TempDir::new().unwrap();
        let mgr = test_mgr(tmp.path());

        let out = inject_first_message_prefix(
            "Hello",
            &mgr,
            InjectionConfig {
                preset_context: None,
                skills: &[],
                native_skill_support: false,
            },
        )
        .await;
        assert_eq!(out, "Hello");
    }

    #[tokio::test]
    async fn heavy_mode_with_preset_context_no_skills() {
        let tmp = TempDir::new().unwrap();
        let mgr = test_mgr(tmp.path());

        let out = inject_first_message_prefix(
            "Go.",
            &mgr,
            InjectionConfig {
                preset_context: Some("Rule 1."),
                skills: &[],
                native_skill_support: false,
            },
        )
        .await;

        assert!(out.contains("[Assistant Rules]"));
        assert!(out.contains("Rule 1."));
        assert!(out.ends_with("Go."));
    }

    #[tokio::test]
    async fn heavy_mode_with_resolved_skills_injects_index() {
        // Set up two canonical packages; pass only one in `skills`.
        let tmp = TempDir::new().unwrap();
        let skills = tmp.path().join("skills");
        let git: Arc<dyn WorkspaceGitProvisioner> = Arc::new(TestGit);
        create_skill(&skills, "cron", "cron", "Schedule stuff", git.clone())
            .await
            .unwrap();
        create_skill(&skills, "pdf", "pdf", "Render PDFs", git).await.unwrap();
        let mgr = test_mgr(tmp.path());

        let out = inject_first_message_prefix(
            "Hello",
            &mgr,
            InjectionConfig {
                preset_context: None,
                skills: &["cron".to_owned()],
                native_skill_support: false,
            },
        )
        .await;
        assert!(out.contains("cron"));
        assert!(!out.contains("pdf"));
        assert!(out.ends_with("Hello"));
    }

    #[tokio::test]
    async fn native_support_uses_light_mode_even_with_skills() {
        let tmp = TempDir::new().unwrap();
        let mgr = test_mgr(tmp.path());

        let out = inject_first_message_prefix(
            "Do stuff",
            &mgr,
            InjectionConfig {
                preset_context: Some("Custom rule"),
                skills: &["cron".to_owned()],
                native_skill_support: true,
            },
        )
        .await;

        assert!(out.contains("[Assistant Rules]"));
        assert!(out.contains("Custom rule"));
        assert!(!out.contains("Available Skills"));
        assert!(out.ends_with("Do stuff"));
    }
}
