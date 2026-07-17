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
		parent_provider_session_id TEXT NOT NULL DEFAULT '',
		root_provider_session_id TEXT NOT NULL DEFAULT '',
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
	`CREATE VIEW IF NOT EXISTS ctx_sessions AS
		SELECT
			src.source_key || '#session:' || COALESCE(NULLIF(e.provider_session_id, ''), src.source_key) AS ctx_session_id,
			NULL AS history_record_id,
			CASE
				WHEN MAX(e.parent_provider_session_id) = '' THEN NULL
				ELSE src.source_key || '#session:' || MAX(e.parent_provider_session_id)
			END AS parent_ctx_session_id,
			src.source_key || '#session:' || COALESCE(NULLIF(MAX(e.root_provider_session_id), ''), NULLIF(e.provider_session_id, ''), src.source_key) AS root_ctx_session_id,
			src.provider AS provider,
			e.provider_session_id AS provider_session_id,
			'' AS external_agent_id,
			'primary' AS agent_type,
			'' AS role_hint,
			1 AS is_primary,
			gen.state AS status,
			'lossless' AS fidelity,
			CAST(strftime('%s', MIN(e.occurred_at)) AS INTEGER) * 1000 AS started_at_ms,
			CAST(strftime('%s', MAX(e.occurred_at)) AS INTEGER) * 1000 AS ended_at_ms,
			'' AS cwd,
			src.uri AS source_path,
			src.source_key AS source_key,
			gen.id AS generation_id
		FROM events e
		JOIN sources src ON src.id = e.source_id AND src.active_generation_id = e.generation_id
		JOIN source_generations gen ON gen.id = e.generation_id AND gen.state = 'active'
		GROUP BY src.source_key, src.provider, src.uri, gen.id, gen.state, e.provider_session_id`,
	`CREATE VIEW IF NOT EXISTS ctx_events AS
		SELECT
			src.source_key || '#event:' || e.source_event_id AS ctx_event_id,
			src.source_key || '#session:' || COALESCE(NULLIF(e.provider_session_id, ''), src.source_key) AS ctx_session_id,
			NULL AS history_record_id,
			src.provider AS provider,
			e.provider_session_id AS provider_session_id,
			e.provider_event_index AS event_seq,
			e.event_type AS event_type,
			e.role AS role,
			CAST(strftime('%s', e.occurred_at) AS INTEGER) * 1000 AS occurred_at_ms,
			CASE WHEN e.metadata_json = '' THEN '{}' ELSE e.metadata_json END AS payload_json,
			'lossless' AS fidelity,
			'' AS cwd,
			src.uri AS source_path,
			src.source_key AS source_key,
			e.source_event_id AS source_event_id,
			e.text AS text
		FROM events e
		JOIN sources src ON src.id = e.source_id AND src.active_generation_id = e.generation_id
		JOIN source_generations gen ON gen.id = e.generation_id AND gen.state = 'active'`,
	`CREATE VIEW IF NOT EXISTS ctx_files_touched AS
		SELECT
			NULL AS ctx_file_touch_id,
			NULL AS path,
			NULL AS old_path,
			NULL AS change_kind,
			NULL AS line_count_delta,
			NULL AS confidence,
			NULL AS ctx_event_id,
			NULL AS ctx_session_id,
			NULL AS history_record_id,
			NULL AS provider,
			NULL AS provider_session_id,
			NULL AS created_at_ms,
			NULL AS updated_at_ms
		WHERE 0`,
	`CREATE VIEW IF NOT EXISTS ctx_sources AS
		SELECT
			src.provider AS provider,
			src.source_format AS source_format,
			src.uri AS source_root,
			src.uri AS source_path,
			'' AS provider_session_id,
			'' AS parent_provider_session_id,
			'primary' AS agent_type,
			'' AS role_hint,
			'' AS cwd,
			NULL AS session_started_at_ms,
			src.size_bytes AS file_size_bytes,
			CAST(strftime('%s', src.updated_at) AS INTEGER) * 1000 AS file_modified_at_ms,
			CAST(strftime('%s', src.created_at) AS INTEGER) * 1000 AS cataloged_at_ms,
			CASE WHEN src.active_generation_id IS NULL THEN 'pending' ELSE 'indexed' END AS indexed_status,
			CAST(strftime('%s', src.updated_at) AS INTEGER) * 1000 AS indexed_at_ms,
			'' AS indexed_error,
			(
				SELECT count(*)
				FROM events e
				WHERE e.source_id = src.id AND e.generation_id = src.active_generation_id
			) AS indexed_event_count,
			src.source_key AS source_key
		FROM sources src`,
}
