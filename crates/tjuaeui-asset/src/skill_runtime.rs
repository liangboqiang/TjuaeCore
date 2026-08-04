use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{debug, warn};

use tjuaeui_db::{ISkillRepository, SkillRow, UpsertSkillParams};

use crate::AssetError;

const SKILLS_DIR_NAME: &str = "skills";
const CRON_SKILLS_DIR_NAME: &str = "cron/skills";
const SKILL_MANIFEST_FILE: &str = "SKILL.md";

// ---------------------------------------------------------------------------
// Skill paths resolution
// ---------------------------------------------------------------------------

/// Resolved directories used by runtime skill projection.
#[derive(Debug, Clone)]
pub struct SkillPaths {
    /// User-created skills directory (~/.tjuaeui/skills/).
    pub user_skills_dir: PathBuf,
    /// Per-job cron skills directory (~/.tjuaeui/cron/skills/).
    pub cron_skills_dir: PathBuf,
}

/// Resolve standard skill paths.
pub fn resolve_skill_paths(_app_resource_dir: &Path, data_dir: &Path) -> SkillPaths {
    SkillPaths {
        user_skills_dir: data_dir.join(SKILLS_DIR_NAME),
        cron_skills_dir: data_dir.join(CRON_SKILLS_DIR_NAME),
    }
}

// ---------------------------------------------------------------------------
// Runtime skill listing
// ---------------------------------------------------------------------------

/// Runtime ownership of a locally installed skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    Managed,
    Cron,
    Asset,
}

/// A discovered skill item for listing.
///
#[derive(Debug, Clone, PartialEq)]
pub struct SkillListItem {
    pub name: String,
    pub description: String,
    pub location: String,
    pub is_custom: bool,
    pub source: SkillSource,
}

/// List all locally installed skills.
pub async fn list_available_skills(paths: &SkillPaths) -> Result<Vec<SkillListItem>, AssetError> {
    // DB-backed production callers use `list_available_skills_with_repo`.
    // This path-only fallback is retained for low-level tests.
    let mut custom_skills = list_user_skills_from_disk(paths).await?;

    custom_skills.sort_by(|a, b| {
        skill_modified_time(&b.location)
            .cmp(&skill_modified_time(&a.location))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(custom_skills)
}

/// List all available skills using the database as the user-skill state source.
pub async fn list_available_skills_with_repo(
    paths: &SkillPaths,
    repo: &dyn ISkillRepository,
) -> Result<Vec<SkillListItem>, AssetError> {
    list_skills_from_repo(paths, repo).await
}

#[derive(Debug, Clone, PartialEq)]
struct ScannedSkill {
    name: String,
    description: String,
    path: String,
}

fn skill_modified_time(path: &str) -> SystemTime {
    std::fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH)
}

// ---------------------------------------------------------------------------
// D2. Per-agent skill resolution
// ---------------------------------------------------------------------------

/// A resolved skill reference returned by [`materialize_skills_for_agent`].
///
/// `name` is the skill's requested name; `source_path` is the absolute
/// on-disk directory containing its `SKILL.md`. The caller is expected
/// to symlink that directory into the agent CLI's native skills dir
/// rather than copy it — backend no longer owns per-conversation files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentSkill {
    pub name: String,
    pub source_path: PathBuf,
}

