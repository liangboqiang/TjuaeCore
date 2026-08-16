use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tjuaeui_common::{WorkspaceGitProvision, WorkspaceGitProvisioner};
use tjuaeui_runtime::Builder as CommandBuilder;
use tokio::sync::Mutex;

use crate::error::FileError;
use crate::traits::IGitService;
use crate::types::{
    GitBranch, GitCommit, GitCommitFile, GitFileChange, GitFileStatus, GitRepositoryInfo, GitRevision, GitStatus,
    GitWorktree,
};

const INITIAL_COMMIT_MESSAGE: &str = "chore: 初始化 Tjuae 工作区";
const LOCAL_GIT_USER_NAME: &str = "Tjuae";
const LOCAL_GIT_USER_EMAIL: &str = "tjuae@localhost";

#[derive(Clone, Default)]
pub struct GitService {
    provision_lock: Arc<Mutex<()>>,
    mutation_locks: Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
}

#[derive(Debug, Clone)]
struct RepositoryContext {
    workspace: PathBuf,
    repository_root: PathBuf,
    scope: String,
}

#[derive(Debug)]
struct GitOutput {
    stdout: Vec<u8>,
}

impl GitService {
    pub fn new() -> Self {
        Self::default()
    }

    async fn ensure_repository(&self, workspace: &Path) -> Result<RepositoryContext, FileError> {
        let workspace = canonical_workspace(workspace)?;
        // Repository discovery and first-time initialization need one short
        // global gate so concurrent root/child requests cannot create nested
        // repositories. Once the repository root is known, all mutations are
        // serialized by that repository's own lock.
        let provision_guard = self.provision_lock.lock().await;

        // Every Tjuae workspace owns its repository.  An ancestor repository
        // must never absorb a project, conversation, team, or skill workspace:
        // otherwise their histories and working-tree changes leak into each
        // other.  A `.git` file is accepted here because a real Git worktree
        // uses that form instead of a directory.
        let mut initialized = false;
        if !workspace.join(".git").exists() {
            run_git(&workspace, ["init", "-b", "main"], Duration::from_secs(30)).await?;
            initialized = true;
        }

        let context = repository_context(&workspace).await?;
        let repository_lock = self.mutation_lock(&context.repository_root);
        let _repository_guard = repository_lock.lock().await;
        drop(provision_guard);
        if initialized || !has_head(&context.repository_root).await {
            ensure_main_head(&context.repository_root).await?;
            ensure_local_identity(&context.repository_root).await?;
            ensure_local_excludes(&context.repository_root)?;
            stage_scope(&context).await?;
            ensure_index_is_scoped(&context).await?;
            run_git(
                &context.repository_root,
                ["commit", "--allow-empty", "-m", INITIAL_COMMIT_MESSAGE],
                Duration::from_secs(30),
            )
            .await?;
        }
        Ok(context)
    }

    async fn context(&self, workspace: &str) -> Result<RepositoryContext, FileError> {
        let workspace = canonical_workspace(Path::new(workspace))?;
        repository_context(&workspace).await
    }

