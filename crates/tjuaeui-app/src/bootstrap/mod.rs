//! Process-level bootstrap helpers for the binary.
//!
//! These are *not* subcommands — they are layered initialization steps
//! (logging, work_dir resolution, database
//! init) that subcommands compose to start the application.

mod environment;
mod error;
mod instance_guard;
mod parent_exit;
mod tracing_init;
mod work_dir;

pub use environment::{ServerEnvironment, init_data_layer, init_environment};
pub(crate) use error::{BootstrapError, BootstrapErrorCode};
pub(crate) use instance_guard::wait_for_instance_guard;
pub(crate) use parent_exit::{ParentExitSignal, parent_exit_signal};
