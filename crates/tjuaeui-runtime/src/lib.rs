#![warn(clippy::disallowed_types)]

//! Managed runtime and subprocess infrastructure for tjuaecore.

mod agent_env;
mod cache;
mod http_client;
pub mod managed_resources;
pub mod managed_resources_contract;
mod network_proxy;
pub mod node_runtime;
mod registry_npx_lock;
mod resolver;
mod shell_env;

pub use agent_env::agent_process_env;
pub use cache::init;
pub use http_client::{build_http_client, build_streaming_http_client};
pub use managed_resources::{ManagedResourcesMode, managed_resources_mode, set_managed_resources_mode};
pub use network_proxy::{
    NetworkProxyConfig, NetworkProxyMode, NetworkProxySource, NetworkProxyState, NetworkProxyStatus,
    NetworkProxyWarning, apply_network_proxy_to_http_client, network_proxy_status, network_proxy_url_for,
    set_network_proxy_config, validate_network_proxy_config,
};
pub use node_runtime::{
    DoctorRow, NodeRuntimeError, NodeRuntimeFailureKind, NodeRuntimeProgress, NodeRuntimeProgressPhase,
    NodeRuntimeProgressReporter, NodeRuntimeSupport, NodeTool, ResolvedCommand, ResolvedNodeRuntime,
    ResolvedNodeSource, RuntimeCommandProbe, SharedNodeRuntimeProgressReporter, doctor_snapshot,
    doctor_snapshot_for_test, ensure_node_runtime, ensure_node_runtime_with_reporter, ensure_runtime_command,
    ensure_runtime_command_with_reporter, probe_node_runtime_supported, probe_runtime_command,
};
pub use registry_npx_lock::{RegistryNpxLockError, pin_registry_npx_args, should_skip_registry_npx_version_probe};
pub use resolver::{resolve_command_in, resolve_command_path};
pub use shell_env::enhance_process_path;
mod spawn;
pub use spawn::{Builder, kill_process_tree};

#[cfg(test)]
mod test_support;
