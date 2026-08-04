#![warn(clippy::disallowed_types)]

//! Local assistant management.
//!
//! Official assistant definitions are distributed by TjuaeHub and installed
//! through the asset runtime projector. This crate deliberately owns no
//! embedded assistant corpus or second "official" registry.

pub mod agent_catalog;
pub mod asset_definition;
pub mod error;
pub mod routes;
pub mod service;
pub mod state;

pub use agent_catalog::AssistantAgentCatalogPort;
pub use error::AssistantError;
pub use routes::{AssistantRouterState, assistant_routes};
pub use service::{AssistantAvatarAsset, AssistantService};
