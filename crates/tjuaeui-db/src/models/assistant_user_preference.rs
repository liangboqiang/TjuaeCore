use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, Eq)]
pub struct AssistantUserPreferenceRow {
    pub source: String,
    pub namespace: String,
    pub slug: String,
    pub selected_version: Option<String>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub activation_status: String,
    pub activation_fingerprint: Option<String>,
    pub resource_bindings: String,
    pub runtime_overrides: String,
    pub sort_order: i32,
    pub last_used_at: Option<TimestampMs>,
    pub updated_at: TimestampMs,
}
