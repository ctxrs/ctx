ALTER TABLE sessions
ADD COLUMN execution_environment TEXT NOT NULL DEFAULT 'host';

UPDATE sessions
SET execution_environment = COALESCE(
    (
        SELECT CASE json_extract(settings_json, '$.execution.environment')
            WHEN 'sandbox' THEN 'sandbox'
            WHEN 'container_host_mounted' THEN 'sandbox'
            WHEN 'container_disk_isolated' THEN 'sandbox'
            WHEN 'host' THEN 'host'
            ELSE NULL
        END
        FROM runtime_settings
        LIMIT 1
    ),
    execution_environment
);
