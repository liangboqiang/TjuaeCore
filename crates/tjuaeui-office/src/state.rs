use std::path::PathBuf;
use std::sync::Arc;

use crate::conversion::ConversionService;
use crate::snapshot::SnapshotService;

#[derive(Clone)]
pub struct OfficeRouterState {
    pub snapshot_service: Arc<SnapshotService>,
    pub conversion_service: Arc<ConversionService>,
    pub allowed_roots: Vec<PathBuf>,
}
