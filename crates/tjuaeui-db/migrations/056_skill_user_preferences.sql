CREATE TABLE skill_user_preferences (
    source TEXT NOT NULL,
    namespace TEXT NOT NULL DEFAULT '',
    slug TEXT NOT NULL,
    selected_version TEXT,
    follow_latest INTEGER NOT NULL DEFAULT 1 CHECK (follow_latest IN (0, 1)),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    auto_inject INTEGER NOT NULL DEFAULT 0 CHECK (auto_inject IN (0, 1)),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (source, namespace, slug),
    CHECK (auto_inject = 0 OR enabled = 1)
);

CREATE INDEX idx_skill_user_preferences_enabled
    ON skill_user_preferences(enabled, auto_inject, updated_at);
