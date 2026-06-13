DROP TABLE IF EXISTS temp._ctx_legacy_error_sessions;
DROP TABLE IF EXISTS temp._ctx_legacy_turn_error_latest;
DROP TABLE IF EXISTS temp._ctx_turn_finished_latest;
DROP TABLE IF EXISTS temp._ctx_failed_turn_finished_latest;
DROP TABLE IF EXISTS temp._ctx_failed_turn_failure;
DROP TABLE IF EXISTS temp._ctx_failure_projection_sessions;

CREATE TEMP TABLE _ctx_legacy_error_sessions AS
SELECT DISTINCT session_id
FROM session_events
WHERE event_type = 'error';

CREATE TEMP TABLE _ctx_legacy_turn_error_latest AS
SELECT e.session_id, e.turn_id, e.seq, e.payload_json
FROM session_events e
JOIN (
  SELECT session_id, turn_id, MAX(seq) AS seq
  FROM session_events
  WHERE event_type = 'error'
    AND turn_id IS NOT NULL
    AND json_valid(payload_json)
  GROUP BY session_id, turn_id
) latest
  ON latest.session_id = e.session_id
 AND latest.turn_id = e.turn_id
 AND latest.seq = e.seq;

UPDATE session_events
SET event_type = 'turn_finished',
    payload_json = json_set(
      payload_json,
      '$.status',
      'failed',
      '$.message',
      COALESCE(
        CASE
          WHEN json_type(payload_json, '$.message') = 'text'
          THEN json_extract(payload_json, '$.message')
        END,
        CASE
          WHEN json_type(payload_json, '$.error') = 'text'
          THEN json_extract(payload_json, '$.error')
        END,
        'Harness error.'
      )
    )
WHERE event_type = 'error'
  AND turn_id IS NOT NULL
  AND json_valid(payload_json)
  AND EXISTS (
    SELECT 1
    FROM _ctx_legacy_turn_error_latest le
    WHERE le.session_id = session_events.session_id
      AND le.turn_id = session_events.turn_id
      AND le.seq = session_events.seq
  )
  AND NOT EXISTS (
    SELECT 1
    FROM session_events tf
    WHERE tf.session_id = session_events.session_id
      AND tf.turn_id = session_events.turn_id
      AND tf.event_type = 'turn_finished'
      AND tf.seq > session_events.seq
      AND json_valid(tf.payload_json)
      AND json_extract(tf.payload_json, '$.status') IN ('completed', 'failed', 'error', 'interrupted')
  );

UPDATE session_events
SET payload_json = json_set(
  payload_json,
  '$.status',
  'failed',
  '$.message',
  COALESCE(
    CASE
      WHEN json_type(payload_json, '$.message') = 'text'
      THEN json_extract(payload_json, '$.message')
    END,
    CASE
      WHEN json_type(payload_json, '$.error') = 'text'
      THEN json_extract(payload_json, '$.error')
    END,
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.message') = 'text'
          THEN json_extract(le.payload_json, '$.message')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    ),
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.error') = 'text'
          THEN json_extract(le.payload_json, '$.error')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    ),
    'Harness error.'
  ),
  '$.error',
  COALESCE(
    CASE
      WHEN json_type(payload_json, '$.error') = 'text'
      THEN json_extract(payload_json, '$.error')
    END,
    CASE
      WHEN json_type(payload_json, '$.message') = 'text'
      THEN json_extract(payload_json, '$.message')
    END,
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.error') = 'text'
          THEN json_extract(le.payload_json, '$.error')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    ),
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.message') = 'text'
          THEN json_extract(le.payload_json, '$.message')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    ),
    'Harness error.'
  ),
  '$.reason',
  COALESCE(
    CASE
      WHEN json_type(payload_json, '$.reason') = 'text'
      THEN json_extract(payload_json, '$.reason')
    END,
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.reason') = 'text'
          THEN json_extract(le.payload_json, '$.reason')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    )
  ),
  '$.kind',
  COALESCE(
    CASE
      WHEN json_type(payload_json, '$.kind') = 'text'
      THEN json_extract(payload_json, '$.kind')
    END,
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.kind') = 'text'
          THEN json_extract(le.payload_json, '$.kind')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    )
  )
)
WHERE event_type = 'turn_finished'
  AND turn_id IS NOT NULL
  AND json_valid(payload_json)
  AND json_extract(payload_json, '$.status') IN ('failed', 'error')
  AND NOT EXISTS (
    SELECT 1
    FROM session_events later_tf
    WHERE later_tf.session_id = session_events.session_id
      AND later_tf.turn_id = session_events.turn_id
      AND later_tf.event_type = 'turn_finished'
      AND later_tf.seq > session_events.seq
      AND json_valid(later_tf.payload_json)
      AND json_extract(later_tf.payload_json, '$.status') IN ('completed', 'failed', 'error', 'interrupted')
  )
  AND EXISTS (
    SELECT 1
    FROM _ctx_legacy_turn_error_latest le
    WHERE le.session_id = session_events.session_id
      AND le.turn_id = session_events.turn_id
  );

