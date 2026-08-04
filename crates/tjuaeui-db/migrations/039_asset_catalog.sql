-- Local asset repository metadata. Definition contents live under the Core
-- managed asset root; these tables only store safe identities, digests and
-- content-addressed object references.

CREATE TABLE IF NOT EXISTS asset_records (
    user_id TEXT NOT NULL,
    id TEXT NOT NULL,
    kind TEXT NOT NULL,
    display_name TEXT NOT NULL,
    description TEXT,
    origin TEXT NOT NULL,
    trust TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('system', 'user')),
    editability TEXT NOT NULL,
    workspace_key TEXT NOT NULL,
    definition_digest TEXT NOT NULL,
    entry_file TEXT,
    runtime_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_records_user_kind_updated
    ON asset_records(user_id, kind, updated_at DESC, id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_asset_records_user_runtime
    ON asset_records(user_id, kind, runtime_id)
    WHERE runtime_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS asset_upstreams (
    user_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    package_name TEXT NOT NULL,
    remote_asset_id TEXT NOT NULL,
    version TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    remote_digest TEXT NOT NULL,
    tracking_mode TEXT NOT NULL,
    checked_at INTEGER,
    PRIMARY KEY (user_id, asset_id),
    UNIQUE (user_id, package_name, remote_asset_id),
    FOREIGN KEY (user_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_upstreams_remote
    ON asset_upstreams(package_name, remote_asset_id, source_revision);

CREATE TABLE IF NOT EXISTS asset_snapshots (
    user_id TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    base_digest TEXT NOT NULL,
    object_key TEXT NOT NULL,
    manifest_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(manifest_json) AND json_type(manifest_json) = 'array'),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, asset_id, base_digest),
    FOREIGN KEY (user_id, asset_id)
        REFERENCES asset_records(user_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_snapshots_asset_created
    ON asset_snapshots(user_id, asset_id, created_at DESC);

CREATE TABLE IF NOT EXISTS asset_operations (
    user_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    phase TEXT NOT NULL,
    error_code TEXT,
    recovery_json TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(recovery_json) AND json_type(recovery_json) = 'object'),
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, operation_id),
    UNIQUE (user_id, idempotency_key),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_asset_operations_recovery
    ON asset_operations(state, updated_at)
    WHERE state IN ('queued', 'running');