    fn mutation_lock(&self, root: &Path) -> Arc<Mutex<()>> {
        self.mutation_locks
            .entry(root.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn locked_context(&self, workspace: &str) -> Result<(RepositoryContext, Arc<Mutex<()>>), FileError> {
        let context = self.context(workspace).await?;
        let lock = self.mutation_lock(&context.repository_root);
        Ok((context, lock))
    }

    async fn read_repository_info(&self, context: &RepositoryContext) -> Result<GitRepositoryInfo, FileError> {
        let branch = optional_git_stdout(&context.repository_root, ["branch", "--show-current"])
            .await?
            .unwrap_or_else(|| "main".to_owned());
        let head_commit = optional_git_stdout(&context.repository_root, ["rev-parse", "HEAD"])
            .await?
            .unwrap_or_default();
        let upstream = optional_git_stdout(
            &context.repository_root,
            ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
        )
        .await?;
        let (ahead, behind) = match upstream.as_deref() {
            Some(value) => ahead_behind(&context.repository_root, value).await.unwrap_or_default(),
            None => (0, 0),
        };
        let status = parse_status(context).await?;
        let branches = list_branches(context).await?;
        let worktrees = list_worktrees(context).await?;
        let remotes = git_stdout(&context.repository_root, ["remote"])
            .await?
            .lines()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();

        Ok(GitRepositoryInfo {
            repository_root: display_path(&context.repository_root),
            workspace_path: display_path(&context.workspace),
            workspace_relative_path: context.scope.clone(),
            branch,
            head_commit,
            upstream,
            ahead,
            behind,
            dirty: !status.conflicted.is_empty() || !status.staged.is_empty() || !status.unstaged.is_empty(),
            branches,
            worktrees,
            remotes,
        })
    }
}

#[async_trait::async_trait]
impl WorkspaceGitProvisioner for GitService {
    async fn ensure_workspace_git(&self, workspace: &Path) -> Result<WorkspaceGitProvision, String> {
        let context = self
            .ensure_repository(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let info = self
            .read_repository_info(&context)
            .await
            .map_err(|error| error.to_string())?;
        Ok(WorkspaceGitProvision {
            repository_root: info.repository_root,
            workspace_path: info.workspace_path,
            branch: info.branch,
            head_commit: info.head_commit,
        })
    }
}

#[async_trait::async_trait]
impl IGitService for GitService {
    async fn ensure(&self, workspace: &str) -> Result<GitRepositoryInfo, FileError> {
        let context = self.ensure_repository(Path::new(workspace)).await?;
        self.read_repository_info(&context).await
    }

    async fn repository_info(&self, workspace: &str) -> Result<GitRepositoryInfo, FileError> {
        let context = self.context(workspace).await?;
        self.read_repository_info(&context).await
    }

    async fn status(&self, workspace: &str) -> Result<GitStatus, FileError> {
        parse_status(&self.context(workspace).await?).await
    }

    async fn baseline_content(&self, workspace: &str, file_path: &str) -> Result<Option<String>, FileError> {
        let context = self.context(workspace).await?;
        let repo_path = validate_workspace_file(&context, file_path)?;
        show_text(&context.repository_root, &format!("HEAD:{repo_path}")).await
    }

    async fn index_content(&self, workspace: &str, file_path: &str) -> Result<Option<String>, FileError> {
        let context = self.context(workspace).await?;
        let repo_path = validate_workspace_file(&context, file_path)?;
        show_text(&context.repository_root, &format!(":{repo_path}")).await
    }

    async fn stage_file(&self, workspace: &str, file_path: &str) -> Result<(), FileError> {
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        let repo_path = validate_workspace_file(&context, file_path)?;
        run_git(
            &context.repository_root,
            ["add", "-A", "--", &repo_path],
            Duration::from_secs(30),
        )
        .await?;
        Ok(())
    }

    async fn stage_all(&self, workspace: &str) -> Result<(), FileError> {
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        stage_scope(&context).await
    }

    async fn unstage_file(&self, workspace: &str, file_path: &str) -> Result<(), FileError> {
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        let repo_path = validate_workspace_file(&context, file_path)?;
        run_git(
            &context.repository_root,
            ["restore", "--staged", "--", &repo_path],
            Duration::from_secs(30),
        )
        .await?;
        Ok(())
    }

    async fn unstage_all(&self, workspace: &str) -> Result<(), FileError> {
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        let scope = scope_pathspec(&context);
        run_git(
            &context.repository_root,
            ["restore", "--staged", "--", scope.as_str()],
            Duration::from_secs(30),
        )
        .await?;
        Ok(())
    }

    async fn discard_file(&self, workspace: &str, file_path: &str) -> Result<(), FileError> {
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        let repo_path = validate_workspace_file(&context, file_path)?;
        let status = parse_status(&context).await?;
        let is_untracked = status
            .unstaged
            .iter()
            .any(|item| item.relative_path == normalize_relative(file_path) && item.status == GitFileStatus::Untracked);
        if is_untracked {
            let absolute = safe_workspace_child(&context.workspace, file_path)?;
            let metadata = std::fs::symlink_metadata(&absolute)
                .map_err(|error| FileError::Internal(format!("无法读取待删除文件：{error}")))?;
            if metadata.is_dir() {
                std::fs::remove_dir_all(&absolute)
            } else {
                std::fs::remove_file(&absolute)
            }
            .map_err(|error| FileError::Internal(format!("无法放弃未跟踪文件：{error}")))?;
            return Ok(());
        }

        let conflicted = status
            .conflicted
            .iter()
            .any(|item| item.relative_path == normalize_relative(file_path));
        let args = if conflicted {
            vec![
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                repo_path.as_str(),
            ]
        } else {
            vec!["restore", "--worktree", "--", repo_path.as_str()]
        };
        run_git(&context.repository_root, args, Duration::from_secs(30)).await?;
        Ok(())
    }

    async fn history(
        &self,
        workspace: &str,
        file_path: Option<&str>,
        reference: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GitCommit>, FileError> {
        let context = self.context(workspace).await?;
        let limit = limit.clamp(1, 500).to_string();
        let mut args = vec![
            OsString::from("log"),
            OsString::from("--topo-order"),
            OsString::from("--date-order"),
            OsString::from("--decorate=full"),
            OsString::from("-n"),
            OsString::from(limit),
            OsString::from("--format=%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1f%D%x1e"),
        ];
        if let Some(reference) = reference {
            validate_revision(reference)?;
            args.push(OsString::from(reference));
        }
        if let Some(file_path) = file_path {
            let repo_path = validate_workspace_file(&context, file_path)?;
            args.push(OsString::from("--follow"));
            args.push(OsString::from("--"));
            args.push(OsString::from(repo_path));
        } else if context.scope != "." {
            args.push(OsString::from("--"));
            args.push(OsString::from(context.scope.clone()));
        }
        let output = run_git_owned(&context.repository_root, args, Duration::from_secs(30)).await?;
        Ok(parse_history(&String::from_utf8_lossy(&output.stdout)))
    }

    async fn commit_files(&self, workspace: &str, revision: &str) -> Result<Vec<GitCommitFile>, FileError> {
        validate_revision(revision)?;
        let context = self.context(workspace).await?;
        let mut args = vec![
            OsString::from("diff-tree"),
            OsString::from("--root"),
            OsString::from("--no-commit-id"),
            OsString::from("--name-status"),
            OsString::from("-r"),
            OsString::from("-M"),
            OsString::from(revision),
        ];
        if context.scope != "." {
            args.push(OsString::from("--"));
            args.push(OsString::from(context.scope.clone()));
        }
        let output = run_git_owned(&context.repository_root, args, Duration::from_secs(30)).await?;
        Ok(parse_commit_files(&String::from_utf8_lossy(&output.stdout), &context))
    }

    async fn revision(&self, workspace: &str, file_path: &str, revision: &str) -> Result<GitRevision, FileError> {
        validate_revision(revision)?;
        let context = self.context(workspace).await?;
        let repo_path = validate_workspace_file(&context, file_path)?;
        let parent = optional_git_stdout(&context.repository_root, ["rev-parse", &format!("{revision}^")]).await?;
        let modified = show_bytes(&context.repository_root, &format!("{revision}:{repo_path}")).await?;
        let original = match parent.as_deref() {
            Some(parent) => show_bytes(&context.repository_root, &format!("{parent}:{repo_path}")).await?,
            None => None,
        };
        let binary = modified
            .as_ref()
            .is_some_and(|value| std::str::from_utf8(value).is_err())
            || original
                .as_ref()
                .is_some_and(|value| std::str::from_utf8(value).is_err());
        let original_content = original.and_then(|value| String::from_utf8(value).ok());
        let modified_content = modified.and_then(|value| String::from_utf8(value).ok());
        let patch = match parent.as_deref() {
            Some(parent) => {
                git_stdout(
                    &context.repository_root,
                    ["diff", "--no-ext-diff", "--binary", parent, revision, "--", &repo_path],
                )
                .await?
            }
            None => {
                git_stdout(
                    &context.repository_root,
                    [
                        "show",
                        "--format=",
                        "--no-ext-diff",
                        "--binary",
                        revision,
                        "--",
                        &repo_path,
                    ],
                )
                .await?
            }
        };
        Ok(GitRevision {
            revision: revision.to_owned(),
            file_path: normalize_relative(file_path),
            original_revision: parent,
            original_content,
            modified_content,
            patch,
            binary,
        })
    }

    async fn create_branch(&self, workspace: &str, name: &str, start_point: Option<&str>) -> Result<(), FileError> {
        validate_branch(name)?;
        if let Some(start) = start_point {
            validate_revision(start)?;
        }
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        let mut args = vec![OsString::from("switch"), OsString::from("-c"), OsString::from(name)];
        if let Some(start) = start_point {
            args.push(OsString::from(start));
        }
        run_git_owned(&context.repository_root, args, Duration::from_secs(30)).await?;
        Ok(())
    }

    async fn switch_branch(&self, workspace: &str, name: &str) -> Result<(), FileError> {
        validate_branch(name)?;
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        run_git(&context.repository_root, ["switch", name], Duration::from_secs(30)).await?;
        Ok(())
    }

    async fn checkout_revision(&self, workspace: &str, revision: &str) -> Result<(), FileError> {
        validate_revision(revision)?;
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        run_git(
            &context.repository_root,
            ["switch", "--detach", revision],
            Duration::from_secs(30),
        )
        .await?;
        Ok(())
    }

    async fn clone_repository(
        &self,
        repository_url: &str,
        parent_directory: &str,
    ) -> Result<GitRepositoryInfo, FileError> {
        let repository_url = validate_repository_url(repository_url)?;
        let parent = canonical_workspace(Path::new(parent_directory))?;
        let folder_name = repository_folder_name(repository_url)?;
        let destination = parent.join(&folder_name);
        if destination.exists() {
            return Err(FileError::BadRequest(format!("目标文件夹已存在：{folder_name}")));
        }
        run_git_owned(
            &parent,
            vec![
                OsString::from("clone"),
                OsString::from("--origin"),
                OsString::from("origin"),
                OsString::from("--"),
                OsString::from(repository_url),
                OsString::from(&folder_name),
            ],
            Duration::from_secs(300),
        )
        .await?;
        let context = self.ensure_repository(&destination).await?;
        self.read_repository_info(&context).await
    }

    async fn commit(&self, workspace: &str, message: &str, include_unstaged: bool) -> Result<String, FileError> {
        let message = message.trim();
        if message.is_empty() || message.len() > 10_000 {
            return Err(FileError::BadRequest(
                "提交说明不能为空且不能超过 10000 个字符".to_owned(),
            ));
        }
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        if include_unstaged {
            stage_scope(&context).await?;
        }
        ensure_index_is_scoped(&context).await?;
        ensure_local_identity(&context.repository_root).await?;
        run_git(
            &context.repository_root,
            ["commit", "-m", message],
            Duration::from_secs(30),
        )
        .await?;
        git_stdout(&context.repository_root, ["rev-parse", "HEAD"]).await
    }

    async fn fetch(&self, workspace: &str) -> Result<(), FileError> {
        remote_mutation(self, workspace, ["fetch", "--prune"]).await
    }

    async fn pull(&self, workspace: &str) -> Result<(), FileError> {
        require_upstream(&self.context(workspace).await?).await?;
        remote_mutation(self, workspace, ["pull", "--ff-only"]).await
    }

    async fn push(&self, workspace: &str) -> Result<(), FileError> {
        require_upstream(&self.context(workspace).await?).await?;
        remote_mutation(self, workspace, ["push"]).await
    }

    async fn sync(&self, workspace: &str) -> Result<(), FileError> {
        self.pull(workspace).await?;
        self.push(workspace).await
    }

    async fn create_worktree(
        &self,
        workspace: &str,
        path: &str,
        branch: &str,
        start_point: Option<&str>,
    ) -> Result<GitWorktree, FileError> {
        validate_branch(branch)?;
        if let Some(start) = start_point {
            validate_revision(start)?;
        }
        let target = validate_new_worktree_path(path)?;
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        let mut args = vec![
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("-b"),
            OsString::from(branch),
            target.as_os_str().to_os_string(),
        ];
        if let Some(start) = start_point {
            args.push(OsString::from(start));
        }
        run_git_owned(&context.repository_root, args, Duration::from_secs(60)).await?;
        let target_display = display_path(&target);
        list_worktrees(&context)
            .await?
            .into_iter()
            .find(|item| same_path(Path::new(&item.path), &target))
            .ok_or_else(|| FileError::Internal(format!("已创建工作树但无法读取其状态：{target_display}")))
    }

    async fn remove_worktree(&self, workspace: &str, path: &str) -> Result<(), FileError> {
        let (context, lock) = self.locked_context(workspace).await?;
        let _guard = lock.lock().await;
        let requested = PathBuf::from(path);
        let worktree = list_worktrees(&context)
            .await?
            .into_iter()
            .find(|item| same_path(Path::new(&item.path), &requested))
            .ok_or_else(|| FileError::BadRequest("指定路径不是当前仓库的工作树".to_owned()))?;
        if worktree.current {
            return Err(FileError::BadRequest("不能移除当前工作区".to_owned()));
        }
        run_git_owned(
            &context.repository_root,
            vec![
                OsString::from("worktree"),
                OsString::from("remove"),
                OsString::from(worktree.path),
            ],
            Duration::from_secs(60),
        )
        .await?;
        Ok(())
    }
}

async fn remote_mutation<I, S>(service: &GitService, workspace: &str, args: I) -> Result<(), FileError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let (context, lock) = service.locked_context(workspace).await?;
    let _guard = lock.lock().await;
    run_git(&context.repository_root, args, Duration::from_secs(120)).await?;
    Ok(())
}

async fn require_upstream(context: &RepositoryContext) -> Result<(), FileError> {
    if optional_git_stdout(
        &context.repository_root,
        ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    )
    .await?
    .is_none()
    {
        return Err(FileError::BadRequest("当前分支尚未关联远程上游".to_owned()));
    }
    Ok(())
}

async fn discover_repository(workspace: &Path) -> Result<PathBuf, FileError> {
    let root = git_stdout(workspace, ["rev-parse", "--show-toplevel"]).await?;
    canonical_workspace(Path::new(&root))
}

async fn repository_context(workspace: &Path) -> Result<RepositoryContext, FileError> {
    if !workspace.join(".git").exists() {
        return Err(FileError::NotFound("工作区尚未建立独立 Git 仓库".to_owned()));
    }
    let repository_root = discover_repository(workspace)
        .await
        .map_err(|_| FileError::NotFound("工作区尚未建立 Git 仓库".to_owned()))?;
    if repository_root != workspace {
        return Err(FileError::Forbidden("工作区必须使用根目录下的独立 Git 仓库".to_owned()));
    }
    Ok(RepositoryContext {
        workspace: workspace.to_path_buf(),
        repository_root,
        scope: ".".to_owned(),
    })
}

async fn has_head(root: &Path) -> bool {
    optional_git_stdout(root, ["rev-parse", "--verify", "HEAD"])
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn ensure_main_head(root: &Path) -> Result<(), FileError> {
    if has_head(root).await {
        return Ok(());
    }
    run_git(
        root,
        ["symbolic-ref", "HEAD", "refs/heads/main"],
        Duration::from_secs(10),
    )
    .await?;
    Ok(())
}

async fn ensure_local_identity(root: &Path) -> Result<(), FileError> {
    if optional_git_stdout(root, ["config", "user.name"]).await?.is_none() {
        run_git(
            root,
            ["config", "user.name", LOCAL_GIT_USER_NAME],
            Duration::from_secs(10),
        )
        .await?;
    }
    if optional_git_stdout(root, ["config", "user.email"]).await?.is_none() {
        run_git(
            root,
            ["config", "user.email", LOCAL_GIT_USER_EMAIL],
            Duration::from_secs(10),
        )
        .await?;
    }
    Ok(())
}

fn ensure_local_excludes(root: &Path) -> Result<(), FileError> {
    let git_dir = if root.join(".git").is_dir() {
        root.join(".git")
    } else {
        return Ok(());
    };
    let info = git_dir.join("info");
    std::fs::create_dir_all(&info).map_err(|error| FileError::Internal(format!("无法创建 Git info 目录：{error}")))?;
    let exclude = info.join("exclude");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    let managed = "\n# Tjuae workspace defaults\nnode_modules/\ndist/\nbuild/\ntarget/\n.env\n.env.*\n";
    if !existing.contains("# Tjuae workspace defaults") {
        std::fs::write(&exclude, format!("{existing}{managed}"))
            .map_err(|error| FileError::Internal(format!("无法写入 Git 排除规则：{error}")))?;
    }
    Ok(())
}

async fn stage_scope(context: &RepositoryContext) -> Result<(), FileError> {
    let scope = scope_pathspec(context);
    run_git(
        &context.repository_root,
        ["add", "-A", "--", scope.as_str()],
        Duration::from_secs(30),
    )
    .await?;
    Ok(())
}

async fn ensure_index_is_scoped(context: &RepositoryContext) -> Result<(), FileError> {
    if context.scope == "." {
        return Ok(());
    }
    let output = run_git(
        &context.repository_root,
        ["diff", "--cached", "--name-only", "-z"],
        Duration::from_secs(30),
    )
    .await?;
    let prefix = format!("{}/", context.scope.trim_end_matches('/'));
    let outside = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| normalize_relative(&String::from_utf8_lossy(path)))
        .find(|path| path != &context.scope && !path.starts_with(&prefix));
    if let Some(path) = outside {
        return Err(FileError::BadRequest(format!(
            "Git 暂存区还包含当前工作区之外的文件：{path}。请先处理仓库根目录中的这些暂存更改"
        )));
    }
    Ok(())
}

fn scope_pathspec(context: &RepositoryContext) -> String {
    if context.scope == "." {
        ".".to_owned()
    } else {
        context.scope.clone()
    }
}

async fn parse_status(context: &RepositoryContext) -> Result<GitStatus, FileError> {
    let scope = scope_pathspec(context);
    let output = run_git(
        &context.repository_root,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            scope.as_str(),
        ],
        Duration::from_secs(30),
    )
    .await?;
    let records: Vec<&[u8]> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect();
    let mut result = GitStatus::default();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 {
            index += 1;
            continue;
        }
        let x = record[0] as char;
        let y = record[1] as char;
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        let has_rename = matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C');
        let old_path = if has_rename && index + 1 < records.len() {
            index += 1;
            Some(String::from_utf8_lossy(records[index]).into_owned())
        } else {
            None
        };
        index += 1;

        let relative = repo_to_workspace_relative(context, &path)?;
        let old_relative = old_path
            .as_deref()
            .and_then(|old| repo_to_workspace_relative(context, old).ok());
        let absolute = display_path(&context.workspace.join(Path::new(&relative)));
        if is_conflict_code(x, y) {
            result.conflicted.push(GitFileChange {
                file_path: absolute,
                relative_path: relative,
                old_relative_path: old_relative,
                status: GitFileStatus::Conflicted,
            });
            continue;
        }
        if x == '?' && y == '?' {
            result.unstaged.push(GitFileChange {
                file_path: absolute,
                relative_path: relative,
                old_relative_path: None,
                status: GitFileStatus::Untracked,
            });
            continue;
        }
        if let Some(status) = status_from_code(x, false) {
            result.staged.push(GitFileChange {
                file_path: absolute.clone(),
                relative_path: relative.clone(),
                old_relative_path: old_relative.clone(),
                status,
            });
        }
        if let Some(status) = status_from_code(y, false) {
            result.unstaged.push(GitFileChange {
                file_path: absolute,
                relative_path: relative,
                old_relative_path: old_relative,
                status,
            });
        }
    }
    Ok(result)
}

fn is_conflict_code(x: char, y: char) -> bool {
    matches!(
        (x, y),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    )
}

fn status_from_code(code: char, untracked: bool) -> Option<GitFileStatus> {
    if untracked {
        return Some(GitFileStatus::Untracked);
    }
    match code {
        'A' => Some(GitFileStatus::Added),
        'M' | 'T' => Some(GitFileStatus::Modified),
        'D' => Some(GitFileStatus::Deleted),
        'R' | 'C' => Some(GitFileStatus::Renamed),
        _ => None,
    }
}

async fn list_branches(context: &RepositoryContext) -> Result<Vec<GitBranch>, FileError> {
    let current = optional_git_stdout(&context.repository_root, ["branch", "--show-current"])
        .await?
        .unwrap_or_default();
    let checked_out: std::collections::HashSet<String> = list_worktrees(context)
        .await?
        .into_iter()
        .filter_map(|worktree| worktree.branch)
        .collect();
    let output = run_git(
        &context.repository_root,
        [
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)",
            "refs/heads",
        ],
        Duration::from_secs(30),
    )
    .await?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|record| {
            let mut fields = record.trim().split('\0');
            let name = fields.next()?.trim().to_owned();
            if name.is_empty() {
                return None;
            }
            let commit = fields.next().unwrap_or_default().trim().to_owned();
            Some(GitBranch {
                current: name == current,
                checked_out: checked_out.contains(&name),
                name,
                commit,
            })
        })
        .collect())
}

