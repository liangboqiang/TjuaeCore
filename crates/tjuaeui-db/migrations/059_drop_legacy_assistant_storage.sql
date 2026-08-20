-- The assistant catalog and assistant_user_preferences are now the only
-- assistant definition and preference sources. Conversation snapshots keep a
-- frozen catalog identity for replay, but do not reference a mutable assistant
-- definition row.

DROP TABLE IF EXISTS conversation_assistant_snapshots;
DROP TABLE IF EXISTS assistant_preferences;
DROP TABLE IF EXISTS assistant_overlays;
DROP TABLE IF EXISTS assistant_definitions;
DROP TABLE IF EXISTS assistant_overrides;
DROP TABLE IF EXISTS assistants;

CREATE TABLE conversation_assistant_snapshots (
    conversation_id                    TEXT PRIMARY KEY,
    assistant_catalog_id               TEXT    NOT NULL,
    assistant_id                       TEXT    NOT NULL,
    assistant_source                   TEXT    NOT NULL,
    agent_id                           TEXT    NOT NULL,
    rules_content                      TEXT    NOT NULL DEFAULT '',
    default_model_mode                 TEXT    NOT NULL,
    resolved_model_id                  TEXT,
    default_permission_mode            TEXT    NOT NULL,
    resolved_permission_value          TEXT,
    default_thought_level_mode         TEXT    NOT NULL,
    resolved_thought_level_value       TEXT,
    default_skills_mode                TEXT    NOT NULL,
    resolved_skill_ids                 TEXT    NOT NULL DEFAULT '[]',
    resolved_disabled_builtin_skill_ids TEXT   NOT NULL DEFAULT '[]',
    default_mcps_mode                  TEXT    NOT NULL,
    resolved_mcp_ids                   TEXT    NOT NULL DEFAULT '[]',
    created_at                         INTEGER NOT NULL,
    updated_at                         INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);

CREATE INDEX idx_conversation_assistant_snapshots_catalog_id
    ON conversation_assistant_snapshots(assistant_catalog_id);