/// Resolve each requested skill name to its on-disk source directory.
///
/// Search order per name (first match wins):
/// 1. `{user_skills_dir}/{name}/` — installed or user-created skill.
/// 2. `{cron_skills_dir}/{name}/` — per-job cron skill.
///
/// No files are copied and no per-conversation directory is created —
/// the backend just hands the absolute source paths back to the caller,
/// which is responsible for symlinking them where the CLI expects. This
/// replaces the older "copy into `{data_dir}/agent-skills/{conv_id}/`"
/// behavior once the frontend moved to a symlink-only contract.
///
/// Unknown names are silently skipped (a warning is emitted). Names
/// containing path separators or `..` are rejected with a warning and
/// skipped. Empty names are ignored.
///
/// The returned list is sorted by `name` for determinism. The
/// `conversation_id` is still validated (rejects path-traversal values)
/// so downstream callers can safely use it in log lines or paths even
/// though this function no longer touches disk per-conversation.
pub async fn materialize_skills_for_agent(
    paths: &SkillPaths,
    conversation_id: &str,
    skills: &[String],
) -> Result<Vec<ResolvedAgentSkill>, AssetError> {
    validate_filename(conversation_id)?;

    let mut resolved = Vec::with_capacity(skills.len());
    for name in skills {
        if name.is_empty() {
            continue;
        }
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            warn!(skill = %name, "skipping skill with invalid name");
            continue;
        }
        match resolve_skill_source_path(paths, name).await? {
            Some(source_path) => resolved.push(ResolvedAgentSkill {
                name: name.clone(),
                source_path,
            }),
            None => warn!(skill = %name, "skill not found in any source"),
        }
    }

    resolved.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(resolved)
}

/// Resolve requested skill names using the database for user skill state.
pub async fn materialize_skills_for_agent_with_repo(
    paths: &SkillPaths,
    repo: &dyn ISkillRepository,
    conversation_id: &str,
    skills: &[String],
) -> Result<Vec<ResolvedAgentSkill>, AssetError> {
    validate_filename(conversation_id)?;

    let mut resolved = Vec::with_capacity(skills.len());
    for name in skills {
        if name.is_empty() {
            continue;
        }
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            warn!(skill = %name, "skipping skill with invalid name");
            continue;
        }
        match resolve_skill_source_path_with_repo(paths, repo, name).await? {
            Some(source_path) => resolved.push(ResolvedAgentSkill {
                name: name.clone(),
                source_path,
            }),
            None => warn!(skill = %name, "skill not found in any source"),
        }
    }

    resolved.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(resolved)
}

/// Create symlinks from a set of resolved skills into the agent CLI's
/// native skills directories inside `workspace`.
///
/// For each relative `skills_rel_dir` (e.g. `.claude/skills`):
/// 1. Resolve the target directory. Existing `{workspace}/{skills_rel_dir}/`
///    wins; if the requested leaf is `skills` and sibling `skill` already
///    exists, reuse that singular directory; otherwise create the requested
///    directory.
/// 2. For each `{ name, source_path }` in `skills`, create a symlink
///    `{target_skills_dir}/{name} -> {source_path}`.
///
/// Existing symlinks/files at the target name are left untouched
/// (first-write-wins, matches the frontend's lstat-then-skip behavior
/// before symlink creation). Individual symlink failures are logged and
/// skipped — skill discovery degrades gracefully, it is not fatal.
///
/// Returns the number of symlinks successfully created across all
/// target dirs.
pub async fn link_workspace_skills(
    workspace: &Path,
    skills_rel_dirs: &[&str],
    skills: &[ResolvedAgentSkill],
) -> Result<usize, AssetError> {
    let mut created = 0usize;
    for rel in skills_rel_dirs {
        let target_skills_dir = resolve_workspace_skills_dir(workspace, rel).await;
        tokio::fs::create_dir_all(&target_skills_dir).await?;

        for skill in skills {
            let target = target_skills_dir.join(&skill.name);
            match tokio::fs::symlink_metadata(&target).await {
                // Target already exists — leave it alone.
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!(
                        target = %target.display(),
                        error = %e,
                        "skipping skill link: failed to stat target"
                    );
                    continue;
                }
            }
            match link_skill_or_fallback_copy(&skill.source_path, &target).await {
                Ok(()) => {
                    debug!(
                        skill = %skill.name,
                        target = %target.display(),
                        "linked workspace skill"
                    );
                    created += 1;
                }
                Err(e) => {
                    warn!(
                        skill = %skill.name,
                        target = %target.display(),
                        error = %e,
                        "failed to link workspace skill"
                    );
                }
            }
        }
    }
    Ok(created)
}

