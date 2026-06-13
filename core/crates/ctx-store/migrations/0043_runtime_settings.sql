CREATE TABLE IF NOT EXISTS runtime_settings (
    id TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    settings_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
