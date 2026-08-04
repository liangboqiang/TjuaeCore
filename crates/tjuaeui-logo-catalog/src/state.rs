use std::sync::Arc;

use crate::service::LogoCatalogService;

/// Shared state for the public asset router.
#[derive(Clone)]
pub struct LogoCatalogRouterState {
    pub service: Arc<LogoCatalogService>,
}

impl Default for LogoCatalogRouterState {
    fn default() -> Self {
        Self {
            service: Arc::new(LogoCatalogService),
        }
    }
}
