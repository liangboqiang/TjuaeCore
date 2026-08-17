use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::error::ExtensionError;

/// The only application-owned roots used by the canonical skill system.
#[derive(Debug, Clone)]
pub struct SkillPaths {
    /// One immediate child directory per installed skill package.
    pub user_skills_dir: PathBuf,
    /// User-authored assistant rules. Rules are not skill packages.
    pub assistant_rules_dir: PathBuf,
}

pub fn resolve_skill_paths(_app_resource_dir: &Path, data_dir: &Path) -> SkillPaths {
    SkillPaths {
        user_skills_dir: data_dir.join("skills"),
        assistant_rules_dir: data_dir.join("assistant-rules"),
    }
}

pub async fn read_assistant_rule(
    paths: &SkillPaths,
    assistant_id: &str,
    locale: Option<&str>,
) -> Result<String, ExtensionError> {
    read_assistant_resource(&paths.assistant_rules_dir, assistant_id, locale).await
}

pub async fn write_assistant_rule(
    paths: &SkillPaths,
    assistant_id: &str,
    content: &str,
    locale: Option<&str>,
) -> Result<bool, ExtensionError> {
    write_assistant_resource(&paths.assistant_rules_dir, assistant_id, content, locale).await
}

pub async fn delete_assistant_rule(paths: &SkillPaths, assistant_id: &str) -> Result<bool, ExtensionError> {
    delete_assistant_resource(&paths.assistant_rules_dir, assistant_id).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentSkill {
    pub name: String,
    pub source_path: PathBuf,
}

/// Expose canonical packages to a native agent without creating a second copy
/// or a second discovery rule. Existing targets are left untouched.
pub async fn link_workspace_skills(
    workspace: &Path,
    skills_rel_dirs: &[&str],
    skills: &[ResolvedAgentSkill],
) -> Result<usize, ExtensionError> {
    let mut created = 0;
    for relative in skills_rel_dirs {
        let target_dir = resolve_workspace_skills_dir(workspace, relative).await;
        tokio::fs::create_dir_all(&target_dir).await?;

        for skill in skills {
            let target = target_dir.join(&skill.name);
            match tokio::fs::symlink_metadata(&target).await {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    warn!(target = %target.display(), %error, "failed to inspect native skill target");
                    continue;
                }
            }
            match link_or_copy(&skill.source_path, &target).await {
                Ok(()) => created += 1,
                Err(error) => warn!(skill = %skill.name, target = %target.display(), %error, "failed to expose skill"),
            }
        }
    }
    Ok(created)
}

async fn resolve_workspace_skills_dir(workspace: &Path, relative: &str) -> PathBuf {
    let requested = workspace.join(relative);
    if is_dir(&requested).await {
        return requested;
    }
    let relative = Path::new(relative);
    if relative.file_name() == Some(std::ffi::OsStr::new("skills"))
        && let Some(parent) = relative.parent()
    {
        let singular = workspace.join(parent).join("skill");
        if is_dir(&singular).await {
            return singular;
        }
    }
    requested
}

async fn is_dir(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok_and(|metadata| metadata.is_dir())
}

async fn link_or_copy(source: &Path, target: &Path) -> Result<(), ExtensionError> {
    match create_directory_link(source, target).await {
        Ok(()) => Ok(()),
        Err(error) => {
            warn!(source = %source.display(), target = %target.display(), %error, "directory link unavailable; using a package snapshot");
            copy_directory(source, target).await
        }
    }
}

#[cfg(unix)]
async fn create_directory_link(source: &Path, target: &Path) -> Result<(), ExtensionError> {
    tokio::fs::symlink(source, target).await.map_err(ExtensionError::Io)
}

#[cfg(windows)]
async fn create_directory_link(source: &Path, target: &Path) -> Result<(), ExtensionError> {
    let source = source.to_path_buf();
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || junction::create(source, target))
        .await
        .map_err(|error| ExtensionError::Io(std::io::Error::other(error)))?
        .map_err(ExtensionError::Io)
}

async fn copy_directory(source: &Path, target: &Path) -> Result<(), ExtensionError> {
    tokio::fs::create_dir_all(target).await?;
    let mut entries = tokio::fs::read_dir(source).await?;
    while let Some(entry) = entries.next_entry().await? {
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type().await?.is_dir() {
            Box::pin(copy_directory(&source_path, &target_path)).await?;
        } else {
            tokio::fs::copy(source_path, target_path).await?;
        }
    }
    Ok(())
}

fn validate_segment(value: &str) -> Result<(), ExtensionError> {
    if value.is_empty() || value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(ExtensionError::PathTraversal(value.to_owned()));
    }
    Ok(())
}

async fn read_assistant_resource(
    directory: &Path,
    assistant_id: &str,
    locale: Option<&str>,
) -> Result<String, ExtensionError> {
    validate_segment(assistant_id)?;
    if let Some(locale) = locale {
        validate_segment(locale)?;
        if !locale.is_empty() {
            let path = directory.join(format!("{assistant_id}.{locale}.md"));
            if let Ok(content) = tokio::fs::read_to_string(path).await {
                return Ok(content);
            }
        }
    }
    let path = directory.join(format!("{assistant_id}.md"));
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(ExtensionError::Io(error)),
    }
}

async fn write_assistant_resource(
    directory: &Path,
    assistant_id: &str,
    content: &str,
    locale: Option<&str>,
) -> Result<bool, ExtensionError> {
    validate_segment(assistant_id)?;
    if let Some(locale) = locale {
        validate_segment(locale)?;
    }
    tokio::fs::create_dir_all(directory).await?;
    let name = match locale {
        Some(locale) if !locale.is_empty() => format!("{assistant_id}.{locale}.md"),
        _ => format!("{assistant_id}.md"),
    };
    let path = directory.join(name);
    tokio::fs::write(&path, content).await?;
    debug!(path = %path.display(), "assistant rule written");
    Ok(true)
}

async fn delete_assistant_resource(directory: &Path, assistant_id: &str) -> Result<bool, ExtensionError> {
    validate_segment(assistant_id)?;
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ExtensionError::Io(error)),
    };
    let exact = format!("{assistant_id}.md");
    let prefix = format!("{assistant_id}.");
    let mut deleted = false;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == exact || (name.starts_with(&prefix) && name.ends_with(".md")) {
            tokio::fs::remove_file(entry.path()).await?;
            deleted = true;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn assistant_rule_locale_falls_back_and_deletes_all_variants() {
        let temp = TempDir::new().unwrap();
        let paths = resolve_skill_paths(temp.path(), temp.path());
        write_assistant_rule(&paths, "helper", "default", None).await.unwrap();
        write_assistant_rule(&paths, "helper", "中文", Some("zh-CN"))
            .await
            .unwrap();
        assert_eq!(
            read_assistant_rule(&paths, "helper", Some("zh-CN")).await.unwrap(),
            "中文"
        );
        assert_eq!(
            read_assistant_rule(&paths, "helper", Some("fr-FR")).await.unwrap(),
            "default"
        );
        assert!(delete_assistant_rule(&paths, "helper").await.unwrap());
        assert!(read_assistant_rule(&paths, "helper", None).await.unwrap().is_empty());
    }

    #[test]
    fn assistant_rule_rejects_path_traversal() {
        assert!(validate_segment("../helper").is_err());
        assert!(validate_segment("helper/zh-CN").is_err());
    }
}
