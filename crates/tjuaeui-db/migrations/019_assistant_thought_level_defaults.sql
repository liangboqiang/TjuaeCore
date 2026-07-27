ALTER TABLE assistant_definitions
    ADD COLUMN default_thought_level_mode TEXT NOT NULL DEFAULT 'auto'
        CHECK (default_thought_level_mode IN ('auto', 'fixed'));

ALTER TABLE assistant_definitions
    ADD COLUMN default_thought_level_value TEXT;

ALTER TABLE assistant_preferences
    ADD COLUMN last_thought_level_value TEXT;

ALTER TABLE conversation_assistant_snapshots
    ADD COLUMN default_thought_level_mode TEXT NOT NULL DEFAULT 'auto'
        CHECK (default_thought_level_mode IN ('auto', 'fixed'));

ALTER TABLE conversation_assistant_snapshots
    ADD COLUMN resolved_thought_level_value TEXT;

-- 从通用客户端偏好存储中移除已退役的运行时配置和缓存，
-- 避免它们影响助手默认模型。
DELETE FROM client_preferences
WHERE key IN (
    'acp.config',
    'tjuaecli.config',
    'codex.config',
    'acp.cachedModes',
    'acp.cachedInitializeResult',
    'acp.cached_config_options'
);
