ALTER TABLE session_turn_tools ADD COLUMN input_truncated INTEGER;
ALTER TABLE session_turn_tools ADD COLUMN input_original_bytes INTEGER;
ALTER TABLE session_turn_tools ADD COLUMN output_truncated INTEGER;
ALTER TABLE session_turn_tools ADD COLUMN output_original_bytes INTEGER;
