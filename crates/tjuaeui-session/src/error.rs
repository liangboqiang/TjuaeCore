//! `SessionError` — crate-owned error (AGENTS.md: Domain crates use crate-owned
//! errors, never a new `AppError`). Distinct from the in-state terminal
//! `ErrorReason` (that models a session that REACHED an error state); this is
//! for fallible operations (spawn, parse-setup) surfaced to the caller.

use tjuaeui_process::ProcessError;

/// Errors from session-control operations (spawning, transport setup).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Failed to spawn or wire the backend process (wraps the 001 layer).
    #[error("进程错误：{0}")]
    Process(#[from] ProcessError),

    /// The agent process's stdio was already taken (take_stdio is once-only).
    #[error("本回合的标准输入输出已被占用")]
    StdioAlreadyTaken,

    /// Internal invariant violation (should not happen; surfaced not panicked).
    #[error("内部错误：{0}")]
    Internal(String),
}
