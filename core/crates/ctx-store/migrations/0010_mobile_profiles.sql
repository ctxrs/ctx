CREATE TABLE IF NOT EXISTS mobile_connection_profiles (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    base_url TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    token_prefix TEXT NOT NULL,
    scopes_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT
);

CREATE TABLE IF NOT EXISTS mobile_devices (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    device_label TEXT,
    platform TEXT,
    push_token TEXT,
    push_provider TEXT,
    public_key TEXT,
    app_version TEXT,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    FOREIGN KEY(profile_id) REFERENCES mobile_connection_profiles(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_mobile_devices_profile_id ON mobile_devices(profile_id);
CREATE INDEX IF NOT EXISTS idx_mobile_connection_profiles_token_hash ON mobile_connection_profiles(token_hash);
