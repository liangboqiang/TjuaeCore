use std::path::{Path, PathBuf};

use tjuaeui_common::AgentType;

const MANAGED_RUNTIME_DIR: &str = "runtime";
const MANAGED_SKILLS_DIR: &str = "skills";
const CODEX_DIR: &str = "codex";

pub(crate) fn uses_managed_codex_skills(
    agent_type: &AgentType,
    backend: Option<&str>,
    is_custom_workspace: bool,
) -> bool {
    !is_custom_workspace && matches!(agent_type, AgentType::Acp) && backend == Some("codex")
}

pub(crate) fn managed_codex_skill_root(workspace_root: &Path, conversation_id: &str) -> PathBuf {
    workspace_root
        .join(MANAGED_RUNTIME_DIR)
        .join(MANAGED_SKILLS_DIR)
        .join(CODEX_DIR)
        .join(conversation_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_auto_codex_workspaces_use_managed_roots() {
        assert!(uses_managed_codex_skills(&AgentType::Acp, Some("codex"), false));
        assert!(!uses_managed_codex_skills(&AgentType::Acp, Some("codex"), true));
        assert!(!uses_managed_codex_skills(&AgentType::Acp, Some("claude"), false));
        assert!(!uses_managed_codex_skills(&AgentType::TjuaeCli, Some("codex"), false));
    }

    #[test]
    fn managed_root_is_outside_conversation_workspace_tree() {
        let root = managed_codex_skill_root(Path::new("/data"), "conv-1");
        assert_eq!(root, Path::new("/data/runtime/skills/codex/conv-1"));
    }
}