async fn resolve_workspace_skills_dir(workspace: &Path, skills_rel_dir: &str) -> PathBuf {
    let requested = workspace.join(skills_rel_dir);
    if path_is_dir(&requested).await {
        return requested;
    }

    let rel_path = Path::new(skills_rel_dir);
    if rel_path.file_name() == Some(std::ffi::OsStr::new("skills"))
        && let Some(parent) = rel_path.parent()
    {
        let singular = workspace.join(parent).join("skill");
        if path_is_dir(&singular).await {
            return singular;
        }
    }

    requested
}

async fn path_is_dir(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
}

/// Resolve a skill name to its on-disk source directory using the same
/// search order as [`materialize_skills_for_agent`]. Returns `None` if
/// no matching directory exists in any known source.
async fn resolve_skill_source_path(paths: &SkillPaths, name: &str) -> Result<Option<PathBuf>, AssetError> {
    let user = paths.user_skills_dir.join(name);
    if user.is_dir() {
        return Ok(Some(user));
    }
    let cron = paths.cron_skills_dir.join(name);
    if cron.is_dir() {
        return Ok(Some(cron));
    }
    Ok(None)
}

async fn resolve_skill_source_path_with_repo(
    paths: &SkillPaths,
    repo: &dyn ISkillRepository,
    name: &str,
) -> Result<Option<PathBuf>, AssetError> {
    if let Some(row) = repo.find_by_name(name).await? {
        let path = PathBuf::from(&row.path);
        if path.is_dir() {
            return Ok(Some(path));
        }
        warn!(
            skill = %name,
            path = %path.display(),
            "skill row points at a missing directory"
        );
        return Ok(None);
    }
    let cron = paths.cron_skills_dir.join(name);
    if cron.is_dir() {
        return Ok(Some(cron));
    }
    Ok(None)
}

async fn list_skills_from_repo(
    _paths: &SkillPaths,
    repo: &dyn ISkillRepository,
) -> Result<Vec<SkillListItem>, AssetError> {
    let mut items = Vec::new();
    for row in repo.list().await? {
        let description = row.description.clone().unwrap_or_default();
        items.push(skill_row_to_list_item(row, description));
    }
    Ok(items)
}

/// Refresh only generated runtime projections.
///
/// User-authored skill Definitions are owned only by the asset catalog and
/// must never be auto-adopted from `user_skills_dir`. Cron skills remain an
/// explicit generated runtime projection.
pub async fn sync_generated_skill_projections(
    paths: &SkillPaths,
    repo: &dyn ISkillRepository,
) -> Result<(), AssetError> {
    sync_cron_skills_into_repo(paths, repo).await
}

async fn sync_cron_skills_into_repo(paths: &SkillPaths, repo: &dyn ISkillRepository) -> Result<(), AssetError> {
    if let Ok(skills) = scan_skill_dirs(&paths.cron_skills_dir).await {
        for skill in skills {
            sync_managed_skill_into_repo(repo, &skill, "cron").await?;
        }
    }

    Ok(())
}

async fn sync_managed_skill_into_repo(
    repo: &dyn ISkillRepository,
    skill: &ScannedSkill,
    source: &str,
) -> Result<(), AssetError> {
    if let Some(existing) = repo.find_by_name_any(&skill.name).await?
        && existing.source == "user"
        && existing.deleted_at.is_none()
        && existing.enabled
    {
        return Ok(());
    }

    repo.upsert(UpsertSkillParams {
        name: &skill.name,
        description: Some(&skill.description),
        path: &skill.path,
        source,
        enabled: true,
    })
    .await?;
    Ok(())
}

fn skill_row_to_list_item(row: SkillRow, description: String) -> SkillListItem {
    let source = skill_source_from_row(&row.source);
    let location = match source {
        SkillSource::Cron => PathBuf::from(&row.path)
            .join(SKILL_MANIFEST_FILE)
            .to_string_lossy()
            .into_owned(),
        SkillSource::Managed | SkillSource::Asset => row.path.clone(),
    };

    SkillListItem {
        name: row.name,
        description,
        location,
        is_custom: source == SkillSource::Managed,
        source,
    }
}

