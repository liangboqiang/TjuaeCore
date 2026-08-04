use std::collections::{BTreeMap, BTreeSet};

use tjuaeui_api_types::{AssetDiffFileResponse, AssetDiffFileStatus, AssetFileEntryResponse};

use crate::{AssetDefinitionFile, AssetError, digest_bytes};

const MAX_MERGE_MATRIX_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LineEdit {
    start: usize,
    end: usize,
    replacement: Vec<String>,
}

/// Compares every path in the complete Base/Local/Remote Definition.
///
/// Callers must pass already-normalized and verified file sets. Unchanged files
/// are deliberately retained so the UI can present a complete, truthful tree.
pub(crate) fn compare_definitions(
    base: &[AssetDefinitionFile],
    local: &[AssetDefinitionFile],
    remote: &[AssetDefinitionFile],
) -> Vec<AssetDiffFileResponse> {
    let base = file_map(base);
    let local = file_map(local);
    let remote = file_map(remote);
    let paths = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .copied()
        .collect::<BTreeSet<_>>();

    paths
        .into_iter()
        .map(|path| {
            let base_content = base.get(path).copied();
            let local_content = local.get(path).copied();
            let remote_content = remote.get(path).copied();
            let (status, auto_mergeable) = classify_file(base_content, local_content, remote_content);
            AssetDiffFileResponse {
                path: path.to_owned(),
                base: base_content.map(|content| file_entry(path, content)),
                local: local_content.map(|content| file_entry(path, content)),
                remote: remote_content.map(|content| file_entry(path, content)),
                base_digest: base_content.map(digest_bytes),
                local_digest: local_content.map(digest_bytes),
                remote_digest: remote_content.map(digest_bytes),
                status,
                auto_mergeable,
            }
        })
        .collect()
}

fn file_entry(path: &str, content: &[u8]) -> AssetFileEntryResponse {
    AssetFileEntryResponse {
        path: path.to_owned(),
        digest: digest_bytes(content),
        size: content.len() as u64,
        media_type: mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_owned(),
        text: std::str::from_utf8(content).is_ok(),
    }
}

/// Produces a complete merged Definition. It is fail-closed: any binary,
/// delete/modify, add/add or overlapping text edit aborts the whole merge.
pub(crate) fn merge_definitions(
    base: &[AssetDefinitionFile],
    local: &[AssetDefinitionFile],
    remote: &[AssetDefinitionFile],
) -> Result<Vec<AssetDefinitionFile>, AssetError> {
    let base = file_map(base);
    let local = file_map(local);
    let remote = file_map(remote);
    let paths = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut merged = Vec::new();
    let mut conflicts = Vec::new();

    for path in paths {
        let base_content = base.get(path).copied();
        let local_content = local.get(path).copied();
        let remote_content = remote.get(path).copied();
        match merge_file(base_content, local_content, remote_content) {
            Ok(Some(content)) => merged.push(AssetDefinitionFile {
                path: path.to_owned(),
                content,
            }),
            Ok(None) => {}
            Err(()) => conflicts.push(path.to_owned()),
        }
    }

    if conflicts.is_empty() {
        Ok(merged)
    } else {
        Err(AssetError::MergeConflict(conflicts))
    }
}

fn file_map(files: &[AssetDefinitionFile]) -> BTreeMap<&str, &[u8]> {
    files
        .iter()
        .map(|file| (file.path.as_str(), file.content.as_slice()))
        .collect()
}

fn classify_file(base: Option<&[u8]>, local: Option<&[u8]>, remote: Option<&[u8]>) -> (AssetDiffFileStatus, bool) {
    if local == remote {
        return if local == base {
            (AssetDiffFileStatus::Unchanged, true)
        } else {
            (AssetDiffFileStatus::Converged, true)
        };
    }
    if remote == base {
        return (
            match (base, local) {
                (None, Some(_)) => AssetDiffFileStatus::LocalAdded,
                (Some(_), None) => AssetDiffFileStatus::LocalDeleted,
                _ => AssetDiffFileStatus::LocalModified,
            },
            true,
        );
    }
    if local == base {
        return (
            match (base, remote) {
                (None, Some(_)) => AssetDiffFileStatus::RemoteAdded,
                (Some(_), None) => AssetDiffFileStatus::RemoteDeleted,
                _ => AssetDiffFileStatus::RemoteModified,
            },
            true,
        );
    }
    if merge_file(base, local, remote).is_ok() {
        (AssetDiffFileStatus::Diverged, true)
    } else {
        (AssetDiffFileStatus::Conflict, false)
    }
}

fn merge_file(base: Option<&[u8]>, local: Option<&[u8]>, remote: Option<&[u8]>) -> Result<Option<Vec<u8>>, ()> {
    if local == remote {
        return Ok(local.map(ToOwned::to_owned));
    }
    if local == base {
        return Ok(remote.map(ToOwned::to_owned));
    }
    if remote == base {
        return Ok(local.map(ToOwned::to_owned));
    }

    let (Some(base), Some(local), Some(remote)) = (base, local, remote) else {
        return Err(());
    };
    let base = std::str::from_utf8(base).map_err(|_| ())?;
    let local = std::str::from_utf8(local).map_err(|_| ())?;
    let remote = std::str::from_utf8(remote).map_err(|_| ())?;
    merge_text(base, local, remote).map(|content| Some(content.into_bytes()))
}

