CREATE TABLE IF NOT EXISTS github_publish_operations (
    user_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    package_digest TEXT NOT NULL,
    asset_id TEXT NOT NULL,
    extension_name TEXT NOT NULL,
    version TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK (state IN ('running', 'succeeded', 'failed')),
    phase TEXT NOT NULL,
    branch_name TEXT,
    pull_request_url TEXT,
    last_error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, idempotency_key),
    UNIQUE (user_id, operation_id),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_github_publish_operations_recovery
    ON github_publish_operations(user_id, state, updated_at)
    WHERE state IN ('running', 'failed');
