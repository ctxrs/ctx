ALTER TABLE session_turns ADD COLUMN failure_json TEXT;

CREATE INDEX IF NOT EXISTS idx_session_events_session_turn_type_seq
    ON session_events(session_id, turn_id, event_type, seq);
