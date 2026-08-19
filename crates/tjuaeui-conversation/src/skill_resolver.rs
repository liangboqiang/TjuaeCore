//! Runtime resolution for skill references already captured on an assistant.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tjuaeui_api_types::SkillIdentityResponse;
use tjuaeui_common::WorkspaceGitProvisioner;
use tjuaeui_db::ISkillUserPreferenceRepository;
pub use tjuaeui_extension::ResolvedAgentSkill;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedAgentSkill {
    pub name: String,
    pub body: String,
}

#[async_trait]
pub trait SkillResolver: Send + Sync {
    /// Resolve each skill name to its on-disk source directory, using the
    /// same search order as `materialize_skills_for_agent`.
    async fn resolve_skills(&self, names: &[String]) -> Vec<ResolvedAgentSkill>;

    /// Load full skill bodies for prompt-protocol agents that request
    /// `[LOAD_SKILL: name]` in their response.
    async fn load_skill_bodies(&self, names: &[String]) -> Vec<LoadedAgentSkill> {
        let resolved = self.resolve_skills(names).await;
        load_resolved_skill_bodies(&resolved).await
    }

    /// Create symlinks pointing at each resolved skill inside the given
    /// workspace's per-backend native skills directories. `rel_dirs` is
    /// the list of relative paths (e.g. `.claude/skills`) to populate.
    /// Returns the number of symlinks successfully created.
    async fn link_workspace_skills(&self, workspace: &Path, rel_dirs: &[&str], skills: &[ResolvedAgentSkill]) -> usize;
}

/// Production adapter backed by the canonical local skill workspaces.
pub struct ExtensionSkillResolver {
    paths: Arc<tjuaeui_extension::SkillPaths>,
    preferences: Arc<dyn ISkillUserPreferenceRepository>,
    git: Arc<dyn WorkspaceGitProvisioner>,
}

impl ExtensionSkillResolver {
    pub fn new(
        paths: Arc<tjuaeui_extension::SkillPaths>,
        preferences: Arc<dyn ISkillUserPreferenceRepository>,
        git: Arc<dyn WorkspaceGitProvisioner>,
    ) -> Self {
        Self {
            paths,
            preferences,
            git,
        }
    }
}

async fn load_resolved_skill_bodies(skills: &[ResolvedAgentSkill]) -> Vec<LoadedAgentSkill> {
    let mut loaded = Vec::new();
    for skill in skills {
        let skill_file = skill.source_path.join("SKILL.md");
        match tokio::fs::read_to_string(&skill_file).await {
            Ok(content) => loaded.push(LoadedAgentSkill {
                name: skill.name.clone(),
                body: extract_skill_body(&content),
            }),
            Err(e) => {
                warn!(
                    skill = %skill.name,
                    path = %skill_file.display(),
                    error = %e,
                    "Failed to read requested skill body"
                );
            }
        }
    }
    loaded
}

fn extract_skill_body(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }

    let after_open = &trimmed[3..];
    if let Some(close_idx) = after_open.find("---") {
        let after_close = &after_open[close_idx + 3..];
        after_close.trim_start_matches('\n').to_string()
    } else {
        content.to_string()
    }
}

/// Encode a canonical skill reference as one portable directory name.
/// Encoding unsafe UTF-8 bytes rather than replacing characters avoids
/// collisions between namespaces such as `alice/team` and `alice:team`.
fn runtime_skill_name(reference: &str) -> String {
    let mut name = String::with_capacity(reference.len());
    for byte in reference.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            name.push(char::from(byte));
        } else {
            write!(&mut name, "_{byte:02x}").expect("writing to a String cannot fail");
        }
    }
    name
}

#[async_trait]
impl SkillResolver for ExtensionSkillResolver {
    async fn resolve_skills(&self, names: &[String]) -> Vec<ResolvedAgentSkill> {
        if names.is_empty() {
            return Vec::new();
        }
        let mut resolved = Vec::new();
        for reference in names {
            let Some(identity) = SkillIdentityResponse::parse_reference(reference) else {
                tracing::warn!(skill = %reference, "assistant skill reference is not canonical");
                continue;
            };
            let source = identity.source.as_str();
            let preference = match self.preferences.get(source, &identity.namespace, &identity.slug).await {
                Ok(Some(preference)) if preference.enabled => preference,
                Ok(_) => {
                    tracing::warn!(skill = %reference, "assistant skill is not enabled");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(skill = %reference, %error, "resolve_skills failed to read skill preference");
                    continue;
                }
            };
            let Some(version) = preference.selected_version.as_deref() else {
                tracing::warn!(skill = %reference, "enabled skill has no selected version");
                continue;
            };
            let space = match tjuaeui_extension::SkillSpace::parse(source) {
                Ok(space) => space,
                Err(error) => {
                    tracing::warn!(skill = %reference, %error, "enabled skill source is invalid");
                    continue;
                }
            };
            match tjuaeui_extension::ensure_runtime_snapshot(
                &self.paths.user_skills_dir,
                &self.paths.runtime_cache_dir,
                space,
                &preference.namespace,
                &preference.slug,
                version,
                self.git.clone(),
            )
            .await
            {
                Ok(source_path) => {
                    resolved.push(ResolvedAgentSkill {
                        name: runtime_skill_name(reference),
                        source_path,
                    });
                }
                Err(error) => {
                    tracing::warn!(skill = %reference, %error, "enabled skill snapshot could not be prepared")
                }
            }
        }
        resolved.sort_by(|left, right| left.name.cmp(&right.name));
        resolved
    }

    async fn link_workspace_skills(&self, workspace: &Path, rel_dirs: &[&str], skills: &[ResolvedAgentSkill]) -> usize {
        if rel_dirs.is_empty() || skills.is_empty() {
            return 0;
        }
        match tjuaeui_extension::link_workspace_skills(workspace, rel_dirs, skills).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    workspace = %workspace.display(),
                    error = %e,
                    "link_workspace_skills failed"
                );
                0
            }
        }
    }
}

#[cfg(test)]
pub struct FixedSkillResolver {
    pub names: Vec<String>,
}

#[cfg(test)]
#[async_trait]
impl SkillResolver for FixedSkillResolver {
    async fn resolve_skills(&self, _names: &[String]) -> Vec<ResolvedAgentSkill> {
        Vec::new()
    }

    async fn link_workspace_skills(
        &self,
        _workspace: &Path,
        _rel_dirs: &[&str],
        _skills: &[ResolvedAgentSkill],
    ) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extract_skill_body_removes_frontmatter() {
        let content = "---\nname: cron\ndescription: Cron\n---\nCron body";
        assert_eq!(extract_skill_body(content), "Cron body");
    }

    #[test]
    fn runtime_skill_name_is_portable_and_preserves_identity() {
        assert_eq!(
            runtime_skill_name("skillhub:alice/team:writer"),
            "skillhub_3aalice_2fteam_3awriter"
        );
        assert_eq!(
            runtime_skill_name(r"skillhub:alice\team:writer"),
            "skillhub_3aalice_5cteam_3awriter"
        );
        assert_ne!(
            runtime_skill_name("skillhub:alice/team:writer"),
            runtime_skill_name("skillhub:alice:team:writer")
        );
    }
}