fn skill_source_from_row(source: &str) -> SkillSource {
    match source {
        "cron" => SkillSource::Cron,
        "asset" | "extension" => SkillSource::Asset,
        _ => SkillSource::Managed,
    }
}

async fn list_user_skills_from_disk(paths: &SkillPaths) -> Result<Vec<SkillListItem>, AssetError> {
    let scanned = scan_skill_dirs(&paths.user_skills_dir).await?;
    Ok(scanned
        .into_iter()
        .map(|skill| SkillListItem {
            name: skill.name,
            description: skill.description,
            location: skill.path,
            is_custom: true,
            source: SkillSource::Managed,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Validate a filename to prevent path traversal.
fn validate_filename(name: &str) -> Result<(), AssetError> {
    if name.contains('/') || name.contains('\\') || name.contains("..") || name.is_empty() {
        return Err(AssetError::UnsafePath(name.to_string()));
    }
    Ok(())
}

/// Scan a directory for subdirectories containing a SKILL.md file.
async fn scan_skill_dirs(dir: &Path) -> Result<Vec<ScannedSkill>, AssetError> {
    let mut result = Vec::new();
    let mut skill_dirs = Vec::new();
    collect_skill_dirs_recursive(dir, &mut skill_dirs).await?;

    for entry_path in skill_dirs {
        let skill_file = entry_path.join(SKILL_MANIFEST_FILE);
        match tokio::fs::read_to_string(&skill_file).await {
            Ok(content) => {
                if let Some((name, description)) = parse_frontmatter_fields(&content) {
                    let final_name = if name.is_empty() {
                        entry_path
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    } else {
                        name
                    };
                    result.push(ScannedSkill {
                        name: final_name,
                        description,
                        path: entry_path.to_string_lossy().into_owned(),
                    });
                }
            }
            Err(e) => {
                warn!(
                    path = %skill_file.display(),
                    error = %e,
                    "failed to read SKILL.md"
                );
            }
        }
    }

    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

async fn collect_skill_dirs_recursive(dir: &Path, result: &mut Vec<PathBuf>) -> Result<(), AssetError> {
    if dir.join(SKILL_MANIFEST_FILE).exists() {
        result.push(dir.to_path_buf());
        return Ok(());
    }

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AssetError::Io(e)),
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            Box::pin(collect_skill_dirs_recursive(&entry_path, result)).await?;
        }
    }

    result.sort();
    Ok(())
}

/// Parse SKILL.md frontmatter to extract name and description.
///
/// Expected format:
/// ```text
/// ---
/// name: skill-name
/// description: One line description
/// ---
/// Body content here...
/// ```
fn parse_frontmatter_fields(content: &str) -> Option<(String, String)> {
    #[derive(serde::Deserialize)]
    struct SkillFrontmatter {
        #[serde(default)]
        name: String,
        description: String,
    }

    let frontmatter = extract_frontmatter_text(content)?;
    let parsed = serde_yaml::from_str::<SkillFrontmatter>(frontmatter).ok()?;
    let description = parsed.description.trim().to_string();

    if description.is_empty() {
        return None;
    }

    Some((parsed.name.trim().to_string(), description))
}

fn extract_frontmatter_text(content: &str) -> Option<&str> {
    let after_open = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;

    let mut pos = 0;
    for line in after_open.lines() {
        let raw = &after_open[pos..];
        let line_len = line.len();
        let line_with_ending_len = if raw[line_len..].starts_with("\r\n") {
            line_len + 2
        } else if raw[line_len..].starts_with('\n') {
            line_len + 1
        } else {
            line_len
        };

        if line == "---" {
            let yaml_text = &after_open[..pos];
            return Some(
                yaml_text
                    .strip_suffix("\r\n")
                    .or_else(|| yaml_text.strip_suffix('\n'))
                    .unwrap_or(yaml_text),
            );
        }

        pos += line_with_ending_len;
    }

    None
}

/// Recursively copy a directory.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), AssetError> {
    tokio::fs::create_dir_all(dst).await?;

    let mut entries = tokio::fs::read_dir(src).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let entry_path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if entry_path.is_dir() {
            Box::pin(copy_dir_recursive(&entry_path, &dest_path)).await?;
        } else {
            tokio::fs::copy(&entry_path, &dest_path).await?;
        }
    }

    Ok(())
}

