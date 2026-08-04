-- Split catalog provenance from runtime load evidence.
--
-- `local_definition_digest` is the AssetCatalog Definition digest exposed by
-- Trace. `runtime_content_digest` is retained internally so persistence can
-- verify that the stored snapshot is the exact runtime-produced receipt.
--
-- The old rows carried only a runtime-effective digest and therefore cannot
-- be upgraded into truthful catalog provenance. Drop those receipt headers
-- and refs instead of manufacturing a localDefinitionDigest.

DELETE FROM conversation_trace_runtime_asset_snapshots;
DROP TABLE conversation_trace_runtime_asset_refs;

CREATE TABLE conversation_trace_runtime_asset_refs (
    user_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    local_asset_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    local_definition_digest TEXT NOT NULL
        CHECK (
            length(local_definition_digest) = 71
            AND local_definition_digest GLOB 'sha256-[0-9a-f]*'
            AND substr(local_definition_digest, 8) NOT GLOB '*[^0-9a-f]*'
        ),
    runtime_content_digest TEXT NOT NULL
        CHECK (
            length(runtime_content_digest) = 71
            AND runtime_content_digest GLOB 'sha256-[0-9a-f]*'
            AND substr(runtime_content_digest, 8) NOT GLOB '*[^0-9a-f]*'
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

CREATE INDEX idx_trace_runtime_asset_refs_trace
    ON conversation_trace_runtime_asset_refs(user_id, conversation_id, trace_id, position);
