INSERT OR IGNORE INTO session_turns (
  turn_id,
  session_id,
  run_id,
  user_message_id,
  status,
  start_seq,
  end_seq,
  started_at,
  updated_at,
  assistant_partial,
  thought_partial,
  metrics_json,
  tool_total,
  tool_pending,
  tool_running,
  tool_completed,
  tool_failed
)
SELECT
  m.turn_id,
  m.session_id,
  MAX(m.run_id) AS run_id,
  (
    SELECT um.id
    FROM messages um
    WHERE um.turn_id = m.turn_id AND um.session_id = m.session_id AND um.role = 'user'
    ORDER BY um.created_at ASC, um.turn_sequence ASC
    LIMIT 1
  ) AS user_message_id,
  CASE
    WHEN MAX(CASE WHEN m.role = 'assistant' THEN 1 ELSE 0 END) = 1 THEN 'completed'
    WHEN MAX(CASE WHEN m.delivery = 'queued' AND m.delivered_at IS NULL THEN 1 ELSE 0 END) = 1 THEN 'queued'
    ELSE 'running'
  END AS status,
  NULL AS start_seq,
  NULL AS end_seq,
  MIN(m.created_at) AS started_at,
  MAX(m.created_at) AS updated_at,
  NULL AS assistant_partial,
  NULL AS thought_partial,
  NULL AS metrics_json,
  0 AS tool_total,
  0 AS tool_pending,
  0 AS tool_running,
  0 AS tool_completed,
  0 AS tool_failed
FROM messages m
WHERE m.turn_id IS NOT NULL
GROUP BY m.turn_id, m.session_id;
