use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fs2::FileExt;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AssetError;
use crate::definition::{AssetDefinitionFile, ScannedDefinition, prepare_definition, scan_definition};

#[derive(Clone, Debug)]
pub struct AssetContentStore {
    root: PathBuf,
}

impl AssetContentStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: data_dir.into().join("asset-repository"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn workspace_key(&self, user_id: &str, asset_id: &str) -> String {
        format!("{}/{}", stable_path_key(user_id), stable_path_key(asset_id))
    }

    pub fn workspace_path(&self, workspace_key: &str) -> Result<PathBuf, AssetError> {
        let path = self.root.join("workspaces").join(workspace_key);
        ensure_descendant(&self.root.join("workspaces"), &path)?;
        Ok(path)
    }

    pub fn object_path(&self, object_key: &str) -> Result<PathBuf, AssetError> {
        if object_key.len() != 64 || !object_key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AssetError::UnsafePath(object_key.into()));
        }
        Ok(self.root.join("objects").join(object_key))
    }

    pub fn lock_asset(&self, user_id: &str, asset_id: &str) -> Result<AssetFileLock, AssetError> {
        let locks = self.root.join("locks");
        std::fs::create_dir_all(&locks)?;
        let lock_path = locks.join(format!(
            "{}-{}.lock",
            stable_path_key(user_id),
            stable_path_key(asset_id)
        ));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        file.lock_exclusive()?;
        Ok(AssetFileLock { file })
    }

    pub fn ensure_object(
        &self,
        files: Vec<AssetDefinitionFile>,
        expected_digest: &str,
    ) -> Result<(String, ScannedDefinition), AssetError> {
        let (files, scanned) = prepare_definition(files)?;
        if scanned.digest != expected_digest {
            return Err(AssetError::DigestMismatch {
                expected: expected_digest.into(),
                actual: scanned.digest,
            });
        }
        let object_key = expected_digest
            .strip_prefix("sha256-")
            .ok_or_else(|| AssetError::InvalidMetadata("摘要必须使用 sha256- 前缀".into()))?
            .to_owned();
        let final_path = self.object_path(&object_key)?;
        if final_path.is_dir() {
            let stored = scan_definition(&final_path)?;
            if stored.digest != expected_digest {
                return Err(AssetError::CorruptObject(final_path));
            }
            return Ok((object_key, stored));
        }
        let staging_root = self.root.join("staging");
        std::fs::create_dir_all(&staging_root)?;
        let staging = staging_root.join(format!("object-{}", Uuid::now_v7()));
        write_definition_tree(&staging, &files)?;
        let verified = scan_definition(&staging)?;
        if verified.digest != expected_digest {
            remove_scoped_dir(&staging_root, &staging);
            return Err(AssetError::DigestMismatch {
                expected: expected_digest.into(),
                actual: verified.digest,
            });
        }
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match rename_with_retry(&staging, &final_path) {
            Ok(()) => {}
            Err(error) if final_path.is_dir() => {
                remove_scoped_dir(&staging_root, &staging);
                let stored = scan_definition(&final_path)?;
                if stored.digest != expected_digest {
                    return Err(AssetError::CorruptObject(final_path));
                }
                tracing::debug!(%error, "asset object was created by another process");
            }
            Err(error) => {
                remove_scoped_dir(&staging_root, &staging);
                return Err(error.into());
            }
        }
        Ok((object_key, verified))
    }

    pub fn activate_workspace(
        &self,
        workspace_key: &str,
        files: &[AssetDefinitionFile],
    ) -> Result<WorkspaceActivation, AssetError> {
        let workspaces = self.root.join("workspaces");
        std::fs::create_dir_all(&workspaces)?;
        let final_path = self.workspace_path(workspace_key)?;
        let parent = final_path
            .parent()
            .ok_or_else(|| AssetError::UnsafePath(workspace_key.into()))?;
        std::fs::create_dir_all(parent)?;
        let staging = parent.join(format!(".staging-{}", Uuid::now_v7()));
        let backup = parent.join(format!(".backup-{}", Uuid::now_v7()));
        write_definition_tree(&staging, files)?;
        let had_previous = final_path.exists();
        if had_previous {
            rename_with_retry(&final_path, &backup)?;
        }
        if let Err(error) = rename_with_retry(&staging, &final_path) {
            if had_previous {
                let _ = rename_with_retry(&backup, &final_path);
            }
            remove_scoped_dir(parent, &staging);
            return Err(error.into());
        }
        Ok(WorkspaceActivation {
            final_path,
            backup: had_previous.then_some(backup),
            committed: false,
        })
    }

    pub(crate) fn deactivate_workspace(&self, workspace_key: &str) -> Result<WorkspaceRemoval, AssetError> {
        let final_path = self.workspace_path(workspace_key)?;
        if !final_path.is_dir() {
            return Err(AssetError::NotFound(workspace_key.into()));
        }
        let parent = final_path
            .parent()
            .ok_or_else(|| AssetError::UnsafePath(workspace_key.into()))?;
        let backup = parent.join(format!(".removing-{}", Uuid::now_v7()));
        rename_with_retry(&final_path, &backup)?;
        Ok(WorkspaceRemoval {
            final_path,
            backup,
            committed: false,
        })
    }
}

