use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, Eq)]
pub struct SkillUserPreferenceRow {
    pub source: String,
    pub namespace: String,
    pub slug: String,
    pub selected_version: Option<String>,
    pub follow_latest: bool,
    pub enabled: bool,
    pub auto_inject: bool,
    pub updated_at: TimestampMs,
}
