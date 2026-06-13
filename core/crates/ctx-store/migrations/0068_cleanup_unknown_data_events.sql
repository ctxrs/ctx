DROP TABLE IF EXISTS temp._ctx_unknown_data_event_ids;

CREATE TEMP TABLE _ctx_unknown_data_event_ids AS
SELECT id, session_id
FROM (
  SELECT
    id,
    session_id,
    transient,
    json_extract(payload_json, '$.kind') AS kind,
    json_extract(payload_json, '$.crp_channel') AS crp_channel
  FROM session_events
  WHERE event_type = 'notice'
    AND json_valid(payload_json)
)
WHERE kind = 'crp_unknown_event'
  AND crp_channel = 'data'
  AND transient != 0;

DELETE FROM session_events
WHERE id IN (SELECT id FROM _ctx_unknown_data_event_ids);

DELETE FROM session_head_materializations
WHERE session_id IN (
  SELECT DISTINCT session_id FROM _ctx_unknown_data_event_ids
);

DELETE FROM session_active_snapshot_heads
WHERE session_id IN (
  SELECT DISTINCT session_id FROM _ctx_unknown_data_event_ids
);

UPDATE session_snapshot_summaries
SET projection_rev = projection_rev + 1,
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE session_id IN (
  SELECT DISTINCT session_id FROM _ctx_unknown_data_event_ids
);

DROP TABLE temp._ctx_unknown_data_event_ids;