fn merge_text(base: &str, local: &str, remote: &str) -> Result<String, ()> {
    let base_lines = split_lines(base);
    let local_lines = split_lines(local);
    let remote_lines = split_lines(remote);
    let local_edits = line_edits(&base_lines, &local_lines)?;
    let remote_edits = line_edits(&base_lines, &remote_lines)?;
    let mut edits = local_edits;

    for remote_edit in remote_edits {
        if let Some(existing) = edits.iter().find(|local_edit| edits_overlap(local_edit, &remote_edit)) {
            if existing != &remote_edit {
                return Err(());
            }
        } else {
            edits.push(remote_edit);
        }
    }
    edits.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then(left.end.cmp(&right.end))
            .then(left.replacement.cmp(&right.replacement))
    });
    edits.dedup();

    let mut output = String::new();
    let mut cursor = 0;
    for edit in edits {
        if edit.start < cursor {
            return Err(());
        }
        for line in &base_lines[cursor..edit.start] {
            output.push_str(line);
        }
        for line in edit.replacement {
            output.push_str(&line);
        }
        cursor = edit.end;
    }
    for line in &base_lines[cursor..] {
        output.push_str(line);
    }
    Ok(output)
}

fn split_lines(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    value.split_inclusive('\n').map(ToOwned::to_owned).collect()
}

/// Builds replacement hunks using an LCS table. Definitions are bounded to
/// 1 MiB per file; the additional cell limit prevents pathological CPU/RAM use.
fn line_edits(base: &[String], side: &[String]) -> Result<Vec<LineEdit>, ()> {
    let rows = base.len().checked_add(1).ok_or(())?;
    let columns = side.len().checked_add(1).ok_or(())?;
    if rows.checked_mul(columns).ok_or(())? > MAX_MERGE_MATRIX_CELLS {
        return Err(());
    }
    let mut lcs = vec![0_u32; rows * columns];
    for base_index in (0..base.len()).rev() {
        for side_index in (0..side.len()).rev() {
            let value = if base[base_index] == side[side_index] {
                1 + lcs[(base_index + 1) * columns + side_index + 1]
            } else {
                lcs[(base_index + 1) * columns + side_index].max(lcs[base_index * columns + side_index + 1])
            };
            lcs[base_index * columns + side_index] = value;
        }
    }

    let mut edits = Vec::new();
    let (mut base_index, mut side_index) = (0, 0);
    let (mut edit_start, mut replacement) = (None, Vec::new());
    while base_index < base.len() || side_index < side.len() {
        if base_index < base.len() && side_index < side.len() && base[base_index] == side[side_index] {
            if let Some(start) = edit_start.take() {
                edits.push(LineEdit {
                    start,
                    end: base_index,
                    replacement: std::mem::take(&mut replacement),
                });
            }
            base_index += 1;
            side_index += 1;
        } else if side_index < side.len()
            && (base_index == base.len()
                || lcs[base_index * columns + side_index + 1] >= lcs[(base_index + 1) * columns + side_index])
        {
            edit_start.get_or_insert(base_index);
            replacement.push(side[side_index].clone());
            side_index += 1;
        } else {
            edit_start.get_or_insert(base_index);
            base_index += 1;
        }
    }
    if let Some(start) = edit_start {
        edits.push(LineEdit {
            start,
            end: base_index,
            replacement,
        });
    }
    Ok(edits)
}

fn edits_overlap(left: &LineEdit, right: &LineEdit) -> bool {
    if left.start == right.start && left.end == right.end {
        return true;
    }
    if left.start == left.end && right.start == right.end {
        return left.start == right.start;
    }
    if left.start == left.end {
        return left.start > right.start && left.start < right.end;
    }
    if right.start == right.end {
        return right.start > left.start && right.start < left.end;
    }
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(content: impl Into<Vec<u8>>) -> Vec<AssetDefinitionFile> {
        vec![AssetDefinitionFile {
            path: "SKILL.md".into(),
            content: content.into(),
        }]
    }

    #[test]
    fn non_overlapping_text_changes_merge_without_markers() {
        let base = file(b"one\ntwo\nthree\n".to_vec());
        let local = file(b"ONE\ntwo\nthree\n".to_vec());
        let remote = file(b"one\ntwo\nTHREE\n".to_vec());
        let merged = merge_definitions(&base, &local, &remote).unwrap();
        assert_eq!(merged[0].content, b"ONE\ntwo\nTHREE\n");
        let comparison = compare_definitions(&base, &local, &remote);
        assert_eq!(comparison[0].status, AssetDiffFileStatus::Diverged);
        assert!(comparison[0].auto_mergeable);
    }

    #[test]
    fn overlapping_text_changes_fail_closed() {
        let base = file(b"one\ntwo\n".to_vec());
        let local = file(b"one\nLOCAL\n".to_vec());
        let remote = file(b"one\nREMOTE\n".to_vec());
        assert!(matches!(
            merge_definitions(&base, &local, &remote),
            Err(AssetError::MergeConflict(paths)) if paths == ["SKILL.md"]
        ));
        let comparison = compare_definitions(&base, &local, &remote);
        assert_eq!(comparison[0].status, AssetDiffFileStatus::Conflict);
        assert!(!comparison[0].auto_mergeable);
    }

    #[test]
    fn binary_changes_on_both_sides_fail_closed() {
        let base = file(vec![0xff, 0]);
        let local = file(vec![0xff, 1]);
        let remote = file(vec![0xff, 2]);
        assert!(merge_definitions(&base, &local, &remote).is_err());
    }
}
