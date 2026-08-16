#![warn(clippy::disallowed_types)]

//! File system operations: read/write, path safety, file watching, persistent Git, and zip.
pub mod browse;
pub mod error;
pub mod git_service;
pub mod path_safety;
pub mod routes;
pub mod service;
pub mod traits;
pub mod types;
pub mod watch_service;

pub use error::FileError;
pub use git_service::GitService;
pub use path_safety::{has_traversal, validate_path, validate_path_for_write};
pub use routes::{BrowseRoots, FileRouterState, file_routes};
pub use service::FileService;
pub use traits::{FileServiceRef, FileWatchServiceRef, GitServiceRef, IFileService, IFileWatchService, IGitService};
pub use types::{
    ContentUpdateEvent, ContentUpdateOperation, CopyResult, DirOrFile, FileMetadata, FileWatchEvent, GitBranch,
    GitCommit, GitFileChange, GitFileStatus, GitRepositoryInfo, GitRevision, GitStatus, GitWorktree,
    OfficeFileAddedEvent, WorkspaceFlatFile, ZipEntry,
};
pub use watch_service::FileWatchService;