/// Try to symlink `src` into `dst`; on failure, fall back to a recursive
/// copy of the source directory.
///
/// Motivation: on Windows machines without "Developer Mode" or admin
/// privileges, `CreateSymbolicLinkW` fails with `os error 1314`
/// (`ERROR_PRIVILEGE_NOT_HELD`). Auto-injected managed skills under each
/// backend's `.<backend>/skills/` directory then become invisible to the
/// CLI agent — silently degrading the product. Falling back to a copy
/// keeps the skills discoverable; the trade-off is that copies do not
/// track upstream changes until the next link pass clears them. The
/// fallback applies on every platform (Linux/macOS shouldn't normally
/// hit this, but we keep behavior uniform so a future EPERM/EROFS sandbox
/// also stays healthy).
///
/// Logs a `warn!` with the OS error kind and `raw_os_error` so we can
/// keep tracking 1314 vs other failure modes in local diagnostics. No
/// user-identifying data is logged — only the source/target paths
/// (already considered safe to log elsewhere in this module) and the
/// error code.
async fn link_skill_or_fallback_copy(src: &Path, dst: &Path) -> Result<(), AssetError> {
    match create_symlink_for_link(src, dst).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // Surface the raw OS error so dashboards can keep counting 1314
            // (ERROR_PRIVILEGE_NOT_HELD) separately from other failure modes.
            let raw_os_error = match &e {
                AssetError::Io(io_err) => io_err.raw_os_error(),
                _ => None,
            };
            warn!(
                src = %src.display(),
                dst = %dst.display(),
                error = %e,
                raw_os_error = ?raw_os_error,
                "create_symlink failed; falling back to copy_dir_recursive"
            );
            copy_dir_recursive(src, dst).await
        }
    }
}

async fn create_symlink_for_link(src: &Path, dst: &Path) -> Result<(), AssetError> {
    create_symlink(src, dst).await
}

/// Create a symlink (platform-aware).
#[cfg(unix)]
async fn create_symlink(src: &Path, dst: &Path) -> Result<(), AssetError> {
    tokio::fs::symlink(src, dst).await.map_err(AssetError::Io)
}

#[cfg(windows)]
async fn create_symlink(src: &Path, dst: &Path) -> Result<(), AssetError> {
    // On Windows, directory symlinks require `SeCreateSymbolicLink`
    // (Developer Mode or Admin), which most users don't have — this is
    // the source of the `os error 1314` permission-failure regressions.
    //
    // NTFS junctions are an unprivileged alternative for *directory*
    // targets: the kernel exposes them via `FSCTL_SET_REPARSE_POINT`
    // which does not require the symlink privilege. Use them whenever
    // possible. File targets cannot be junctioned, so they fall back to
    // `tokio::fs::symlink_file`; in the rare cases that fails the
    // outer `link_skill_or_fallback_copy` wrapper still rescues us via
    // `copy_dir_recursive`.
    if src.is_dir() {
        let src = src.to_path_buf();
        let dst = dst.to_path_buf();
        tokio::task::spawn_blocking(move || junction::create(&src, &dst))
            .await
            .map_err(|e| AssetError::Io(std::io::Error::other(format!("junction::create join error: {e}"))))?
            .map_err(AssetError::Io)
    } else {
        tokio::fs::symlink_file(src, dst).await.map_err(AssetError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_accepts_valid_skill() {
        let content = "---\nname: test-skill\ndescription: A test skill\n---\n# Body";
        assert_eq!(
            parse_frontmatter_fields(content),
            Some(("test-skill".to_owned(), "A test skill".to_owned()))
        );
    }
}
