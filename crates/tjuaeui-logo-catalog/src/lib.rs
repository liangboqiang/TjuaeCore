#![warn(clippy::disallowed_types)]

//! Backend-served static logo assets.
pub mod error;
pub mod routes;
pub mod service;
pub mod state;

pub use error::LogoAssetError;
pub use routes::logo_asset_routes;
pub use service::LogoCatalogService;
pub use state::LogoCatalogRouterState;
