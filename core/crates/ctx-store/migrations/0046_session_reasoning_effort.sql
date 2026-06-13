ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;

UPDATE sessions
SET
  reasoning_effort = CASE
    WHEN lower(trim(model_id)) LIKE '%/none' THEN 'none'
    WHEN lower(trim(model_id)) LIKE '%/minimal' THEN 'minimal'
    WHEN lower(trim(model_id)) LIKE '%/low' THEN 'low'
    WHEN lower(trim(model_id)) LIKE '%/medium' THEN 'medium'
    WHEN lower(trim(model_id)) LIKE '%/high' THEN 'high'
    WHEN lower(trim(model_id)) LIKE '%/xhigh' THEN 'xhigh'
    WHEN lower(trim(model_id)) LIKE '%/extra-high' THEN 'xhigh'
    WHEN lower(trim(model_id)) LIKE '%/extra_high' THEN 'xhigh'
    WHEN lower(trim(model_id)) LIKE '%/extrahigh' THEN 'xhigh'
    ELSE reasoning_effort
  END,
  model_id = CASE
    WHEN lower(trim(model_id)) LIKE '%/none' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/none'))
    WHEN lower(trim(model_id)) LIKE '%/minimal' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/minimal'))
    WHEN lower(trim(model_id)) LIKE '%/low' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/low'))
    WHEN lower(trim(model_id)) LIKE '%/medium' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/medium'))
    WHEN lower(trim(model_id)) LIKE '%/high' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/high'))
    WHEN lower(trim(model_id)) LIKE '%/xhigh' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/xhigh'))
    WHEN lower(trim(model_id)) LIKE '%/extra-high' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/extra-high'))
    WHEN lower(trim(model_id)) LIKE '%/extra_high' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/extra_high'))
    WHEN lower(trim(model_id)) LIKE '%/extrahigh' THEN substr(trim(model_id), 1, length(trim(model_id)) - length('/extrahigh'))
    ELSE trim(model_id)
  END
WHERE reasoning_effort IS NULL
  AND (
    lower(trim(model_id)) LIKE '%/none'
    OR lower(trim(model_id)) LIKE '%/minimal'
    OR lower(trim(model_id)) LIKE '%/low'
    OR lower(trim(model_id)) LIKE '%/medium'
    OR lower(trim(model_id)) LIKE '%/high'
    OR lower(trim(model_id)) LIKE '%/xhigh'
    OR lower(trim(model_id)) LIKE '%/extra-high'
    OR lower(trim(model_id)) LIKE '%/extra_high'
    OR lower(trim(model_id)) LIKE '%/extrahigh'
  );
