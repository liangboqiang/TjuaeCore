use tjuaeui_channel::channel_settings::{ChannelAssistantCatalogEntry, ChannelAssistantCatalogPort};
use tjuaeui_channel::error::ChannelError;

pub struct StaticChannelAssistantCatalog {
    rows: Vec<ChannelAssistantCatalogEntry>,
}

impl StaticChannelAssistantCatalog {
    pub fn empty() -> Self {
        Self { rows: Vec::new() }
    }

    #[allow(dead_code)]
    pub fn new(rows: Vec<ChannelAssistantCatalogEntry>) -> Self {
        Self { rows }
    }
}

#[async_trait::async_trait]
impl ChannelAssistantCatalogPort for StaticChannelAssistantCatalog {
    async fn list_runtime_assistants(&self) -> Result<Vec<ChannelAssistantCatalogEntry>, ChannelError> {
        Ok(self.rows.clone())
    }
}
