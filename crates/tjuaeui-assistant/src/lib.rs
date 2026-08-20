#![warn(clippy::disallowed_types)]

//! 双来源版本化助手目录、显式资源激活与统一运行时视图。

pub mod activation;
pub mod agent_catalog;
pub mod catalog;
pub mod error;
pub mod routes;
pub mod state;

pub use activation::AssistantActivationService;
pub use agent_catalog::AssistantAgentCatalogPort;
pub use catalog::AssistantCatalogService;
pub use error::AssistantError;
pub use routes::{AssistantRouterState, assistant_asset_routes, assistant_routes};
