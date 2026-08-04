CREATE TABLE IF NOT EXISTS a2a_delegation_permissions (
    id TEXT PRIMARY KEY NOT NULL,
    parent_task_id TEXT NOT NULL,
    target_agent_ids_json TEXT NOT NULL,
    scopes_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'approved', 'revoked', 'expired')),
    capability_token_hash TEXT,
    requested_expires_at INTEGER NOT NULL,
    approved_at INTEGER,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (parent_task_id) REFERENCES a2a_tasks(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_a2a_delegation_permissions_parent
    ON a2a_delegation_permissions(parent_task_id, created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_a2a_delegation_permissions_token
    ON a2a_delegation_permissions(capability_token_hash)
    WHERE capability_token_hash IS NOT NULL;

CREATE TABLE IF NOT EXISTS a2a_delegations (
    id TEXT PRIMARY KEY NOT NULL,
    parent_task_id TEXT NOT NULL,
    child_task_id TEXT,
    target_agent_id TEXT NOT NULL,
    permission_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL,
    context_id TEXT,
    last_error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (parent_task_id) REFERENCES a2a_tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (child_task_id) REFERENCES a2a_tasks(id) ON DELETE SET NULL,
    FOREIGN KEY (target_agent_id) REFERENCES a2a_agent_profiles(agent_id) ON DELETE CASCADE,
    FOREIGN KEY (permission_id) REFERENCES a2a_delegation_permissions(id) ON DELETE RESTRICT,
    UNIQUE(parent_task_id, target_agent_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_a2a_delegations_parent
    ON a2a_delegations(parent_task_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_a2a_delegations_child
    ON a2a_delegations(child_task_id);

CREATE TABLE IF NOT EXISTS a2a_audit_events (
    id TEXT PRIMARY KEY NOT NULL,
    event_type TEXT NOT NULL,
    actor_agent_id TEXT,
    target_agent_id TEXT,
    task_id TEXT,
    delegation_id TEXT,
    metadata_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_a2a_audit_events_task
    ON a2a_audit_events(task_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_a2a_audit_events_agent
    ON a2a_audit_events(actor_agent_id, target_agent_id, created_at DESC);
