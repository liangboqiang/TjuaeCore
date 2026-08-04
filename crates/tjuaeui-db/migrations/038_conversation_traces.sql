-- Minimal, privacy-preserving execution traces for conversation turns.
-- A trace id is the turn id. These tables must never contain conversation
-- text, thinking text, tool input/output, environment values or credentials.

CREATE TABLE IF NOT EXISTS conversation_traces (
    conversation_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'cancelled', 'interrupted')),
    backend TEXT,
    model TEXT,
    mode TEXT,
    started_at INTEGER NOT NULL,
    first_event_at INTEGER,
    first_output_at INTEGER,
    ended_at INTEGER,
    duration_ms INTEGER,
    input_size INTEGER NOT NULL DEFAULT 0 CHECK (input_size >= 0),
    output_size INTEGER NOT NULL DEFAULT 0 CHECK (output_size >= 0),
    input_tokens INTEGER,
    output_tokens INTEGER,
    total_tokens INTEGER,
    cost_usd REAL,
    error_code TEXT,
    retryable INTEGER,
    incomplete INTEGER NOT NULL DEFAULT 0,
    truncated INTEGER NOT NULL DEFAULT 0,
    span_count INTEGER NOT NULL DEFAULT 0 CHECK (span_count >= 0),
    dropped_span_count INTEGER NOT NULL DEFAULT 0 CHECK (dropped_span_count >= 0),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, trace_id),
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_conversation_traces_conversation_started
    ON conversation_traces(conversation_id, started_at DESC, trace_id DESC);
CREATE INDEX IF NOT EXISTS idx_conversation_traces_running
    ON conversation_traces(status) WHERE status = 'running';

CREATE TABLE IF NOT EXISTS conversation_trace_spans (
    conversation_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('thinking', 'tool', 'permission')),
    source_id TEXT,
    source_message_id TEXT,
    name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'cancelled', 'interrupted')),
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    duration_ms INTEGER,
    safe_attributes TEXT NOT NULL DEFAULT '{}'
        CHECK (
            json_valid(safe_attributes)
            AND json_type(safe_attributes) = 'object'
            AND length(CAST(safe_attributes AS BLOB)) <= 4096
        ),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, trace_id, span_id),
    FOREIGN KEY (conversation_id, trace_id)
        REFERENCES conversation_traces(conversation_id, trace_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_conversation_trace_spans_trace_started
    ON conversation_trace_spans(conversation_id, trace_id, started_at, span_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_conversation_trace_spans_source
    ON conversation_trace_spans(conversation_id, trace_id, kind, source_id)
    WHERE source_id IS NOT NULL;
