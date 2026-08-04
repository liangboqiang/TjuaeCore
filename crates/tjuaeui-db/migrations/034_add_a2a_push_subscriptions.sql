CREATE TABLE IF NOT EXISTS a2a_push_subscriptions (
    id TEXT PRIMARY KEY NOT NULL,
    agent_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    config_id TEXT NOT NULL,
    callback_url TEXT NOT NULL,
    path_secret_hash TEXT NOT NULL,
    notification_token_hash TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (agent_id) REFERENCES a2a_agent_profiles(agent_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_a2a_push_subscriptions_agent
    ON a2a_push_subscriptions(agent_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_a2a_push_subscriptions_task
    ON a2a_push_subscriptions(agent_id, task_id);

CREATE TABLE IF NOT EXISTS a2a_push_deliveries (
    id TEXT PRIMARY KEY NOT NULL,
    subscription_id TEXT NOT NULL,
    event_key TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    task_id TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    FOREIGN KEY (subscription_id) REFERENCES a2a_push_subscriptions(id) ON DELETE CASCADE,
    UNIQUE(subscription_id, event_key)
);

CREATE INDEX IF NOT EXISTS idx_a2a_push_deliveries_rate
    ON a2a_push_deliveries(subscription_id, received_at DESC);
