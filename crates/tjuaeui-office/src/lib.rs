#![warn(clippy::disallowed_types)]

//! Office document format conversion and snapshot management.
pub mod conversion;
pub mod error;
pub mod routes;
pub mod snapshot;
pub mod state;

pub use conversion::ConversionService;
pub use error::OfficeError;
pub use routes::office_routes;
pub use snapshot::SnapshotService;
pub use state::OfficeRouterState;
