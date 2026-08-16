use std::{fs, process::Command};

use tempfile::tempdir;
use tjuaeui_file::{GitFileStatus, GitService, IGitService};

#[tokio::test]
async fn non_repository_is_initialized_on_main_with_clean_initial_commit() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("README.md"), "hello\n").unwrap();
    let service = GitService::new();

    let info = service.ensure(temp.path().to_str().unwrap()).await.unwrap();
    let status = service.status(temp.path().to_str().unwrap()).await.unwrap();

    assert_eq!(info.branch, "main");
    assert!(!info.head_commit.is_empty());
    assert!(temp.path().join(".git").is_dir());
    assert!(status.conflicted.is_empty());
    assert!(status.staged.is_empty());
    assert!(status.unstaged.is_empty());
}

#[tokio::test]
async fn unborn_repository_is_moved_from_generated_branch_to_main_and_committed() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("SKILL.md"), "# Skill\n").unwrap();
    let init = Command::new("git")
        .args(["init"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(init.status.success(), "{}", String::from_utf8_lossy(&init.stderr));
    let symbolic_ref = Command::new("git")
        .args(["symbolic-ref", "HEAD", "refs/heads/codex/generated-skill"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        symbolic_ref.status.success(),
        "{}",
        String::from_utf8_lossy(&symbolic_ref.stderr)
    );

    let service = GitService::new();
    let info = service.ensure(temp.path().to_str().unwrap()).await.unwrap();
    let status = service.status(temp.path().to_str().unwrap()).await.unwrap();

    assert_eq!(info.branch, "main");
    assert!(!info.head_commit.is_empty());
    assert!(status.conflicted.is_empty());
    assert!(status.staged.is_empty());
    assert!(status.unstaged.is_empty());
}

#[tokio::test]
async fn existing_repository_history_and_dirty_state_are_preserved() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("a.txt"), "one\n").unwrap();
    let service = GitService::new();
    let first = service.ensure(temp.path().to_str().unwrap()).await.unwrap();
    fs::write(temp.path().join("a.txt"), "two\n").unwrap();

    let second = service.ensure(temp.path().to_str().unwrap()).await.unwrap();
    let status = service.status(temp.path().to_str().unwrap()).await.unwrap();

    assert_eq!(first.head_commit, second.head_commit);
    assert!(second.dirty);
    assert_eq!(status.unstaged[0].status, GitFileStatus::Modified);
}

#[tokio::test]
async fn child_workspace_gets_an_independent_repository_and_clean_initial_commit() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("root.txt"), "root\n").unwrap();
    let service = GitService::new();
    service.ensure(temp.path().to_str().unwrap()).await.unwrap();

    let child = temp.path().join("packages").join("demo");
    fs::create_dir_all(&child).unwrap();
    fs::write(child.join("demo.txt"), "demo\n").unwrap();

    let info = service.ensure(child.to_str().unwrap()).await.unwrap();
    let status = service.status(child.to_str().unwrap()).await.unwrap();

    assert_eq!(
        std::path::PathBuf::from(&info.repository_root).canonicalize().unwrap(),
        child.canonicalize().unwrap()
    );
    assert_eq!(info.workspace_relative_path, ".");
    assert_eq!(info.branch, "main");
    assert_eq!(info.worktrees.iter().filter(|worktree| worktree.current).count(), 1);
    assert!(child.join(".git").is_dir());
    assert!(status.conflicted.is_empty());
    assert!(status.staged.is_empty());
    assert!(status.unstaged.is_empty());
}

#[tokio::test]
async fn file_history_and_revision_return_only_the_selected_file() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("a.txt"), "one\n").unwrap();
    fs::write(temp.path().join("b.txt"), "other\n").unwrap();
    let service = GitService::new();
    service.ensure(temp.path().to_str().unwrap()).await.unwrap();
    fs::write(temp.path().join("a.txt"), "two\n").unwrap();
    service
        .stage_file(temp.path().to_str().unwrap(), "a.txt")
        .await
        .unwrap();
    let hash = service
        .commit(temp.path().to_str().unwrap(), "update a", false)
        .await
        .unwrap();

    let history = service
        .history(temp.path().to_str().unwrap(), Some("a.txt"), None, 20)
        .await
        .unwrap();
    let revision = service
        .revision(temp.path().to_str().unwrap(), "a.txt", &hash)
        .await
        .unwrap();

    assert_eq!(history[0].subject, "update a");
    assert_eq!(revision.original_content.as_deref(), Some("one\n"));
    assert_eq!(revision.modified_content.as_deref(), Some("two\n"));
    assert!(revision.patch.contains("+two"));
    assert!(!revision.patch.contains("b.txt"));
}

#[tokio::test]
async fn branches_and_worktrees_are_managed_by_the_same_repository() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("README.md"), "hello\n").unwrap();
    let service = GitService::new();
    service.ensure(temp.path().to_str().unwrap()).await.unwrap();

    service
        .create_branch(temp.path().to_str().unwrap(), "feature/ui", None)
        .await
        .unwrap();
    let feature = service.repository_info(temp.path().to_str().unwrap()).await.unwrap();
    assert_eq!(feature.branch, "feature/ui");

    service
        .switch_branch(temp.path().to_str().unwrap(), "main")
        .await
        .unwrap();
    let worktree_parent = temp.path().parent().unwrap();
    let worktree_path = worktree_parent.join(format!("tjuae-worktree-{}", tjuaeui_common::generate_short_id()));
    let worktree = service
        .create_worktree(
            temp.path().to_str().unwrap(),
            worktree_path.to_str().unwrap(),
            "feature/worktree",
            None,
        )
        .await
        .unwrap();
    assert_eq!(worktree.branch.as_deref(), Some("feature/worktree"));
    assert!(!worktree.current);

    let info = service.repository_info(temp.path().to_str().unwrap()).await.unwrap();
    assert!(info.worktrees.iter().any(|entry| entry.path == worktree.path));
    service
        .remove_worktree(temp.path().to_str().unwrap(), &worktree.path)
        .await
        .unwrap();
    assert!(!worktree_path.exists());
}

#[tokio::test]
async fn child_workspace_operations_never_mutate_the_ancestor_repository() {
    let temp = tempdir().unwrap();
    fs::write(temp.path().join("outside.txt"), "one\n").unwrap();
    let service = GitService::new();
    service.ensure(temp.path().to_str().unwrap()).await.unwrap();

    let child = temp.path().join("apps").join("ui");
    fs::create_dir_all(&child).unwrap();
    fs::write(child.join("inside.txt"), "one\n").unwrap();
    service.ensure(child.to_str().unwrap()).await.unwrap();

    fs::write(temp.path().join("outside.txt"), "two\n").unwrap();
    fs::write(child.join("inside.txt"), "two\n").unwrap();
    service
        .stage_file(temp.path().to_str().unwrap(), "outside.txt")
        .await
        .unwrap();
    service.stage_file(child.to_str().unwrap(), "inside.txt").await.unwrap();

    service
        .commit(child.to_str().unwrap(), "update ui", false)
        .await
        .unwrap();

    let parent_status = service.status(temp.path().to_str().unwrap()).await.unwrap();
    let child_status = service.status(child.to_str().unwrap()).await.unwrap();
    assert_eq!(parent_status.staged.len(), 1);
    assert!(child_status.conflicted.is_empty());
    assert!(child_status.staged.is_empty());
    assert!(child_status.unstaged.is_empty());
}
