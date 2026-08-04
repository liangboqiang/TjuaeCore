-- 资产运行状态、私有 Overlay 和可重建运行投影绑定。
--
-- `user_id` 是实际运行/配置用户，`asset_owner_id` 是 Definition 所有者。
-- 两者分离后，每个用户都能为只读 system seed 保存独立 Overlay，且不能读取
-- 其他用户的私有配置。

CREATE TABLE IF NOT EXISTS asset_runtime_states (
    user_id TEXT NOT NULL,
    asset_owner_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('notConfigured', 'inactive', 'activating', 'active', 'degraded', 'needsRepair')
    ),
    last_error_code TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, asset_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_owner_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_runtime_states_owner
    ON asset_runtime_states(asset_owner_id, asset_id);

CREATE TABLE IF NOT EXISTS asset_overlays (
    user_id TEXT NOT NULL,
    asset_owner_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('assistant', 'engineAdapter', 'skill', 'mcp')),
    overlay_json TEXT NOT NULL CHECK (
        json_valid(overlay_json) AND json_type(overlay_json) = 'object'
    ),
    version INTEGER NOT NULL CHECK (version > 0),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, asset_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_owner_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_overlays_owner
    ON asset_overlays(asset_owner_id, asset_id);

CREATE TABLE IF NOT EXISTS asset_runtime_bindings (
    user_id TEXT NOT NULL,
    asset_owner_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('assistant', 'engineAdapter', 'skill', 'mcp')),
    projection_kind TEXT NOT NULL CHECK (
        projection_kind IN ('assistant', 'engineAdapter', 'skill', 'mcp')
    ),
    runtime_id TEXT NOT NULL,
    definition_digest TEXT NOT NULL,
    overlay_version INTEGER NOT NULL CHECK (overlay_version >= 0),
    health_status TEXT NOT NULL CHECK (health_status IN ('unknown', 'healthy', 'unhealthy')),
    last_error_code TEXT,
    projected_at INTEGER NOT NULL,
    health_checked_at INTEGER,
    PRIMARY KEY (user_id, asset_id),
    UNIQUE (user_id, projection_kind, runtime_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_owner_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_runtime_bindings_owner
    ON asset_runtime_bindings(asset_owner_id, asset_id);

-- 迁移已有本地资产时只建立安全的未运行状态，不创建 Overlay 或运行投影。
INSERT OR IGNORE INTO asset_runtime_states (
    user_id, asset_owner_id, asset_id, state, last_error_code, updated_at
)
SELECT
    user_id,
    user_id,
    id,
    CASE
        WHEN kind IN ('engineAdapter', 'mcp') THEN 'notConfigured'
        ELSE 'inactive'
    END,
    NULL,
    updated_at
FROM asset_records;