async fn list_worktrees(context: &RepositoryContext) -> Result<Vec<GitWorktree>, FileError> {
    let output = git_stdout(&context.repository_root, ["worktree", "list", "--porcelain"]).await?;
    let mut result = Vec::new();
    for block in output.split("\n\n").filter(|block| !block.trim().is_empty()) {
        let mut path = String::new();
        let mut head = String::new();
        let mut branch = None;
        let mut locked = false;
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = value.to_owned();
            } else if let Some(value) = line.strip_prefix("HEAD ") {
                head = value.to_owned();
            } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
                branch = Some(value.to_owned());
            } else if line == "locked" || line.starts_with("locked ") {
                locked = true;
            }
        }
        if !path.is_empty() {
            result.push(GitWorktree {
                // A Tjuae workspace may be a scoped subdirectory of an
                // ancestor repository. The current worktree is therefore the
                // repository root, not necessarily the selected workspace.
                current: same_path(Path::new(&path), &context.repository_root),
                path,
                branch,
                head,
                locked,
            });
        }
    }
    Ok(result)
}

fn parse_history(output: &str) -> Vec<GitCommit> {
    output
        .split('\x1e')
        .filter_map(|record| {
            let fields: Vec<&str> = record.trim_matches(['\r', '\n']).split('\x1f').collect();
            if fields.len() < 7 || fields[0].is_empty() {
                return None;
            }
            Some(GitCommit {
                hash: fields[0].to_owned(),
                short_hash: fields[1].to_owned(),
                parents: fields[2].split_whitespace().map(str::to_owned).collect(),
                author: fields[3].to_owned(),
                authored_at: fields[4].parse().unwrap_or_default(),
                subject: fields[5].to_owned(),
                decorations: fields[6]
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(normalize_decoration)
                    .collect(),
            })
        })
        .collect()
}

