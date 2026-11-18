CREATE TABLE modes (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    system_prompt TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    mode_id TEXT REFERENCES modes(id),
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX sessions_started_at ON sessions(started_at);

CREATE TABLE transcript_segments (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    speaker TEXT NOT NULL,
    segment_id INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    text TEXT NOT NULL,
    is_final INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER,
    UNIQUE (session_id, speaker, segment_id)
);

CREATE TABLE answers (
    id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    origin TEXT NOT NULL,
    question TEXT NOT NULL,
    text TEXT NOT NULL,
    model TEXT NOT NULL,
    latency_ms INTEGER,
    state TEXT NOT NULL,
    usage TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX answers_session ON answers(session_id);

CREATE TABLE mode_files (
    id TEXT PRIMARY KEY NOT NULL,
    mode_id TEXT NOT NULL REFERENCES modes(id) ON DELETE CASCADE,
    file_name TEXT NOT NULL,
    text TEXT NOT NULL,
    token_estimate INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);

CREATE TABLE chunks (
    id TEXT PRIMARY KEY NOT NULL,
    mode_file_id TEXT NOT NULL REFERENCES mode_files(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    embedding BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX chunks_file ON chunks(mode_file_id);

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
