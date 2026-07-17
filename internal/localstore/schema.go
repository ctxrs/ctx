package localstore

var schemaStatements = []string{
	`CREATE TABLE IF NOT EXISTS meta(
		key TEXT PRIMARY KEY,
		value TEXT NOT NULL
	)`,
	`CREATE TABLE IF NOT EXISTS sources(
		id INTEGER PRIMARY KEY,
		source_key TEXT NOT NULL UNIQUE,
		provider TEXT NOT NULL,
		source_format TEXT NOT NULL,
		uri TEXT NOT NULL,
		identity_token TEXT NOT NULL DEFAULT '',
		metadata_json TEXT NOT NULL DEFAULT '',
		size_bytes INTEGER NOT NULL DEFAULT 0,
		tail_hash TEXT NOT NULL DEFAULT '',
		active_generation_id INTEGER,
		created_at TEXT NOT NULL,
		updated_at TEXT NOT NULL,
		FOREIGN KEY(active_generation_id) REFERENCES source_generations(id)
	)`,
	`CREATE TABLE IF NOT EXISTS source_generations(
		id INTEGER PRIMARY KEY,
		source_id INTEGER NOT NULL,
		kind TEXT NOT NULL CHECK(kind IN ('replace', 'append')),
		state TEXT NOT NULL CHECK(state IN ('building', 'active', 'stale')),
		source_identity TEXT NOT NULL,
		base_size_bytes INTEGER NOT NULL DEFAULT 0,
		base_tail_hash TEXT NOT NULL DEFAULT '',
		high_water_bytes INTEGER NOT NULL DEFAULT 0,
		tail_hash TEXT NOT NULL DEFAULT '',
		created_at TEXT NOT NULL,
		activated_at TEXT,
		stale_at TEXT,
		cleanup_marked_at TEXT,
		FOREIGN KEY(source_id) REFERENCES sources(id) ON DELETE CASCADE
	)`,
	`CREATE INDEX IF NOT EXISTS idx_source_generations_source_state
		ON source_generations(source_id, state)`,
	`CREATE TABLE IF NOT EXISTS source_revisions(
		id INTEGER PRIMARY KEY,
		source_id INTEGER NOT NULL,
		generation_id INTEGER NOT NULL,
		source_identity TEXT NOT NULL,
		previous_size_bytes INTEGER NOT NULL,
		previous_tail_hash TEXT NOT NULL,
		size_bytes INTEGER NOT NULL,
		tail_hash TEXT NOT NULL,
		page_start_offset INTEGER NOT NULL,
		page_end_offset INTEGER NOT NULL,
		created_at TEXT NOT NULL,
		UNIQUE(generation_id, page_start_offset, page_end_offset, size_bytes, tail_hash),
		FOREIGN KEY(source_id) REFERENCES sources(id) ON DELETE CASCADE,
		FOREIGN KEY(generation_id) REFERENCES source_generations(id) ON DELETE CASCADE
	)`,
	`CREATE TABLE IF NOT EXISTS events(
		id INTEGER PRIMARY KEY,
		generation_id INTEGER NOT NULL,
		source_id INTEGER NOT NULL,
		source_event_id TEXT NOT NULL,
		provider_session_id TEXT NOT NULL DEFAULT '',
		provider_event_index INTEGER NOT NULL DEFAULT 0,
		role TEXT NOT NULL DEFAULT '',
		event_type TEXT NOT NULL DEFAULT '',
		occurred_at TEXT,
		text TEXT NOT NULL DEFAULT '',
		metadata_json TEXT NOT NULL DEFAULT '',
		source_offset INTEGER NOT NULL DEFAULT 0,
		source_end_offset INTEGER NOT NULL DEFAULT 0,
		created_at TEXT NOT NULL,
		UNIQUE(generation_id, source_event_id),
		FOREIGN KEY(source_id) REFERENCES sources(id) ON DELETE CASCADE,
		FOREIGN KEY(generation_id) REFERENCES source_generations(id) ON DELETE CASCADE
	)`,
	`CREATE INDEX IF NOT EXISTS idx_events_generation ON events(generation_id)`,
	`CREATE INDEX IF NOT EXISTS idx_events_source ON events(source_id)`,
	`CREATE VIRTUAL TABLE IF NOT EXISTS event_fts USING fts5(
		text,
		content='events',
		content_rowid='id',
		tokenize='unicode61'
	)`,
	`CREATE TRIGGER IF NOT EXISTS events_ai AFTER INSERT ON events BEGIN
		INSERT INTO event_fts(rowid, text) VALUES (new.id, new.text);
	END`,
	`CREATE TRIGGER IF NOT EXISTS events_ad AFTER DELETE ON events BEGIN
		INSERT INTO event_fts(event_fts, rowid, text) VALUES('delete', old.id, old.text);
	END`,
	`CREATE TABLE IF NOT EXISTS jobs(
		id INTEGER PRIMARY KEY,
		job_key TEXT NOT NULL UNIQUE,
		job_type TEXT NOT NULL,
		state TEXT NOT NULL CHECK(state IN ('pending', 'running', 'failed', 'completed')),
		payload_json TEXT NOT NULL DEFAULT '',
		attempts INTEGER NOT NULL DEFAULT 0,
		available_at TEXT NOT NULL,
		leased_until TEXT,
		last_error TEXT NOT NULL DEFAULT '',
		created_at TEXT NOT NULL,
		updated_at TEXT NOT NULL
	)`,
	`CREATE INDEX IF NOT EXISTS idx_jobs_ready ON jobs(state, available_at, id)`,
}