fn parse_commit_files(output: &str, context: &RepositoryContext) -> Vec<GitCommitFile> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let status = *fields.first()?;
            let (status, old_path, path) = if status.starts_with('R') && fields.len() >= 3 {
                (GitFileStatus::Renamed, Some(fields[1]), fields[2])
            } else {
                let path = *fields.get(1)?;
                let status = match status.chars().next()? {
                    'A' => GitFileStatus::Added,
                    'D' => GitFileStatus::Deleted,
                    'M' | 'T' | 'C' => GitFileStatus::Modified,
                    _ => return None,
                };
                (status, None, path)
            };
            Some(GitCommitFile {
                path: workspace_relative_path(context, path),
                old_path: old_path.map(|value| workspace_relative_path(context, value)),
                status,
            })
        })
        .collect()
}

fn workspace_relative_path(context: &RepositoryContext, path: &str) -> String {
    let normalized = normalize_relative(path);
    if context.scope == "." {
        return normalized;
    }
    let prefix = format!("{}/", context.scope.trim_end_matches('/'));
    normalized.strip_prefix(&prefix).unwrap_or(&normalized).to_owned()
}

fn validate_repository_url(value: &str) -> Result<&str, FileError> {
    let value = value.trim();
    let supported = value.starts_with("https://")
        || value.starts_with("http://")
        || value.starts_with("ssh://")
        || value.starts_with("git://")
        || value.starts_with("git@");
    if value.is_empty() || value.len() > 2_048 || value.starts_with('-') || !supported {
        return Err(FileError::BadRequest("请输入有效的 Git 仓库地址".to_owned()));
    }
    Ok(value)
}

