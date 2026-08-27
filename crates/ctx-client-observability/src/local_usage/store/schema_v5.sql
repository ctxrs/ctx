CREATE TABLE daily_usage (
    day_utc TEXT NOT NULL
        CHECK (
            length(day_utc) = 10
            AND day_utc GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(day_utc) IS NOT NULL
            AND date(day_utc) = day_utc
        ),
    definition_version INTEGER NOT NULL CHECK (definition_version IN (1, 2, 3)),
    ctx_version TEXT NOT NULL
        CHECK (
            length(ctx_version) BETWEEN 1 AND 64
            AND ctx_version NOT GLOB '*[^0-9A-Za-z.+-]*'
        ),
    surface TEXT NOT NULL CHECK (surface IN ('cli', 'mcp')),
    operation TEXT NOT NULL CHECK (
        (
            definition_version = 1
            AND surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show', 'locate',
                'search', 'docs', 'integrations', 'daemon_status',
                'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            definition_version = 2
            AND surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show_session',
                'show_event', 'locate', 'search', 'docs',
                'integrations', 'daemon_status', 'daemon_enable',
                'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            definition_version = 3
            AND surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show_session',
                'show_event', 'locate', 'search', 'blame', 'docs',
                'integrations', 'daemon_status', 'daemon_enable',
                'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            definition_version IN (1, 2)
            AND surface = 'mcp'
            AND operation IN (
                'status', 'sources', 'search', 'show_session', 'show_event'
            )
        )
        OR
        (
            definition_version = 3
            AND surface = 'mcp'
            AND operation IN (
                'status', 'sources', 'search', 'show_session', 'show_event', 'blame'
            )
        )
    ),
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'failure')),
    value_class TEXT NOT NULL
        CHECK (value_class IN ('result_bearing', 'empty', 'not_applicable')),
    duration_bucket TEXT NOT NULL
        CHECK (duration_bucket IN (
            'under_10_ms', '10_to_49_ms', '50_to_249_ms', '250_to_999_ms',
            '1_to_4_s', '5_to_29_s', '30_s_or_more'
        )),
    context_coverage TEXT NOT NULL
        CHECK (context_coverage IN ('complete', 'unavailable', 'not_applicable')),
    calls INTEGER NOT NULL CHECK (calls > 0),
    result_count INTEGER NOT NULL CHECK (result_count >= 0),
    delivered_output_bytes INTEGER NOT NULL
        CHECK (
            delivered_output_bytes >= 0
            AND (
                (
                    definition_version = 1
                    AND (
                        (surface = 'cli' AND delivered_output_bytes = 0)
                        OR (surface = 'mcp' AND delivered_output_bytes > 0)
                    )
                )
                OR (
                    definition_version = 2
                    AND (
                        delivered_output_bytes > 0
                        OR (surface = 'cli' AND outcome = 'failure')
                    )
                )
                OR (
                    definition_version = 3
                    AND (
                        (operation = 'blame'
                            AND (
                                (surface = 'cli' AND delivered_output_bytes = 0)
                                OR (surface = 'mcp' AND delivered_output_bytes > 0)
                            ))
                        OR (
                            operation != 'blame'
                            AND (
                                delivered_output_bytes > 0
                                OR (surface = 'cli' AND outcome = 'failure')
                            )
                        )
                    )
                )
            )
        ),
    delivered_context_bytes INTEGER NOT NULL CHECK (delivered_context_bytes >= 0),
    matched_normalized_session_bytes INTEGER NOT NULL
        CHECK (matched_normalized_session_bytes >= 0),
    CHECK (
        (
            outcome = 'failure'
            AND value_class = 'not_applicable'
            AND result_count = 0
        )
        OR outcome = 'success'
    ),
    CHECK (
        (value_class = 'result_bearing' AND result_count >= calls)
        OR (value_class IN ('empty', 'not_applicable') AND result_count = 0)
    ),
    CHECK (
        outcome = 'failure'
        OR (
            definition_version IN (1, 2, 3)
            AND surface = 'cli'
            AND (
                (definition_version IN (2, 3) AND operation = 'search'
                    AND value_class IN ('result_bearing', 'empty'))
                OR ((definition_version = 1 OR operation != 'search')
                    AND value_class = 'not_applicable')
            )
        )
        OR (
            definition_version IN (1, 2, 3)
            AND surface = 'mcp'
            AND (
                (operation IN ('sources', 'search', 'show_session', 'show_event')
                    AND value_class IN ('result_bearing', 'empty'))
                OR (operation IN ('status', 'blame')
                    AND value_class = 'not_applicable')
            )
        )
    ),
    CHECK (
        (
            definition_version IN (2, 3)
            AND operation = 'search'
            AND outcome = 'success'
            AND value_class = 'result_bearing'
            AND (
                (context_coverage = 'complete'
                    AND delivered_context_bytes > 0
                    AND matched_normalized_session_bytes >= delivered_context_bytes)
                OR (context_coverage = 'unavailable'
                    AND delivered_context_bytes = 0
                    AND matched_normalized_session_bytes = 0)
            )
        )
        OR (context_coverage = 'not_applicable'
            AND delivered_context_bytes = 0
            AND matched_normalized_session_bytes = 0)
    ),
    PRIMARY KEY (
        day_utc, definition_version, ctx_version, surface, operation, outcome,
        value_class, duration_bucket, context_coverage
    )
) WITHOUT ROWID, STRICT;