pub struct AssetFileLock {
    file: File,
}

impl Drop for AssetFileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub struct WorkspaceActivation {
    final_path: PathBuf,
    backup: Option<PathBuf>,
    committed: bool,
}

impl WorkspaceActivation {
    pub fn commit(mut self) {
        if let Some(backup) = self.backup.take()
            && let Some(parent) = backup.parent()
        {
            remove_scoped_dir(parent, &backup);
        }
        self.committed = true;
    }
}

impl Drop for WorkspaceActivation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(parent) = self.final_path.parent() {
            remove_scoped_dir(parent, &self.final_path);
        }
        if let Some(backup) = self.backup.take() {
            let _ = rename_with_retry(&backup, &self.final_path);
        }
    }
}

pub(crate) struct WorkspaceRemoval {
    final_path: PathBuf,
    backup: PathBuf,
    committed: bool,
}

impl WorkspaceRemoval {
    pub(crate) fn commit(mut self) {
        if let Some(parent) = self.backup.parent() {
            remove_scoped_dir(parent, &self.backup);
        }
        self.committed = true;
    }
}

impl Drop for WorkspaceRemoval {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if !self.final_path.exists() && self.backup.exists() {
            let _ = rename_with_retry(&self.backup, &self.final_path);
        }
    }
}

fn write_definition_tree(root: &Path, files: &[AssetDefinitionFile]) -> Result<(), AssetError> {
    std::fs::create_dir(root)?;
    for file in files {
        let target = crate::definition::join_safe(root, &file.path)?;
        ensure_descendant(root, &target)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        use std::io::Write;
        let mut output = options.open(target)?;
        output.write_all(&file.content)?;
        output.sync_all()?;
    }
    Ok(())
}

fn stable_path_key(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))[..24].to_owned()
}

fn ensure_descendant(root: &Path, candidate: &Path) -> Result<(), AssetError> {
    if !candidate.starts_with(root) || candidate == root {
        return Err(AssetError::UnsafePath(candidate.display().to_string()));
    }
    Ok(())
}

fn remove_scoped_dir(root: &Path, target: &Path) {
    if target != root && target.starts_with(root) {
        let _ = remove_dir_all_with_retry(target);
    }
}

fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    retry_transient_io(|| std::fs::rename(from, to))
}

fn remove_dir_all_with_retry(path: &Path) -> io::Result<()> {
    retry_transient_io(|| match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    })
}

fn retry_transient_io(mut operation: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    const MAX_ATTEMPTS: u32 = 12;
    for attempt in 1..=MAX_ATTEMPTS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) if attempt < MAX_ATTEMPTS && is_transient_filesystem_error(&error) => {
                std::thread::sleep(Duration::from_millis(u64::from(attempt) * 5));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the retry loop always returns on its final attempt")
}

