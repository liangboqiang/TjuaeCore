-- Immutable, privacy-preserving receipt for the exact assets accepted by one
-- conversation runtime. Local roots and definition contents deliberately have
-- no columns in this schema.

CREATE TABLE IF NOT EXISTS conversation_trace_runtime_asset_snapshots (
    user_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    runtime_snapshot_id TEXT NOT NULL
        CHECK (
            length(runtime_snapshot_id) = 71
            AND runtime_snapshot_id GLOB 'sha256-[0-9a-f]*'
            AND substr(runtime_snapshot_id, 8) NOT GLOB '*[^0-9a-f]*'
        ),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, conversation_id, trace_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (conversation_id, trace_id)
        REFERENCES conversation_traces(conversation_id, trace_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_trace_runtime_asset_snapshots_trace
    ON conversation_trace_runtime_asset_snapshots(conversation_id, trace_id, user_id);

CREATE TABLE IF NOT EXISTS conversation_trace_runtime_asset_refs (
    user_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    local_asset_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    definition_digest TEXT NOT NULL
        CHECK (
            length(definition_digest) = 71
            AND definition_digest GLOB 'sha256-[0-9a-f]*'
            AND substr(definition_digest, 8) NOT GLOB '*[^0-9a-f]*'
        ),
    upstream_package TEXT,
    upstream_asset_id TEXT,
    upstream_version TEXT,
    upstream_revision TEXT,
    PRIMARY KEY (user_id, conversation_id, trace_id, position),
    UNIQUE (user_id, conversation_id, trace_id, local_asset_id, kind),
    FOREIGN KEY (user_id, conversation_id, trace_id)
        REFERENCES conversation_trace_runtime_asset_snapshots(user_id, conversation_id, trace_id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_trace_runtime_asset_refs_trace
    ON conversation_trace_runtime_asset_refs(user_id, conversation_id, trace_id, position);
