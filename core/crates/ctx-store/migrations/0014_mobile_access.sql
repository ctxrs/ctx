CREATE TABLE IF NOT EXISTS mobile_access_config (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    tunnel_id TEXT NOT NULL,
    public_base_url TEXT NOT NULL,
    relay_base_url TEXT NOT NULL,
    tunnel_secret TEXT NOT NULL,
    daemon_public_key TEXT NOT NULL,
    daemon_private_key TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS mobile_pairing_tokens (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

ALTER TABLE mobile_devices ADD COLUMN last_seen_seq INTEGER;