fn repository_folder_name(repository_url: &str) -> Result<String, FileError> {
    let without_query = repository_url.split(['?', '#']).next().unwrap_or(repository_url);
    let name = without_query
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .strip_suffix(".git")
        .unwrap_or_else(|| {
            without_query
                .trim_end_matches('/')
                .rsplit(['/', ':'])
                .next()
                .unwrap_or_default()
        });
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(FileError::BadRequest("无法从仓库地址确定本地项目名称".to_owned()));
    }
    Ok(name.to_owned())
}

fn normalize_decoration(value: &str) -> String {
    let reference = value.strip_prefix("HEAD -> ").unwrap_or(value);
    reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/tags/"))
        .or_else(|| reference.strip_prefix("refs/remotes/"))
        .unwrap_or(reference)
        .to_owned()
}

async fn ahead_behind(root: &Path, upstream: &str) -> Result<(u32, u32), FileError> {
    validate_revision(upstream)?;
    let output = git_stdout(
        root,
        ["rev-list", "--left-right", "--count", &format!("HEAD...{upstream}")],
    )
    .await?;
    let mut values = output.split_whitespace();
    Ok((
        values.next().unwrap_or("0").parse().unwrap_or(0),
        values.next().unwrap_or("0").parse().unwrap_or(0),
    ))
}