UPDATE session_events
SET payload_json = json_set(
  payload_json,
  '$.status',
  'failed',
      '$.message',
      COALESCE(
        CASE
          WHEN json_type(payload_json, '$.message') = 'text'
          THEN json_extract(payload_json, '$.message')
        END,
        CASE
          WHEN json_type(payload_json, '$.error') = 'text'
          THEN json_extract(payload_json, '$.error')
        END,
        'Harness error.'
      )
    )
WHERE event_type = 'turn_finished'
  AND turn_id IS NOT NULL
  AND json_valid(payload_json)
  AND json_extract(payload_json, '$.status') = 'error';

UPDATE session_events
SET payload_json = json_set(
  payload_json,
  '$.message',
  COALESCE(
    CASE
      WHEN json_type(payload_json, '$.message') = 'text'
      THEN NULLIF(TRIM(json_extract(payload_json, '$.message')), '')
    END,
    CASE
      WHEN json_type(payload_json, '$.error') = 'text'
      THEN NULLIF(TRIM(json_extract(payload_json, '$.error')), '')
    END,
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.message') = 'text'
          THEN NULLIF(TRIM(json_extract(le.payload_json, '$.message')), '')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    ),
    (
      SELECT CASE
          WHEN json_type(le.payload_json, '$.error') = 'text'
          THEN NULLIF(TRIM(json_extract(le.payload_json, '$.error')), '')
        END
      FROM _ctx_legacy_turn_error_latest le
      WHERE le.session_id = session_events.session_id
        AND le.turn_id = session_events.turn_id
    ),
    'Harness error.'
  )
)
WHERE event_type = 'turn_finished'
  AND turn_id IS NOT NULL
  AND json_valid(payload_json)
  AND json_extract(payload_json, '$.status') = 'failed'
  AND (
    json_type(payload_json, '$.message') IS NULL
    OR json_type(payload_json, '$.message') != 'text'
    OR NULLIF(TRIM(json_extract(payload_json, '$.message')), '') IS NULL
  );

CREATE TEMP TABLE _ctx_turn_finished_latest AS
SELECT e.session_id, e.turn_id, e.seq, e.created_at, e.payload_json
FROM session_events e
JOIN (
  SELECT session_id, turn_id, MAX(seq) AS seq
  FROM session_events
  WHERE event_type = 'turn_finished'
    AND turn_id IS NOT NULL
    AND json_valid(payload_json)
    AND json_extract(payload_json, '$.status') IN ('completed', 'failed', 'interrupted')
  GROUP BY session_id, turn_id
) latest
  ON latest.session_id = e.session_id
 AND latest.turn_id = e.turn_id
 AND latest.seq = e.seq;

CREATE TEMP TABLE _ctx_failed_turn_finished_latest AS
SELECT session_id, turn_id, seq, created_at, payload_json
FROM _ctx_turn_finished_latest
WHERE json_extract(payload_json, '$.status') = 'failed';

