pub const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS workflows (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    status      TEXT NOT NULL,
    input_hash  TEXT NOT NULL,
    parent_id   TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workflow_id  TEXT NOT NULL REFERENCES workflows(id),
    sequence     INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    payload      TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE (workflow_id, sequence)
);

CREATE INDEX IF NOT EXISTS idx_events_workflow_seq
    ON events(workflow_id, sequence);
"#;
