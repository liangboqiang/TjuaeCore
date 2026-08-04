pub(crate) mod a2a;
pub(crate) mod acp;

pub(crate) mod engine_adapter_probe;
pub use engine_adapter_probe::{probe_engine_adapter, probe_engine_adapter_in_directory};
pub(crate) mod error;
pub mod events;
pub(crate) mod npx_cache_repair;
pub mod send_error;
