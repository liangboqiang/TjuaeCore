//! 二进制程序的进程级启动辅助模块。
//!
//! 这些模块不是子命令，而是由各子命令组合使用的分层初始化步骤：
//! 日志初始化、工作目录解析、托管资源准备和数据库初始化。

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
