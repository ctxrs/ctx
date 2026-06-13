ALTER TABLE session_turn_tools ADD COLUMN order_seq INTEGER;

UPDATE session_turn_tools
SET order_seq = (
  SELECT CAST(COALESCE(
    json_extract(se.payload_json, '$.order_seq'),
    json_extract(se.payload_json, '$.orderSeq')
  ) AS INTEGER)
  FROM session_events se
  WHERE se.session_id = session_turn_tools.session_id
    AND se.turn_id = session_turn_tools.turn_id
    AND se.event_type IN ('tool_call', 'tool_call_update', 'tool_result')
    AND (
      json_extract(se.payload_json, '$.tool_call_id') = session_turn_tools.tool_call_id
      OR json_extract(se.payload_json, '$.toolCallId') = session_turn_tools.tool_call_id
      OR json_extract(se.payload_json, '$.update.tool_call_id') = session_turn_tools.tool_call_id
      OR json_extract(se.payload_json, '$.update.toolCallId') = session_turn_tools.tool_call_id
      OR json_extract(se.payload_json, '$.update.rawInput.call_id') = session_turn_tools.tool_call_id
      OR json_extract(se.payload_json, '$.update.raw_input.call_id') = session_turn_tools.tool_call_id
      OR json_extract(se.payload_json, '$.update.toolCall.rawInput.call_id') = session_turn_tools.tool_call_id
    )
    AND COALESCE(
      json_extract(se.payload_json, '$.order_seq'),
      json_extract(se.payload_json, '$.orderSeq')
    ) IS NOT NULL
  ORDER BY CAST(COALESCE(
    json_extract(se.payload_json, '$.order_seq'),
    json_extract(se.payload_json, '$.orderSeq')
  ) AS INTEGER) ASC, se.seq ASC
  LIMIT 1
)
WHERE order_seq IS NULL;