CREATE TEMP TABLE _ctx_failed_turn_failure AS
SELECT
  tf.session_id,
  tf.turn_id,
  tf.seq AS end_seq,
  tf.created_at AS updated_at,
  json_object(
    'message',
    COALESCE(
      CASE
        WHEN json_type(tf.payload_json, '$.message') = 'text'
        THEN json_extract(tf.payload_json, '$.message')
      END,
      CASE
        WHEN json_type(tf.payload_json, '$.error') = 'text'
        THEN json_extract(tf.payload_json, '$.error')
      END,
      CASE
        WHEN json_type(le.payload_json, '$.message') = 'text'
        THEN json_extract(le.payload_json, '$.message')
      END,
      CASE
        WHEN json_type(le.payload_json, '$.error') = 'text'
        THEN json_extract(le.payload_json, '$.error')
      END,
      'Harness error.'
    ),
    'details',
    COALESCE(
      json_extract(tf.payload_json, '$.details'),
      json_extract(le.payload_json, '$.details')
    ),
    'kind',
    COALESCE(
      CASE
        WHEN json_type(tf.payload_json, '$.kind') = 'text'
        THEN json_extract(tf.payload_json, '$.kind')
      END,
      CASE
        WHEN json_type(le.payload_json, '$.kind') = 'text'
        THEN json_extract(le.payload_json, '$.kind')
      END
    ),
    'reason',
    COALESCE(
      CASE
        WHEN json_type(tf.payload_json, '$.reason') = 'text'
        THEN json_extract(tf.payload_json, '$.reason')
      END,
      CASE
        WHEN json_type(le.payload_json, '$.reason') = 'text'
        THEN json_extract(le.payload_json, '$.reason')
      END
    ),
    'provider',
    COALESCE(
      CASE
        WHEN json_type(tf.payload_json, '$.provider') = 'text'
        THEN json_extract(tf.payload_json, '$.provider')
      END,
      CASE
        WHEN json_type(le.payload_json, '$.provider') = 'text'
        THEN json_extract(le.payload_json, '$.provider')
      END
    ),
    'provider_id',
    COALESCE(
      CASE
        WHEN json_type(tf.payload_json, '$.provider_id') = 'text'
        THEN json_extract(tf.payload_json, '$.provider_id')
      END,
      CASE
        WHEN json_type(tf.payload_json, '$.providerId') = 'text'
        THEN json_extract(tf.payload_json, '$.providerId')
      END,
      CASE
        WHEN json_type(le.payload_json, '$.provider_id') = 'text'
        THEN json_extract(le.payload_json, '$.provider_id')
      END,
      CASE
        WHEN json_type(le.payload_json, '$.providerId') = 'text'
        THEN json_extract(le.payload_json, '$.providerId')
      END
    )
  ) AS failure_json
FROM _ctx_failed_turn_finished_latest tf
LEFT JOIN _ctx_legacy_turn_error_latest le
  ON le.session_id = tf.session_id
 AND le.turn_id = tf.turn_id;

UPDATE session_turns
SET status = 'failed',
    end_seq = COALESCE(
      (
        SELECT failure.end_seq
        FROM _ctx_failed_turn_failure failure
        WHERE failure.session_id = session_turns.session_id
          AND failure.turn_id = session_turns.turn_id
      ),
      end_seq
    ),
    updated_at = COALESCE(
      (
        SELECT failure.updated_at
        FROM _ctx_failed_turn_failure failure
        WHERE failure.session_id = session_turns.session_id
          AND failure.turn_id = session_turns.turn_id
      ),
      updated_at
    ),
    failure_json = (
      SELECT failure.failure_json
      FROM _ctx_failed_turn_failure failure
      WHERE failure.session_id = session_turns.session_id
        AND failure.turn_id = session_turns.turn_id
    )
WHERE EXISTS (
  SELECT 1
  FROM _ctx_failed_turn_failure failure
  WHERE failure.session_id = session_turns.session_id
    AND failure.turn_id = session_turns.turn_id
);

UPDATE session_turns
SET failure_json = NULL
WHERE status != 'failed'
  AND failure_json IS NOT NULL;

UPDATE session_events
SET event_type = 'notice',
    payload_json = json_set(
      payload_json,
      '$.kind',
      COALESCE(
        CASE
          WHEN json_type(payload_json, '$.kind') = 'text'
          THEN json_extract(payload_json, '$.kind')
        END,
        'legacy_error'
      )
    )
WHERE event_type = 'error'
  AND turn_id IS NULL
  AND json_valid(payload_json);

DELETE FROM session_events
WHERE event_type = 'error';

CREATE TEMP TABLE _ctx_failure_projection_sessions AS
SELECT session_id FROM _ctx_legacy_error_sessions
UNION
SELECT DISTINCT session_id FROM _ctx_failed_turn_failure;

DELETE FROM session_head_materializations
WHERE session_id IN (
  SELECT session_id FROM _ctx_failure_projection_sessions
);

DELETE FROM session_active_snapshot_heads
WHERE session_id IN (
  SELECT session_id FROM _ctx_failure_projection_sessions
);

UPDATE session_snapshot_summaries
SET projection_rev = projection_rev + 1,
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE session_id IN (
  SELECT session_id FROM _ctx_failure_projection_sessions
);

DROP TABLE temp._ctx_failure_projection_sessions;
DROP TABLE temp._ctx_failed_turn_failure;
DROP TABLE temp._ctx_failed_turn_finished_latest;
DROP TABLE temp._ctx_turn_finished_latest;
DROP TABLE temp._ctx_legacy_turn_error_latest;
DROP TABLE temp._ctx_legacy_error_sessions;
