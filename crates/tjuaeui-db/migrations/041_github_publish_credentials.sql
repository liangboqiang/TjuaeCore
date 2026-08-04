-- Per-user GitHub App device-flow credentials for publishing Core assets.
--
-- Access/refresh/device codes are AES-256-GCM ciphertext produced by Core.
-- The short user code and verification URI are intentionally non-secret and
-- are retained only while a device authorization is pending.
CREATE TABLE IF NOT EXISTS github_publish_credentials (
    user_id TEXT PRIMARY KEY,
    state TEXT NOT NULL
        CHECK (state IN ('disconnected', 'authorizationPending', 'connected', 'insufficientPermissions')),
    access_token_ciphertext TEXT,
    refresh_token_ciphertext TEXT,
    token_type TEXT,
    access_expires_at INTEGER,
    refresh_expires_at INTEGER,
    account_login TEXT,
    scopes_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(scopes_json) AND json_type(scopes_json) = 'array'),
    device_code_ciphertext TEXT,
    user_code TEXT,
    verification_uri TEXT,
    device_expires_at INTEGER,
    poll_interval_seconds INTEGER,
    next_poll_at INTEGER,
    last_error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_github_publish_credentials_state_expiry
    ON github_publish_credentials(state, device_expires_at);
