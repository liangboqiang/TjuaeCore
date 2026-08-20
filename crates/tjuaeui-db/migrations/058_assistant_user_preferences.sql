CREATE TABLE assistant_user_preferences (
    source TEXT NOT NULL CHECK (source IN ('mine', 'tjuae-hub')),
    namespace TEXT NOT NULL DEFAULT '',
    slug TEXT NOT NULL,
    selected_version TEXT,
    follow_latest INTEGER NOT NULL DEFAULT 1 CHECK (follow_latest IN (0, 1)),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    activation_status TEXT NOT NULL DEFAULT 'inactive',
    activation_fingerprint TEXT,
    resource_bindings TEXT NOT NULL DEFAULT '{}',
    runtime_overrides TEXT NOT NULL DEFAULT '{}',
    sort_order INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (source, namespace, slug)
);

CREATE INDEX idx_assistant_user_preferences_enabled
    ON assistant_user_preferences(enabled, sort_order, updated_at);
