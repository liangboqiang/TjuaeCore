//! Resolve explicitly selected local skills into runtime-ready assets.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tjuaeui_api_types::AssetKind;
pub use tjuaeui_asset::ResolvedAgentSkill;
use tjuaeui_asset::{AssetCatalogService, AssetError, RuntimeAssetProvenance};
use tjuaeui_db::ISkillRepository;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedAgentSkill {
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntimeSkill {
    pub name: String,
    pub source_path: PathBuf,
    pub provenance: RuntimeAssetProvenance,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeSkillResolutionError {
    #[error("运行时技能来源目录未配置")]
    ProvenanceUnavailable,
    #[error(transparent)]
    Catalog(#[from] AssetError),
    #[error("运行时技能引用重复映射到同一本地资产：{0}")]
    DuplicateAsset(String),
    #[error("技能资产 {0} 没有唯一的运行时投影")]
    ProjectionMissing(String),
}

#[async_trait]
pub trait SkillResolver: Send + Sync {
    /// Resolve each skill name to its on-disk source directory, using the
    /// same search order as `materialize_skills_for_agent`.
    async fn resolve_skills(&self, names: &[String]) -> Vec<ResolvedAgentSkill>;

    /// Resolve workspace-link sources for an authenticated user. Production
    /// implementations use active catalog bindings; the default preserves
    /// lightweight test resolvers without inventing a global user.
    async fn resolve_skills_for_user(&self, _user_id: &str, names: &[String]) -> Vec<ResolvedAgentSkill> {
        self.resolve_skills(names).await
    }

    /// Resolve selected skills through AssetCatalog before accepting their
    /// runtime projections. Implementations must not infer identity from a
    /// directory name, display name or remote slug.
    async fn resolve_runtime_skills(
        &self,
        _user_id: &str,
        references: &[String],
    ) -> Result<Vec<ResolvedRuntimeSkill>, RuntimeSkillResolutionError> {
        if references.is_empty() {
            Ok(Vec::new())
        } else {
            Err(RuntimeSkillResolutionError::ProvenanceUnavailable)
        }
    }

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

/// Production adapter backed by `tjuaeui_asset::skill_runtime`.
pub struct ExtensionSkillResolver {
    paths: Arc<tjuaeui_asset::SkillPaths>,
    skill_repo: Arc<dyn ISkillRepository>,
    asset_catalog: Arc<AssetCatalogService>,
}

impl ExtensionSkillResolver {
    pub fn new(
        paths: Arc<tjuaeui_asset::SkillPaths>,
        skill_repo: Arc<dyn ISkillRepository>,
        asset_catalog: Arc<AssetCatalogService>,
    ) -> Self {
        Self {
            paths,
            skill_repo,
            asset_catalog,
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

#[async_trait]
impl SkillResolver for ExtensionSkillResolver {
    async fn resolve_skills(&self, names: &[String]) -> Vec<ResolvedAgentSkill> {
        if names.is_empty() {
            return Vec::new();
        }
        // Conversation_id is validated upstream; we don't use a real one here
        // because this resolver is purely a path-resolution helper.
        match tjuaeui_asset::materialize_skills_for_agent_with_repo(
            &self.paths,
            self.skill_repo.as_ref(),
            "workspace-link",
            names,
        )
        .await
        {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "resolve_skills failed; returning empty list"
                );
                Vec::new()
            }
        }
    }

    async fn resolve_skills_for_user(&self, user_id: &str, names: &[String]) -> Vec<ResolvedAgentSkill> {
        let mut resolved = Vec::with_capacity(names.len());
        let mut seen = BTreeSet::new();
        for reference in names {
            match self
                .asset_catalog
                .resolve_bound_runtime_asset(user_id, AssetKind::Skill, reference)
                .await
            {
                Ok(bound) if seen.insert(bound.provenance.local_asset_id.clone()) => {
                    resolved.push(ResolvedAgentSkill {
                        name: bound.provenance.runtime_id,
                        source_path: bound.workspace_path,
                    });
                }
                Ok(_) => warn!(skill_reference = %reference, "duplicate active skill binding ignored"),
                Err(error) => warn!(
                    skill_reference = %reference,
                    error = %error,
                    "active skill binding resolution failed"
                ),
            }
        }
        resolved
    }

    async fn resolve_runtime_skills(
        &self,
        user_id: &str,
        references: &[String],
    ) -> Result<Vec<ResolvedRuntimeSkill>, RuntimeSkillResolutionError> {
        if references.is_empty() {
            return Ok(Vec::new());
        }

        let mut resolved = Vec::with_capacity(references.len());
        let mut local_asset_ids = BTreeSet::new();
        for reference in references {
            let bound = self
                .asset_catalog
                .resolve_bound_runtime_asset(user_id, AssetKind::Skill, reference)
                .await?;
            if !local_asset_ids.insert(bound.provenance.local_asset_id.clone()) {
                return Err(RuntimeSkillResolutionError::DuplicateAsset(
                    bound.provenance.local_asset_id,
                ));
            }
            resolved.push(ResolvedRuntimeSkill {
                name: bound.provenance.runtime_id.clone(),
                source_path: bound.workspace_path,
                provenance: bound.provenance,
            });
        }
        Ok(resolved)
    }

    async fn link_workspace_skills(&self, workspace: &Path, rel_dirs: &[&str], skills: &[ResolvedAgentSkill]) -> usize {
        if rel_dirs.is_empty() || skills.is_empty() {
            return 0;
        }
        match tjuaeui_asset::link_workspace_skills(workspace, rel_dirs, skills).await {
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
pub struct FixedSkillResolver;

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
}
