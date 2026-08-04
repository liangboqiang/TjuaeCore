use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tjuaeui_api_types::{AssetFileEntryResponse, AssetKind};

use crate::AssetError;

pub const MAX_DEFINITION_FILE_BYTES: u64 = 1024 * 1024;
pub const MAX_DEFINITION_TOTAL_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_DEFINITION_FILES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetDefinitionFile {
    pub path: String,
    pub content: Vec<u8>,
}

impl AssetDefinitionFile {
    pub fn text(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into().into_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinitionManifestEntry {
    pub path: String,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
    pub text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedDefinition {
    pub digest: String,
    pub files: Vec<DefinitionManifestEntry>,
}

impl ScannedDefinition {
    pub fn api_files(&self) -> Vec<AssetFileEntryResponse> {
        self.files
            .iter()
            .map(|file| AssetFileEntryResponse {
                path: file.path.clone(),
                digest: file.digest.clone(),
                size: file.size,
                media_type: file.media_type.clone(),
                text: file.text,
            })
            .collect()
    }
}

pub fn validate_entry_file(
    kind: AssetKind,
    entry_file: Option<&str>,
    files: &[DefinitionManifestEntry],
) -> Result<(), AssetError> {
    let required = match kind {
        AssetKind::Assistant => entry_file,
        AssetKind::EngineAdapter => {
            if entry_file != Some("engine-adapter.json") {
                return Err(AssetError::InvalidMetadata(
                    "引擎适配器资产入口必须是 engine-adapter.json".into(),
                ));
            }
            entry_file
        }
        AssetKind::Mcp => {
            if entry_file != Some("mcp.json") {
                return Err(AssetError::InvalidMetadata("MCP 资产入口必须是 mcp.json".into()));
            }
            entry_file
        }
        AssetKind::Skill => Some(entry_file.unwrap_or("SKILL.md")),
    };
    if let Some(required) = required {
        let normalized = normalize_relative_path(required)?;
        if !files.iter().any(|file| file.path == normalized) {
            return Err(AssetError::InvalidMetadata(format!("入口文件不存在：{normalized}")));
        }
    }
    Ok(())
}

pub fn prepare_definition(
    files: Vec<AssetDefinitionFile>,
) -> Result<(Vec<AssetDefinitionFile>, ScannedDefinition), AssetError> {
    if files.is_empty() {
        return Err(AssetError::InvalidMetadata("资产 Definition 不能为空".into()));
    }
    if files.len() > MAX_DEFINITION_FILES {
        return Err(AssetError::InvalidMetadata(format!(
            "资产文件数量超过限制：{}",
            MAX_DEFINITION_FILES
        )));
    }
    let mut seen = HashSet::new();
    let mut normalized = Vec::with_capacity(files.len());
    let mut manifest = Vec::with_capacity(files.len());
    let mut total = 0_u64;
    for file in files {
        let path = normalize_relative_path(&file.path)?;
        let collision_key = path.to_ascii_lowercase();
        if !seen.insert(collision_key) {
            return Err(AssetError::UnsafePath(format!("路径重复或大小写冲突：{path}")));
        }
        let size = file.content.len() as u64;
        if size > MAX_DEFINITION_FILE_BYTES {
            return Err(AssetError::FileTooLarge {
                path,
                actual: size,
                limit: MAX_DEFINITION_FILE_BYTES,
            });
        }
        total = total.saturating_add(size);
        if total > MAX_DEFINITION_TOTAL_BYTES {
            return Err(AssetError::TotalTooLarge {
                actual: total,
                limit: MAX_DEFINITION_TOTAL_BYTES,
            });
        }
        let digest = digest_bytes(&file.content);
        let media_type = mime_guess::from_path(&path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_owned();
        manifest.push(DefinitionManifestEntry {
            path: path.clone(),
            digest,
            size,
            media_type,
            text: std::str::from_utf8(&file.content).is_ok(),
        });
        normalized.push(AssetDefinitionFile {
            path,
            content: file.content,
        });
    }
    normalized.sort_by(|left, right| left.path.cmp(&right.path));
    manifest.sort_by(|left, right| left.path.cmp(&right.path));
    let digest = digest_manifest(&manifest);
    Ok((
        normalized,
        ScannedDefinition {
            digest,
            files: manifest,
        },
    ))
}

pub fn scan_definition(root: &Path) -> Result<ScannedDefinition, AssetError> {
    load_definition(root).map(|(_, scanned)| scanned)
}

pub fn load_definition(root: &Path) -> Result<(Vec<AssetDefinitionFile>, ScannedDefinition), AssetError> {
    let canonical_root = std::fs::canonicalize(root)?;
    let mut files = Vec::new();
    collect_definition_files(&canonical_root, &canonical_root, &mut files)?;
    prepare_definition(files)
}

fn collect_definition_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<AssetDefinitionFile>,
) -> Result<(), AssetError> {
    let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(AssetError::UnsafePath(format!(
                "Definition 不允许符号链接：{}",
                entry.path().display()
            )));
        }
        if file_type.is_dir() {
            let canonical = std::fs::canonicalize(entry.path())?;
            if !canonical.starts_with(root) {
                return Err(AssetError::UnsafePath(entry.path().display().to_string()));
            }
            collect_definition_files(root, &canonical, output)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(AssetError::UnsafePath(format!(
                "Definition 包含特殊文件：{}",
                entry.path().display()
            )));
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| AssetError::UnsafePath(entry.path().display().to_string()))?
            .to_path_buf();
        output.push(AssetDefinitionFile {
            path: path_to_slashes(&relative)?,
            content: std::fs::read(entry.path())?,
        });
    }
    Ok(())
}

pub fn digest_bytes(content: &[u8]) -> String {
    format!("sha256-{}", hex::encode(Sha256::digest(content)))
}

fn digest_manifest(files: &[DefinitionManifestEntry]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tjuae-asset-definition-v1\0");
    for file in files {
        hasher.update((file.path.len() as u64).to_be_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(file.size.to_be_bytes());
        hasher.update(file.digest.as_bytes());
    }
    format!("sha256-{}", hex::encode(hasher.finalize()))
}

pub fn normalize_relative_path(value: &str) -> Result<String, AssetError> {
    if value.is_empty()
        || value.contains('\\')
        || value.starts_with('/')
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
        || value.contains('\0')
    {
        return Err(AssetError::UnsafePath(value.into()));
    }
    let path = Path::new(value);
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(AssetError::UnsafePath(value.into()));
        };
        let value = value
            .to_str()
            .ok_or_else(|| AssetError::UnsafePath(path.display().to_string()))?;
        validate_portable_segment(value)?;
        parts.push(value);
    }
    if parts.is_empty() {
        return Err(AssetError::UnsafePath(value.into()));
    }
    Ok(parts.join("/"))
}

