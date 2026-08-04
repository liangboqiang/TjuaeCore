CREATE TABLE IF NOT EXISTS a2a_credentials (
    id               TEXT PRIMARY KEY NOT NULL,
    auth_kind        TEXT NOT NULL,
    header_name      TEXT,
    encrypted_secret TEXT,
    metadata_json    TEXT,
    origin           TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS a2a_agent_profiles (
    agent_id              TEXT PRIMARY KEY NOT NULL,
    card_url              TEXT NOT NULL,
    base_url              TEXT NOT NULL,
    display_name          TEXT,
    allow_insecure        INTEGER NOT NULL DEFAULT 0,
    compatibility_mode    TEXT NOT NULL DEFAULT 'v1'
                                  CHECK(compatibility_mode IN ('v1', 'v0_3')),
    raw_card_json          TEXT,
    normalized_card_json  TEXT,
    extended_card_json    TEXT,
    protocol_version      TEXT,
    selected_binding      TEXT,
    selected_interface_url TEXT,
    credential_ref        TEXT,
    etag                  TEXT,
    last_modified         TEXT,
    cache_expires_at      INTEGER,
    fetched_at            INTEGER,
    card_hash             TEXT,
    signature_status      TEXT NOT NULL DEFAULT 'unchecked',
    trust_status          TEXT NOT NULL DEFAULT 'untrusted',
    trusted_origin        TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    FOREIGN KEY (agent_id) REFERENCES agent_metadata(id) ON DELETE CASCADE,
    FOREIGN KEY (credential_ref) REFERENCES a2a_credentials(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_a2a_agent_profiles_card_url ON a2a_agent_profiles(card_url);
CREATE INDEX IF NOT EXISTS idx_a2a_agent_profiles_cache_expiry ON a2a_agent_profiles(cache_expires_at);

CREATE TABLE IF NOT EXISTS a2a_tasks (
    id                       TEXT PRIMARY KEY NOT NULL,
    conversation_id          TEXT NOT NULL,
    agent_id                 TEXT NOT NULL,
    remote_task_id           TEXT,
    context_id               TEXT,
    state                    TEXT NOT NULL,
    interface_snapshot_json  TEXT NOT NULL,
    last_event_id            TEXT,
    artifact_snapshot_json   TEXT,
    push_config_json         TEXT,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
    FOREIGN KEY (agent_id) REFERENCES a2a_agent_profiles(agent_id) ON DELETE CASCADE,
    UNIQUE(agent_id, remote_task_id)
);
CREATE INDEX IF NOT EXISTS idx_a2a_tasks_conversation ON a2a_tasks(conversation_id);
CREATE INDEX IF NOT EXISTS idx_a2a_tasks_agent_state ON a2a_tasks(agent_id, state);
