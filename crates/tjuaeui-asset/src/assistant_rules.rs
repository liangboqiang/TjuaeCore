//! Assistant rule dispatch used by the runtime skill router.

use crate::AssetError;

/// Canonical read-only access for assistant rule files.
///
/// Implemented by `tjuaeui_assistant::AssistantService`; every operation is
/// user-scoped and backed by the assistant AssetCatalog Definition.
#[async_trait::async_trait]
pub trait AssistantRuleDispatcher: Send + Sync {
    async fn read_rule(&self, user_id: &str, id: &str, locale: Option<&str>) -> Result<String, AssetError>;
}
