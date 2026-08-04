//! HTTP routes for the ai-agent crate, grouped by capability.
//!
//! - [`agent`] — engine-management endpoints (`/api/engines*`) backed by the
//!   internal agent registry.
//!
//! Session-scoped endpoints (mode / model / config / usage /
//! agent-capabilities / slash-commands / side-question / workspace /
//! openclaw-runtime) now live in the `tjuaeui-conversation` crate, where
//! they dispatch through `AgentInstance` via `ConversationService`.

pub mod agent;
pub(crate) mod error_mapping;
pub mod state;

pub use agent::engine_routes;
pub use state::AgentRouterState;
