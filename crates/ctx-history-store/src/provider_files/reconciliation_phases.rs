fn reconciliation_phase_spec(phase: i64) -> Option<ReconciliationPhaseSpec> {
    match phase {
        CLEANUP_PHASE_LINKS => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM history_record_links INDEXED BY idx_reconcile_history_record_links_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_FILES => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                WITH candidate(id) AS MATERIALIZED (
                    SELECT id FROM (
                        SELECT id FROM files_touched INDEXED BY idx_reconcile_files_touched_source_id
                        WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                        ORDER BY id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT file.id AS id
                        FROM events AS event INDEXED BY idx_reconcile_events_capture_source_id
                        JOIN files_touched AS file INDEXED BY idx_reconcile_files_touched_event_id ON file.event_id = event.id
                        WHERE event.capture_source_id = ?1 AND (?2 IS NULL OR file.id > ?2)
                        ORDER BY file.id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT file.id AS id
                        FROM sessions AS session INDEXED BY idx_reconcile_sessions_capture_source_id
                        JOIN events AS event INDEXED BY idx_reconcile_events_session_id ON event.session_id = session.id
                        JOIN files_touched AS file INDEXED BY idx_reconcile_files_touched_event_id ON file.event_id = event.id
                        WHERE session.capture_source_id = ?1 AND (?2 IS NULL OR file.id > ?2)
                        ORDER BY file.id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT file.id AS id
                        FROM runs AS run INDEXED BY idx_reconcile_runs_source_id
                        JOIN events AS event INDEXED BY idx_reconcile_events_run_id ON event.run_id = run.id
                        JOIN files_touched AS file INDEXED BY idx_reconcile_files_touched_event_id ON file.event_id = event.id
                        WHERE run.source_id = ?1 AND (?2 IS NULL OR file.id > ?2)
                        ORDER BY file.id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT file.id AS id
                        FROM runs AS run INDEXED BY idx_reconcile_runs_source_id
                        JOIN files_touched AS file INDEXED BY idx_reconcile_files_touched_run_id ON file.run_id = run.id
                        WHERE run.source_id = ?1 AND (?2 IS NULL OR file.id > ?2)
                        ORDER BY file.id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT file.id AS id
                        FROM sessions AS session INDEXED BY idx_reconcile_sessions_capture_source_id
                        JOIN runs AS run INDEXED BY idx_reconcile_runs_session_id ON run.session_id = session.id
                        JOIN files_touched AS file INDEXED BY idx_reconcile_files_touched_run_id ON file.run_id = run.id
                        WHERE session.capture_source_id = ?1 AND (?2 IS NULL OR file.id > ?2)
                        ORDER BY file.id LIMIT ?3
                    )
                )
                SELECT entity.id, COALESCE(
                    entity.source_id,
                    (SELECT event.capture_source_id FROM events AS event WHERE event.id = entity.event_id),
                    (SELECT session.capture_source_id FROM events AS event JOIN sessions AS session ON session.id = event.session_id WHERE event.id = entity.event_id),
                    (SELECT run.source_id FROM events AS event JOIN runs AS run ON run.id = event.run_id WHERE event.id = entity.event_id),
                    (SELECT run.source_id FROM runs AS run WHERE run.id = entity.run_id),
                    (SELECT session.capture_source_id FROM runs AS run JOIN sessions AS session ON session.id = run.session_id WHERE run.id = entity.run_id)
                ) = ?1,
                entity.id
                FROM candidate
                JOIN files_touched AS entity ON entity.id = candidate.id
                ORDER BY entity.id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_EDGES => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                WITH candidate(id) AS MATERIALIZED (
                    SELECT id FROM (
                        SELECT id FROM session_edges INDEXED BY idx_reconcile_session_edges_source_id
                        WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                        ORDER BY id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT edge.id AS id
                        FROM sessions AS session INDEXED BY idx_reconcile_sessions_capture_source_id
                        JOIN session_edges AS edge INDEXED BY idx_reconcile_session_edges_from_session_id ON edge.from_session_id = session.id
                        WHERE session.capture_source_id = ?1 AND (?2 IS NULL OR edge.id > ?2)
                        ORDER BY edge.id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT edge.id AS id
                        FROM sessions AS session INDEXED BY idx_reconcile_sessions_capture_source_id
                        JOIN session_edges AS edge INDEXED BY idx_reconcile_session_edges_to_session_id ON edge.to_session_id = session.id
                        WHERE session.capture_source_id = ?1 AND (?2 IS NULL OR edge.id > ?2)
                        ORDER BY edge.id LIMIT ?3
                    )
                )
                SELECT entity.id, COALESCE(
                    entity.source_id,
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = entity.from_session_id),
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = entity.to_session_id)
                ) = ?1,
                entity.id
                FROM candidate JOIN session_edges AS entity ON entity.id = candidate.id
                ORDER BY entity.id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_SUMMARIES => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM summaries INDEXED BY idx_reconcile_summaries_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_EVENTS => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                WITH candidate(id) AS MATERIALIZED (
                    SELECT id FROM (
                        SELECT id FROM events INDEXED BY idx_reconcile_events_capture_source_id
                        WHERE capture_source_id = ?1 AND (?2 IS NULL OR id > ?2)
                        ORDER BY id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT event.id AS id
                        FROM sessions AS session INDEXED BY idx_reconcile_sessions_capture_source_id
                        JOIN events AS event INDEXED BY idx_reconcile_events_session_id ON event.session_id = session.id
                        WHERE session.capture_source_id = ?1 AND (?2 IS NULL OR event.id > ?2)
                        ORDER BY event.id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT event.id AS id
                        FROM runs AS run INDEXED BY idx_reconcile_runs_source_id
                        JOIN events AS event INDEXED BY idx_reconcile_events_run_id ON event.run_id = run.id
                        WHERE run.source_id = ?1 AND (?2 IS NULL OR event.id > ?2)
                        ORDER BY event.id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT event.id AS id
                        FROM sessions AS session INDEXED BY idx_reconcile_sessions_capture_source_id
                        JOIN runs AS run INDEXED BY idx_reconcile_runs_session_id ON run.session_id = session.id
                        JOIN events AS event INDEXED BY idx_reconcile_events_run_id ON event.run_id = run.id
                        WHERE session.capture_source_id = ?1 AND (?2 IS NULL OR event.id > ?2)
                        ORDER BY event.id LIMIT ?3
                    )
                )
                SELECT entity.id, COALESCE(
                    entity.capture_source_id,
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = entity.session_id),
                    (SELECT run_session.capture_source_id FROM runs AS run JOIN sessions AS run_session ON run_session.id = run.session_id WHERE run.id = entity.run_id),
                    (SELECT run.source_id FROM runs AS run WHERE run.id = entity.run_id)
                ) = ?1,
                entity.id
                FROM candidate JOIN events AS entity ON entity.id = candidate.id
                ORDER BY entity.id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_RUNS => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                WITH candidate(id) AS MATERIALIZED (
                    SELECT id FROM (
                        SELECT id FROM runs INDEXED BY idx_reconcile_runs_source_id
                        WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                        ORDER BY id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT run.id AS id
                        FROM sessions AS session INDEXED BY idx_reconcile_sessions_capture_source_id
                        JOIN runs AS run INDEXED BY idx_reconcile_runs_session_id ON run.session_id = session.id
                        WHERE session.capture_source_id = ?1 AND (?2 IS NULL OR run.id > ?2)
                        ORDER BY run.id LIMIT ?3
                    )
                )
                SELECT entity.id, COALESCE(
                    entity.source_id,
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = entity.session_id)
                ) = ?1,
                entity.id
                FROM candidate JOIN runs AS entity ON entity.id = candidate.id
                ORDER BY entity.id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_SESSIONS => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM sessions INDEXED BY idx_reconcile_sessions_capture_source_id
                WHERE capture_source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_VCS_CHANGES => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                WITH candidate(id) AS MATERIALIZED (
                    SELECT id FROM (
                        SELECT id FROM vcs_changes INDEXED BY idx_reconcile_vcs_changes_source_id
                        WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                        ORDER BY id LIMIT ?3
                    )
                    UNION
                    SELECT id FROM (
                        SELECT change.id AS id
                        FROM vcs_workspaces AS workspace INDEXED BY idx_reconcile_vcs_workspaces_source_id
                        JOIN vcs_changes AS change ON change.vcs_workspace_id = workspace.id
                        WHERE change.source_id IS NULL AND workspace.source_id = ?1
                          AND (?2 IS NULL OR change.id > ?2)
                        ORDER BY change.id LIMIT ?3
                    )
                )
                SELECT entity.id, COALESCE(entity.source_id, workspace.source_id) = ?1,
                       entity.id
                FROM candidate
                JOIN vcs_changes AS entity ON entity.id = candidate.id
                LEFT JOIN vcs_workspaces AS workspace ON workspace.id = entity.vcs_workspace_id
                ORDER BY entity.id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_ARTIFACTS => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM artifacts INDEXED BY idx_reconcile_artifacts_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_HISTORY_RECORD_TAGS => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT CAST(rowid AS TEXT), 1, CAST(rowid AS TEXT)
                FROM history_record_tags INDEXED BY idx_history_record_tags_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR rowid > CAST(?2 AS INTEGER))
                ORDER BY rowid LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_RECORD_EDGES => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM record_edges INDEXED BY idx_reconcile_record_edges_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_HISTORY_RECORDS => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM history_records INDEXED BY idx_reconcile_history_records_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_VCS_WORKSPACES => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM vcs_workspaces INDEXED BY idx_reconcile_vcs_workspaces_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        CLEANUP_PHASE_AUDIT_LOG => Some(ReconciliationPhaseSpec {
            owner_select_sql: r#"
                SELECT id, 1, id FROM audit_log INDEXED BY idx_reconcile_audit_log_source_id
                WHERE source_id = ?1 AND (?2 IS NULL OR id > ?2)
                ORDER BY id LIMIT ?3
            "#,
        }),
        _ => None,
    }
}