fn validate_portable_segment(segment: &str) -> Result<(), AssetError> {
    let trimmed = segment.trim_end_matches(['.', ' ']);
    if trimmed != segment || segment.contains(':') {
        return Err(AssetError::UnsafePath(segment.into()));
    }
    let stem = segment.split('.').next().unwrap_or(segment).to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || stem
            .strip_prefix("com")
            .and_then(|value| value.parse::<u8>().ok())
            .is_some_and(|value| (1..=9).contains(&value))
        || stem
            .strip_prefix("lpt")
            .and_then(|value| value.parse::<u8>().ok())
            .is_some_and(|value| (1..=9).contains(&value));
    if reserved {
        return Err(AssetError::UnsafePath(segment.into()));
    }
    Ok(())
}

fn path_to_slashes(path: &Path) -> Result<String, AssetError> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| AssetError::UnsafePath(path.display().to_string())),
            _ => Err(AssetError::UnsafePath(path.display().to_string())),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

pub fn join_safe(root: &Path, relative: &str) -> Result<PathBuf, AssetError> {
    Ok(root.join(normalize_relative_path(relative)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_digest_is_order_independent() {
        let first = prepare_definition(vec![
            AssetDefinitionFile::text("SKILL.md", "# Demo"),
            AssetDefinitionFile::text("references/a.md", "A"),
        ])
        .unwrap()
        .1;
        let second = prepare_definition(vec![
            AssetDefinitionFile::text("references/a.md", "A"),
            AssetDefinitionFile::text("SKILL.md", "# Demo"),
        ])
        .unwrap()
        .1;
        assert_eq!(first, second);
    }

    #[test]
    fn portable_paths_reject_traversal_devices_ads_and_case_collisions() {
        for path in [
            "../secret",
            r"skills\demo",
            "C:/secret",
            "CON.txt",
            "file.txt:ads",
            "trailing.",
        ] {
            assert!(normalize_relative_path(path).is_err(), "{path}");
        }
        assert!(
            prepare_definition(vec![
                AssetDefinitionFile::text("SKILL.md", "A"),
                AssetDefinitionFile::text("skill.md", "B"),
            ])
            .is_err()
        );
    }
}