fn is_transient_filesystem_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(32 | 33))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::prepare_definition;

    #[test]
    fn activation_rolls_back_until_committed() {
        let temp = tempfile::tempdir().unwrap();
        let store = AssetContentStore::new(temp.path());
        let key = store.workspace_key("user", "asset");
        let old = vec![AssetDefinitionFile::text("SKILL.md", "old")];
        store.activate_workspace(&key, &old).unwrap().commit();
        {
            let next = vec![AssetDefinitionFile::text("SKILL.md", "new")];
            let _uncommitted = store.activate_workspace(&key, &next).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(store.workspace_path(&key).unwrap().join("SKILL.md")).unwrap(),
            "old"
        );
        let latest = vec![AssetDefinitionFile::text("SKILL.md", "latest")];
        store.activate_workspace(&key, &latest).unwrap().commit();
        assert_eq!(
            std::fs::read_to_string(store.workspace_path(&key).unwrap().join("SKILL.md")).unwrap(),
            "latest"
        );
    }

    #[test]
    fn object_store_is_content_addressed_and_verified() {
        let temp = tempfile::tempdir().unwrap();
        let store = AssetContentStore::new(temp.path());
        let files = vec![AssetDefinitionFile::text("SKILL.md", "# Demo")];
        let digest = prepare_definition(files.clone()).unwrap().1.digest;
        let (first_key, _) = store.ensure_object(files.clone(), &digest).unwrap();
        let (second_key, _) = store.ensure_object(files, &digest).unwrap();
        assert_eq!(first_key, second_key);
        assert!(store.object_path(&first_key).unwrap().is_dir());
    }

    #[test]
    fn removal_restores_until_committed() {
        let temp = tempfile::tempdir().unwrap();
        let store = AssetContentStore::new(temp.path());
        let key = store.workspace_key("user", "asset");
        let files = vec![AssetDefinitionFile::text("SKILL.md", "content")];
        store.activate_workspace(&key, &files).unwrap().commit();
        {
            let _uncommitted = store.deactivate_workspace(&key).unwrap();
            assert!(!store.workspace_path(&key).unwrap().exists());
        }
        assert!(store.workspace_path(&key).unwrap().join("SKILL.md").is_file());
        store.deactivate_workspace(&key).unwrap().commit();
        assert!(!store.workspace_path(&key).unwrap().exists());
    }

    #[test]
    fn partial_workspace_batch_failures_restore_prior_members() {
        let temp = tempfile::tempdir().unwrap();
        let store = AssetContentStore::new(temp.path());
        let first_key = store.workspace_key("user", "first");
        let second_key = store.workspace_key("user", "second");
        let old_first = vec![AssetDefinitionFile::text("SKILL.md", "old first")];
        let old_second = vec![AssetDefinitionFile::text("SKILL.md", "old second")];
        store.activate_workspace(&first_key, &old_first).unwrap().commit();
        store.activate_workspace(&second_key, &old_second).unwrap().commit();

        {
            let _first_activation = store
                .activate_workspace(&first_key, &[AssetDefinitionFile::text("SKILL.md", "new first")])
                .unwrap();
            assert!(store.activate_workspace("", &old_second).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(store.workspace_path(&first_key).unwrap().join("SKILL.md")).unwrap(),
            "old first"
        );

        {
            let _first_removal = store.deactivate_workspace(&first_key).unwrap();
            assert!(store.deactivate_workspace("missing/workspace").is_err());
        }
        assert_eq!(
            std::fs::read_to_string(store.workspace_path(&first_key).unwrap().join("SKILL.md")).unwrap(),
            "old first"
        );
        assert_eq!(
            std::fs::read_to_string(store.workspace_path(&second_key).unwrap().join("SKILL.md")).unwrap(),
            "old second"
        );
    }
}
