//! Repository traits for the current assistant definition, overlay, and preference tables.

use crate::error::DbError;
use crate::models::{
    AssistantDefinitionRow, AssistantOverlayRow, AssistantPreferenceRow, UpsertAssistantDefinitionParams,
    UpsertAssistantOverlayParams, UpsertAssistantPreferenceParams,
};

/// 用户创建或由本地引擎派生的运行时助手定义。
///
/// 本地资产与市场资产的来源身份记录在 `source_ref` 中，例如 `asset:*` 与 `market:*`。
#[async_trait::async_trait]
pub trait IAssistantDefinitionRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<AssistantDefinitionRow>, DbError>;
    async fn list_including_deleted(&self) -> Result<Vec<AssistantDefinitionRow>, DbError> {
        self.list().await
    }
    async fn get_by_assistant_id(&self, assistant_id: &str) -> Result<Option<AssistantDefinitionRow>, DbError>;
    async fn get_by_assistant_id_including_deleted(
        &self,
        assistant_id: &str,
    ) -> Result<Option<AssistantDefinitionRow>, DbError> {
        self.get_by_assistant_id(assistant_id).await
    }
    async fn get_by_id(&self, id: &str) -> Result<Option<AssistantDefinitionRow>, DbError>;
    async fn get_by_source_ref(
        &self,
        source: &str,
        source_ref: &str,
    ) -> Result<Option<AssistantDefinitionRow>, DbError>;
    async fn get_by_source_ref_including_deleted(
        &self,
        source: &str,
        source_ref: &str,
    ) -> Result<Option<AssistantDefinitionRow>, DbError> {
        self.get_by_source_ref(source, source_ref).await
    }
    async fn upsert(&self, params: &UpsertAssistantDefinitionParams<'_>) -> Result<AssistantDefinitionRow, DbError>;
    async fn update_avatar_fields_preserving_deleted(
        &self,
        id: &str,
        avatar_type: &str,
        avatar_value: Option<&str>,
    ) -> Result<Option<AssistantDefinitionRow>, DbError> {
        let _ = (id, avatar_type, avatar_value);
        Err(DbError::Init(
            "update_avatar_fields_preserving_deleted is not supported by this repository".to_string(),
        ))
    }
    async fn soft_delete(&self, id: &str, deleted_at: i64) -> Result<bool, DbError>;
}

/// Runtime per-user assistant overlay used by the current app version.
#[async_trait::async_trait]
pub trait IAssistantOverlayRepository: Send + Sync {
    async fn get(&self, assistant_definition_id: &str) -> Result<Option<AssistantOverlayRow>, DbError>;
    async fn list(&self) -> Result<Vec<AssistantOverlayRow>, DbError>;
    async fn upsert(&self, params: &UpsertAssistantOverlayParams<'_>) -> Result<AssistantOverlayRow, DbError>;
    async fn delete(&self, assistant_definition_id: &str) -> Result<bool, DbError>;
}

/// Assistant-scoped "auto remember last" preferences.
#[async_trait::async_trait]
pub trait IAssistantPreferenceRepository: Send + Sync {
    async fn get(&self, assistant_definition_id: &str) -> Result<Option<AssistantPreferenceRow>, DbError>;
    async fn upsert(&self, params: &UpsertAssistantPreferenceParams<'_>) -> Result<AssistantPreferenceRow, DbError>;
    async fn delete(&self, assistant_definition_id: &str) -> Result<bool, DbError>;
}
