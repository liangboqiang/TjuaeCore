-- 将可移植 runtimeId 与 Core 内部全局投影 ID 彻底分离。
--
-- 旧 binding/receipt 只有一个含义不明确的 runtime_id，无法证明它是否按
-- 用户隔离。这里采取破坏性迁移：失效全部旧回执与绑定，并要求用户重新
-- 校验、试跑和激活。Definition、Overlay、凭据和 builtin 运行表均不删除。

UPDATE asset_runtime_states
SET state = CASE
        WHEN (
            SELECT record.kind
            FROM asset_records record
            WHERE record.user_id = asset_runtime_states.asset_owner_id
              AND record.id = asset_runtime_states.asset_id
        ) IN ('engineAdapter', 'mcp')
        AND NOT EXISTS (
            SELECT 1
            FROM asset_overlays overlay
            WHERE overlay.user_id = asset_runtime_states.user_id
              AND overlay.asset_owner_id = asset_runtime_states.asset_owner_id
              AND overlay.asset_id = asset_runtime_states.asset_id
        )
            THEN 'notConfigured'
        ELSE 'inactive'
    END,
    last_error_code = 'RUNTIME_PROJECTION_ID_MIGRATION_REQUIRED',
    updated_at = CAST(strftime('%s', 'now') AS INTEGER) * 1000;

DROP TABLE asset_runtime_bindings;
DROP TABLE asset_try_run_receipts;

CREATE TABLE asset_try_run_receipts (
    user_id TEXT NOT NULL,
    asset_owner_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    receipt_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (
        length(trim(idempotency_key)) BETWEEN 1 AND 128
    ),
    definition_digest TEXT NOT NULL,
    overlay_version INTEGER NOT NULL CHECK (overlay_version >= 0),
    portable_runtime_id TEXT NOT NULL CHECK (
        length(trim(portable_runtime_id)) BETWEEN 1 AND 128
    ),
    projection_runtime_id TEXT NOT NULL CHECK (
        length(projection_runtime_id) = 78
        AND projection_runtime_id LIKE 'tjuae-proj-v1-%'
        AND substr(projection_runtime_id, 15) NOT GLOB '*[^0-9a-f]*'
    ),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, asset_id),
    UNIQUE (user_id, receipt_id),
    UNIQUE (user_id, idempotency_key),
    UNIQUE (projection_runtime_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_owner_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX idx_asset_try_run_receipts_owner
    ON asset_try_run_receipts(asset_owner_id, asset_id);

CREATE TABLE asset_runtime_bindings (
    user_id TEXT NOT NULL,
    asset_owner_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('assistant', 'engineAdapter', 'skill', 'mcp')),
    projection_kind TEXT NOT NULL CHECK (
        projection_kind IN ('assistant', 'engineAdapter', 'skill', 'mcp')
    ),
    portable_runtime_id TEXT NOT NULL CHECK (
        length(trim(portable_runtime_id)) BETWEEN 1 AND 128
    ),
    projection_runtime_id TEXT NOT NULL CHECK (
        length(projection_runtime_id) = 78
        AND projection_runtime_id LIKE 'tjuae-proj-v1-%'
        AND substr(projection_runtime_id, 15) NOT GLOB '*[^0-9a-f]*'
    ),
    definition_digest TEXT NOT NULL,
    overlay_version INTEGER NOT NULL CHECK (overlay_version >= 0),
    health_status TEXT NOT NULL CHECK (health_status IN ('unknown', 'healthy', 'unhealthy')),
    -- 激活时必须由仓储事务校验有效回执；Definition 后续变更会保留投影
    -- 绑定并把该字段清空，强制下次显式激活前重新试跑。
    try_run_receipt_id TEXT,
    last_error_code TEXT,
    projected_at INTEGER NOT NULL,
    health_checked_at INTEGER,
    PRIMARY KEY (user_id, asset_id),
    UNIQUE (user_id, projection_kind, portable_runtime_id),
    UNIQUE (projection_runtime_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (asset_owner_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX idx_asset_runtime_bindings_owner
    ON asset_runtime_bindings(asset_owner_id, asset_id);

CREATE INDEX idx_asset_runtime_bindings_user_kind
    ON asset_runtime_bindings(user_id, kind);