struct LegacyReconciliationPhaseSpec {
    direct_owner_select_sql: &'static str,
    indirect_owner_scan_sql: Option<&'static str>,
}

fn legacy_reconciliation_phase_spec(phase: i64) -> Option<LegacyReconciliationPhaseSpec> {
    let spec = match phase {
        CLEANUP_PHASE_LINKS => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM history_record_links AS entity
                     INDEXED BY idx_history_record_links_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_FILES => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM files_touched AS entity INDEXED BY idx_files_touched_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: Some(
                r#"
                SELECT entity.id,
                       entity.source_id IS NULL AND COALESCE(
                         (SELECT event.capture_source_id
                          FROM events AS event WHERE event.id = entity.event_id),
                         (SELECT session.capture_source_id
                          FROM events AS event
                          JOIN sessions AS session ON session.id = event.session_id
                          WHERE event.id = entity.event_id),
                         (SELECT run.source_id
                          FROM events AS event
                          JOIN runs AS run ON run.id = event.run_id
                          WHERE event.id = entity.event_id),
                         (SELECT run.source_id
                          FROM runs AS run WHERE run.id = entity.run_id),
                         (SELECT session.capture_source_id
                          FROM runs AS run
                          JOIN sessions AS session ON session.id = run.session_id
                          WHERE run.id = entity.run_id)
                       ) = ?1,
                       CAST(entity.rowid AS TEXT)
                FROM files_touched AS entity
                WHERE entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            ),
        },
        CLEANUP_PHASE_EDGES => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM session_edges AS entity INDEXED BY idx_session_edges_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: Some(
                r#"
                SELECT entity.id,
                       entity.source_id IS NULL AND COALESCE(
                         (SELECT session.capture_source_id
                          FROM sessions AS session
                          WHERE session.id = entity.from_session_id),
                         (SELECT session.capture_source_id
                          FROM sessions AS session
                          WHERE session.id = entity.to_session_id)
                       ) = ?1,
                       CAST(entity.rowid AS TEXT)
                FROM session_edges AS entity
                WHERE entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            ),
        },
        CLEANUP_PHASE_SUMMARIES => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM summaries AS entity INDEXED BY idx_summaries_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_EVENTS => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM events AS entity INDEXED BY idx_events_capture_source_id
                WHERE entity.capture_source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: Some(
                r#"
                SELECT entity.id,
                       entity.capture_source_id IS NULL AND COALESCE(
                         (SELECT session.capture_source_id
                          FROM sessions AS session WHERE session.id = entity.session_id),
                         (SELECT run_session.capture_source_id
                          FROM runs AS run
                          JOIN sessions AS run_session ON run_session.id = run.session_id
                          WHERE run.id = entity.run_id),
                         (SELECT run.source_id
                          FROM runs AS run WHERE run.id = entity.run_id)
                       ) = ?1,
                       CAST(entity.rowid AS TEXT)
                FROM events AS entity
                WHERE entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            ),
        },
        CLEANUP_PHASE_RUNS => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM runs AS entity INDEXED BY idx_runs_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: Some(
                r#"
                SELECT entity.id,
                       entity.source_id IS NULL AND (
                         SELECT session.capture_source_id
                         FROM sessions AS session
                         WHERE session.id = entity.session_id
                       ) = ?1,
                       CAST(entity.rowid AS TEXT)
                FROM runs AS entity
                WHERE entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            ),
        },
        CLEANUP_PHASE_SESSIONS => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM sessions AS entity INDEXED BY idx_sessions_capture_source_id
                WHERE entity.capture_source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_VCS_CHANGES => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM vcs_changes AS entity INDEXED BY idx_vcs_changes_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: Some(
                r#"
                SELECT entity.id,
                       entity.source_id IS NULL AND (
                         SELECT workspace.source_id
                         FROM vcs_workspaces AS workspace
                         WHERE workspace.id = entity.vcs_workspace_id
                       ) = ?1,
                       CAST(entity.rowid AS TEXT)
                FROM vcs_changes AS entity
                WHERE entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            ),
        },
        CLEANUP_PHASE_ARTIFACTS => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM artifacts AS entity INDEXED BY idx_artifacts_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_HISTORY_RECORD_TAGS => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT CAST(entity.rowid AS TEXT), 1, CAST(entity.rowid AS TEXT)
                FROM history_record_tags AS entity
                     INDEXED BY idx_history_record_tags_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_RECORD_EDGES => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM record_edges AS entity INDEXED BY idx_record_edges_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_HISTORY_RECORDS => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM history_records AS entity INDEXED BY idx_history_records_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_VCS_WORKSPACES => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM vcs_workspaces AS entity INDEXED BY idx_vcs_workspaces_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        CLEANUP_PHASE_AUDIT_LOG => LegacyReconciliationPhaseSpec {
            direct_owner_select_sql: r#"
                SELECT entity.id, 1, CAST(entity.rowid AS TEXT)
                FROM audit_log AS entity INDEXED BY idx_audit_log_source_id
                WHERE entity.source_id = ?1
                  AND entity.rowid > COALESCE(?2, -9223372036854775808)
                ORDER BY entity.rowid LIMIT ?3
            "#,
            indirect_owner_scan_sql: None,
        },
        _ => return None,
    };
    Some(spec)
}