fn validate_workspace_file(context: &RepositoryContext, file_path: &str) -> Result<String, FileError> {
    let relative = normalize_relative(file_path);
    let _ = safe_workspace_child(&context.workspace, &relative)?;
    Ok(if context.scope == "." {
        relative
    } else {
        format!("{}/{}", context.scope.trim_end_matches('/'), relative)
    })
}

fn safe_workspace_child(workspace: &Path, relative: &str) -> Result<PathBuf, FileError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || relative.trim().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(FileError::BadRequest("文件路径必须是工作区内的相对路径".to_owned()));
    }
    Ok(workspace.join(path))
}

fn repo_to_workspace_relative(context: &RepositoryContext, repo_path: &str) -> Result<String, FileError> {
    let repo_path = normalize_relative(repo_path);
    if context.scope == "." {
        return Ok(repo_path);
    }
    let prefix = format!("{}/", context.scope.trim_end_matches('/'));
    repo_path
        .strip_prefix(&prefix)
        .map(str::to_owned)
        .ok_or_else(|| FileError::Forbidden("Git 返回了工作区范围之外的路径".to_owned()))
}

fn normalize_relative(path: &str) -> String {
    path.trim().replace('\\', "/").trim_start_matches("./").to_owned()
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, FileError> {
    if !path.is_absolute() {
        return Err(FileError::BadRequest("工作区必须是绝对路径".to_owned()));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| FileError::NotFound(format!("无法访问工作区 {}：{error}", path.display())))?;
    if !canonical.is_dir() {
        return Err(FileError::BadRequest("工作区路径不是目录".to_owned()));
    }
    Ok(strip_verbatim_prefix(canonical))
}

#[cfg(windows)]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    value.strip_prefix(r"\\?\").map(PathBuf::from).unwrap_or(path)
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

fn validate_branch(value: &str) -> Result<(), FileError> {
    validate_token(value, "分支名称")?;
    if value.ends_with('.')
        || value.ends_with('/')
        || value.contains("..")
        || value.contains("@{")
        || value.contains("//")
        || value
            .chars()
            .any(|character| character.is_whitespace() || "~^:?*[\\".contains(character))
    {
        return Err(FileError::BadRequest("分支名称不符合 Git 规则".to_owned()));
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), FileError> {
    validate_token(value, "版本标识")?;
    if value
        .chars()
        .any(|character| !(character.is_ascii_alphanumeric() || "/._~^{}@-".contains(character)))
    {
        return Err(FileError::BadRequest("版本标识包含不受支持的字符".to_owned()));
    }
    Ok(())
}

fn validate_token(value: &str, label: &str) -> Result<(), FileError> {
    if value.is_empty() || value.len() > 255 || value.starts_with('-') || value.contains('\0') {
        return Err(FileError::BadRequest(format!("{label}无效")));
    }
    Ok(())
}

fn validate_new_worktree_path(value: &str) -> Result<PathBuf, FileError> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || path.parent().is_none() || path == Path::new("/") {
        return Err(FileError::BadRequest("工作树路径必须是明确的绝对目录".to_owned()));
    }
    if path.exists() {
        return Err(FileError::BadRequest("工作树目标路径已存在".to_owned()));
    }
    let parent = path.parent().expect("validated parent");
    if !parent.is_dir() {
        return Err(FileError::BadRequest("工作树目标的父目录不存在".to_owned()));
    }
    Ok(path)
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left)
        .map(strip_verbatim_prefix)
        .unwrap_or_else(|_| strip_verbatim_prefix(left.to_path_buf()));
    let right = std::fs::canonicalize(right)
        .map(strip_verbatim_prefix)
        .unwrap_or_else(|_| strip_verbatim_prefix(right.to_path_buf()));
    #[cfg(windows)]
    {
        display_path(&left).eq_ignore_ascii_case(&display_path(&right))
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn display_path(path: &Path) -> String {
    strip_verbatim_prefix(path.to_path_buf()).to_string_lossy().into_owned()
}

async fn show_text(root: &Path, spec: &str) -> Result<Option<String>, FileError> {
    Ok(show_bytes(root, spec)
        .await?
        .and_then(|value| String::from_utf8(value).ok()))
}

async fn show_bytes(root: &Path, spec: &str) -> Result<Option<Vec<u8>>, FileError> {
    validate_token(spec, "Git 对象")?;
    match run_git(root, ["show", spec], Duration::from_secs(30)).await {
        Ok(output) => Ok(Some(output.stdout)),
        Err(FileError::BadRequest(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn optional_git_stdout<I, S>(root: &Path, args: I) -> Result<Option<String>, FileError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    match run_git(root, args, Duration::from_secs(30)).await {
        Ok(output) => {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok((!value.is_empty()).then_some(value))
        }
        Err(FileError::BadRequest(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn git_stdout<I, S>(root: &Path, args: I) -> Result<String, FileError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_git(root, args, Duration::from_secs(30)).await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

async fn run_git<I, S>(root: &Path, args: I, timeout: Duration) -> Result<GitOutput, FileError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_git_owned(
        root,
        args.into_iter().map(|value| value.as_ref().to_os_string()).collect(),
        timeout,
    )
    .await
}

async fn run_git_owned(root: &Path, args: Vec<OsString>, timeout: Duration) -> Result<GitOutput, FileError> {
    let mut command = CommandBuilder::clean_cli("git");
    command.current_dir(root).args(&args);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| FileError::Internal("Git 操作超时".to_owned()))?
        .map_err(|error| FileError::Internal(format!("无法启动系统 Git：{error}")))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        let message = if stderr.is_empty() {
            format!("Git 操作失败（退出码 {:?}）", output.status.code())
        } else {
            stderr.clone()
        };
        return Err(FileError::BadRequest(message));
    }
    Ok(GitOutput { stdout: output.stdout })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_option_injection_in_refs() {
        assert!(validate_branch("--force").is_err());
        assert!(validate_revision("--all").is_err());
    }

    #[test]
    fn normalizes_workspace_paths() {
        assert_eq!(normalize_relative(r"src\main.rs"), "src/main.rs");
        assert_eq!(normalize_relative("./SKILL.md"), "SKILL.md");
    }

    #[test]
    fn parses_commit_records() {
        let commits = parse_history("abc\x1fa\x1fp1 p2\x1fTjuae\x1f42\x1fmessage\x1fHEAD -> refs/heads/main\x1e");
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].parents, vec!["p1", "p2"]);
        assert_eq!(commits[0].decorations, vec!["main"]);
    }

    #[test]
    fn validates_clone_sources_and_derives_the_project_folder() {
        let https = validate_repository_url("https://github.com/liangboqiang/TjuaeUI.git").unwrap();
        let ssh = validate_repository_url("git@github.com:liangboqiang/TjuaeCore.git").unwrap();
        assert_eq!(repository_folder_name(https).unwrap(), "TjuaeUI");
        assert_eq!(repository_folder_name(ssh).unwrap(), "TjuaeCore");
        assert!(validate_repository_url("--upload-pack=malicious").is_err());
        assert!(validate_repository_url("C:\\private\\repo").is_err());
    }
}
