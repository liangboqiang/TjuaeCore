use serde::{Deserialize, Serialize};
use tjuaeui_common::TimestampMs;

/// Row mapping for the `skills` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SkillRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub path: String,
    pub source: String,
    pub enabled: bool,
    pub deleted_at: Option<TimestampMs>,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}
