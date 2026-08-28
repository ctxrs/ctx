use std::{io::Write as _, path::Path};

use ctx_history_capture::ProviderImportWorkResult;
use ctx_history_ingest_application::ImportIndexDelta;
use unicode_width::UnicodeWidthStr as _;

use crate::ui::{ColorMode, StreamKind, TestContext};

use super::*;

fn context(width: usize, color: ColorMode) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
}

fn report(resume: bool, totals: ImportTotals, sources: Vec<IngestSourceOutcome>) -> IngestReport {
    IngestReport {
        resume,
        totals,
        sources,
        telemetry: None,
        provider_refresh: None,
        core_publication: None,
    }
}

fn changed_report() -> IngestReport {
    report(
        true,
        ImportTotals {
            per_run_counts_available: true,
            source_files: 1,
            source_bytes: 4096,
            imported_sources: 1,
            imported_sessions: 2,
            imported_events: 7,
            imported_edges: 1,
            skipped: 1,
            current_source_count: Some(1),
            current_indexed_sessions: Some(2),
            current_indexed_documents: Some(7),
            index_delta: Some(ImportIndexDelta {
                sessions: 2,
                searchable_events: 7,
            }),
            current_complete_records: Some(7),
            current_retained_records: Some(7),
            current_rejected_records: Some(0),
            current_ignored_records: Some(1),
            current_certified_source_bytes: Some(4096),
            current_sources_with_rejections: Some(0),
            removed_source_count: Some(0),
            work_result: ProviderImportWorkResult::Changed,
            ..ImportTotals::default()
        },
        Vec::new(),
    )
}

fn failed_exact_report() -> IngestReport {
    let selector = "/history/codex/sessions.jsonl";
    report(
        false,
        ImportTotals {
            terminal_route_counts_available: true,
            failed_sources: 3,
            work_result: ProviderImportWorkResult::NoOp,
            ..ImportTotals::default()
        },
        vec![IngestSourceOutcome::Exact(
            ctx_history_ingest_application::ExactPublicationOutcome {
                status: ctx_history_ingest_application::IngestStatus::Failure,
                failure_scope: ctx_history_ingest_application::IngestFailureScope::Source,
                failure_type:
                    ctx_history_ingest_application::IngestFailureType::UnsupportedSchema,
                provider: ctx_history_core::CaptureProvider::Codex,
                path: Path::new(selector).to_path_buf(),
                source_format: "codex_sessions_jsonl_v1",
                stats: ctx_history_ingest_application::SourceStats {
                    files: 1,
                    bytes: 8,
                    change_token: Some([7; 32]),
                },
                route_identity: "route-1".to_owned(),
                catalog_lineage: "lineage-1".to_owned(),
                request_overlay: ctx_history_refresh::ExplicitSourceCatalogAuthority::from_json(
                    &serde_json::json!({
                        "schema_version": 1,
                        "revision": 1,
                        "integrity": {
                            "algorithm": "sha256",
                            "digest": "078d5bef2714da7d54739411aa15cfe03b234795578dd74c941f05054d8dff6f",
                        },
                        "entries": [],
                    }),
                )
                .unwrap(),
                previous_generation: Some("generation-0".to_owned()),
                published_generation: "generation-1".to_owned(),
                generation_changed: false,
                scanned_routes: 1,
                successful_routes: 0,
                source_failure_total: 3,
                route_source_failure_total: 3,
                rejected_record_total: 0,
                rejection_diagnostics: Vec::new(),
                request_id: Some("request-1".to_owned()),
                change: ctx_history_ingest_application::IngestChange::NoOp,
                current: ctx_history_refresh::SourceBackedRefreshCurrent::default(),
                requested_failure: Some(ctx_history_ingest_application::SourceFailureOutcome {
                    status: ctx_history_ingest_application::IngestStatus::Failure,
                    failure_scope: ctx_history_ingest_application::IngestFailureScope::Source,
                    failure_type:
                        ctx_history_ingest_application::IngestFailureType::UnsupportedSchema,
                    source_identity: "source-1".to_owned(),
                    provider: "codex".to_owned(),
                    source_failure_class: "incompatible".to_owned(),
                    carried_forward: true,
                    source_selector: selector.to_owned(),
                    detail: "unsupported source schema".to_owned(),
                }),
                requested_failure_class: Some("incompatible".to_owned()),
            },
        )],
    )
}

#[test]
fn human_import_report_is_outcome_first_and_omits_internal_fields() {
    let report = changed_report();
    for width in [32, 48, 80, 120] {
        let context = context(width, ColorMode::Never);
        let document = render_import_report_human(&context, &report);
        let rendered = document.render_plain();
        assert!(rendered.starts_with("✓ History import completed\nLocal history changed.\n"));
        assert!(rendered.contains("\nImported\n"));
        assert!(rendered.contains("\nCurrent index\n"));
        for internal in [
            "outcome:",
            "failure_scope",
            "failure_type",
            "published_generation",
            "previous_generation",
            "generation_changed",
            "resume_mode",
            "current_source_count",
            "source_files",
        ] {
            assert!(
                !rendered.contains(internal),
                "human output exposed {internal:?}: {rendered}"
            );
        }
        let available = context.content_width().unwrap();
        for line in rendered.lines() {
            assert!(
                line.width() <= available,
                "{line:?} exceeded {available} columns"
            );
        }
    }
}

#[test]
fn human_import_report_has_stable_copy_and_source_failure_recovery() {
    let success = render_import_report_human(&context(80, ColorMode::Never), &changed_report())
        .render_plain();
    assert_eq!(
        success,
        "✓ History import completed\n\
         Local history changed.\n\
         \n\
         Net index change\n\
         Sessions           +2\n\
         Searchable events  +7\n\
         \n\
         Imported\n\
         Sources          1\n\
         Sessions         2\n\
         Events           7\n\
         Edges            1\n\
         Skipped records  1\n\
         \n\
         Current index\n\
         Sources            1\n\
         Sessions           2\n\
         Searchable events  7\n\
         Removed sources    0\n"
    );

    let report = report(
        false,
        ImportTotals {
            per_run_counts_available: true,
            imported_sources: 1,
            failed_sources: 1,
            failed: 2,
            current_retained_records: Some(1),
            work_result: ProviderImportWorkResult::Changed,
            ..ImportTotals::default()
        },
        Vec::new(),
    );
    let warning =
        render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
    assert!(warning.starts_with(
        "! History import completed with source failures\n\
         1 source failed; imported history remains available.\n"
    ));
    assert!(warning.contains("Skipped records  2\n"), "{warning:?}");
    assert!(!warning.contains("rejected"), "{warning:?}");
    assert!(
        warning.ends_with(concat!(
            "Hint: Inspect source availability and import support.\n",
            "\n",
            "Next\n",
            "  ctx sources\n",
        )),
        "{warning:?}"
    );
}

#[test]
fn human_import_report_always_renders_zero_skipped_records() {
    let mut report = changed_report();
    report.totals.skipped = 0;

    let rendered =
        render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();

    assert!(rendered.contains("Skipped records  0\n"), "{rendered:?}");
}

#[test]
fn rejection_only_import_is_success_without_warning_prose_or_retry() {
    let report = report(
        false,
        ImportTotals {
            terminal_route_counts_available: true,
            failed: 2,
            sources_completed_with_rejections: 1,
            current_source_count: Some(1),
            current_complete_records: Some(3),
            current_retained_records: Some(1),
            current_rejected_records: Some(2),
            current_sources_with_rejections: Some(1),
            work_result: ProviderImportWorkResult::Changed,
            ..ImportTotals::default()
        },
        Vec::new(),
    );

    let rendered =
        render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
    assert!(
        rendered.starts_with("✓ History import completed\n"),
        "{rendered}"
    );
    assert!(rendered.contains("Skipped records  2\n"), "{rendered}");
    for forbidden in ["warning", "rejected", "Hint:", "\nNext\n", "retry"] {
        assert!(
            !rendered.to_lowercase().contains(&forbidden.to_lowercase()),
            "{rendered}"
        );
    }
    assert!(import_completion_error(&report).is_none());

    let json = import_report_json(&report);
    assert_eq!(json["outcome"], "completed_with_rejections");
    assert_eq!(json["failure_scope"], "record");
    assert_eq!(json["failure_type"], "record_rejection");
    assert_eq!(json["totals"]["rejected_records"], 2);
    assert_eq!(json["totals"]["sources_completed_with_rejections"], 1);
}

#[test]
fn all_attempted_records_unusable_is_a_concise_failure() {
    let report = report(
        false,
        ImportTotals {
            terminal_route_counts_available: true,
            failed: 3,
            sources_completed_with_rejections: 1,
            current_source_count: Some(1),
            current_complete_records: Some(3),
            current_retained_records: Some(0),
            current_rejected_records: Some(3),
            current_sources_with_rejections: Some(1),
            work_result: ProviderImportWorkResult::Changed,
            ..ImportTotals::default()
        },
        Vec::new(),
    );

    let rendered =
        render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
    assert!(rendered.starts_with("✗ History import failed\nNo usable history was imported.\n"));
    assert!(rendered.contains("Skipped records  3\n"), "{rendered}");
    assert_eq!(import_report_json(&report)["outcome"], "failure");
    assert_eq!(
        import_completion_error(&report).unwrap().to_string(),
        "No usable history was imported"
    );
}

#[test]
fn persisted_rejections_remain_current_index_diagnostics_on_a_noop() {
    let report = report(
        true,
        ImportTotals {
            current_source_count: Some(1),
            current_indexed_documents: Some(7),
            current_rejected_records: Some(2),
            current_sources_with_rejections: Some(1),
            work_result: ProviderImportWorkResult::NoOp,
            ..ImportTotals::default()
        },
        Vec::new(),
    );

    let rendered =
        render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
    assert!(
        rendered.starts_with("✓ History import completed\nNo source changes were found.\n"),
        "{rendered}"
    );
    assert!(rendered.contains("Searchable events  7"), "{rendered}");
    assert!(!rendered.contains("records were rejected"), "{rendered}");
    assert!(rendered.contains("Skipped records  0"), "{rendered}");
    assert!(!rendered.contains("Rejected records"), "{rendered}");
    assert!(!rendered.contains("ctx doctor"), "{rendered}");
}

#[test]
fn retained_usable_generation_reports_bounded_schema_v2_source_failures() {
    let report = report(
        false,
        ImportTotals {
            failed_sources: 5,
            current_source_count: Some(2),
            current_indexed_documents: Some(7),
            current_retained_records: Some(7),
            work_result: ProviderImportWorkResult::NoOp,
            ..ImportTotals::default()
        },
        (0..4)
            .map(|index| {
                IngestSourceOutcome::SourceFailure(
                    ctx_history_ingest_application::SourceFailureOutcome {
                        status: ctx_history_ingest_application::IngestStatus::Failure,
                        failure_scope: ctx_history_ingest_application::IngestFailureScope::Source,
                        failure_type: ctx_history_ingest_application::IngestFailureType::Other,
                        source_identity: format!("source-{index}"),
                        provider: "codex".to_owned(),
                        source_failure_class: "source_changed".to_owned(),
                        carried_forward: true,
                        source_selector: format!("/history/{index}.jsonl"),
                        detail: "source changed during refresh".to_owned(),
                    },
                )
            })
            .collect(),
    );

    let json = import_report_json(&report);
    assert_eq!(json["outcome"], "completed_with_source_failures");
    assert_eq!(json["failure_scope"], "source");
    assert_eq!(json["totals"]["failed_sources"], 5);
    for unsupported in [
        "source_files",
        "source_bytes",
        "imported_sources",
        "imported_sessions",
        "imported_events",
        "imported_edges",
    ] {
        assert!(json["totals"].get(unsupported).is_none(), "{json:#}");
    }
    let rendered =
        render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
    assert!(rendered.starts_with("! History import completed with source failures\n"));
    assert!(rendered.contains("5 sources failed; imported history remains available."));
    assert!(rendered.contains("Source failures\n"));
    assert!(rendered.contains("/history/0.jsonl"));
    assert!(rendered.contains("/history/2.jsonl"));
    assert!(!rendered.contains("/history/3.jsonl"));
    assert!(rendered.contains("2 source failures were omitted"));
}

#[test]
fn failed_exact_route_projects_bounded_human_detail_before_omissions() {
    let report = failed_exact_report();

    assert_eq!(
        source_failure_fields(&report),
        vec![
            (
                "Source 1".to_owned(),
                "/history/codex/sessions.jsonl is not importable (codex, incompatible, retained prior data): unsupported source schema".to_owned(),
            ),
            (
                "Additional".to_owned(),
                "2 source failures were omitted".to_owned(),
            ),
        ]
    );
    let rendered =
        render_import_report_human(&context(120, ColorMode::Never), &report).render_plain();
    assert_eq!(
        rendered,
        concat!(
            "✗ History import failed\n",
            "No usable history was imported.\n",
            "\n",
            "Imported\n",
            "Skipped records  0\n",
            "\n",
            "Source failures\n",
            "Source 1    /history/codex/sessions.jsonl is not importable (codex, incompatible, retained prior data): unsupported\n",
            "            source schema\n",
            "Additional  2 source failures were omitted\n",
            "\n",
            "Hint: Inspect source availability and import support.\n",
            "\n",
            "Next\n",
            "  ctx sources\n",
        )
    );
}

#[test]
fn import_json_contract_is_unchanged_by_human_renderer() {
    let value = import_report_json(&changed_report());
    assert_eq!(
        value,
        json!({
            "schema_version": 2,
            "outcome": "success",
            "failure_scope": "none",
            "failure_type": "none",
            "resume": true,
            "resume_mode": "idempotent_rescan",
            "totals": {
                "source_files": 1,
                "source_bytes": 4096,
                "imported_sources": 1,
                "sources_completed_with_rejections": 0,
                "imported_sessions": 2,
                "imported_events": 7,
                "imported_edges": 1,
                "skipped_sessions": 0,
                "skipped_events": 0,
                "skipped_edges": 0,
                "skipped": 1,
                "rejected_records": 0,
                "current_source_count": 1,
                "current_indexed_sessions": 2,
                "current_indexed_documents": 7,
                "index_delta": {
                    "sessions": 2,
                    "searchable_events": 7
                },
                "current_complete_records": 7,
                "current_retained_records": 7,
                "current_rejected_records": 0,
                "current_ignored_records": 1,
                "current_certified_source_bytes": 4096,
                "current_sources_with_rejections": 0,
                "removed_source_count": 0,
                "change": "changed"
            },
            "sources": [],
        })
    );
}

#[test]
fn import_index_deltas_render_as_signed_counts_and_are_absent_on_first_import() {
    for (sessions, searchable_events, expected_sessions, expected_events) in
        [(2, 7, "+2", "+7"), (0, 0, "0", "0"), (-1, -3, "-1", "-3")]
    {
        let mut report = changed_report();
        report.totals.index_delta = Some(ImportIndexDelta {
            sessions,
            searchable_events,
        });
        let rendered =
            render_import_report_human(&context(80, ColorMode::Never), &report).render_plain();
        assert!(
            rendered.contains(&format!("Sessions           {expected_sessions}")),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("Searchable events  {expected_events}")),
            "{rendered}"
        );
        let json = import_report_json(&report);
        assert_eq!(json["totals"]["index_delta"]["sessions"], sessions);
        assert_eq!(
            json["totals"]["index_delta"]["searchable_events"],
            searchable_events
        );
    }

    let mut first = changed_report();
    first.totals.index_delta = None;
    let rendered =
        render_import_report_human(&context(80, ColorMode::Never), &first).render_plain();
    assert!(!rendered.contains("Net index change"), "{rendered}");
    assert!(import_report_json(&first)["totals"]
        .get("index_delta")
        .is_none());
}

#[test]
fn import_plain_output_equals_ansi_stripped_styled_output() {
    let report = changed_report();
    let context = context(80, ColorMode::Always);
    let document = render_import_report_human(&context, &report);
    let mut stream = anstream::StripStream::new(Vec::new());
    stream
        .write_all(document.render(&context).as_bytes())
        .unwrap();
    assert_eq!(
        String::from_utf8(stream.into_inner()).unwrap(),
        document.render_plain()
    );
}

#[test]
fn provider_database_lock_is_source_scoped() {
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("database is locked".to_owned()),
    );
    let error = anyhow::Error::new(CaptureError::Sqlite(sqlite));
    assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
    assert_eq!(
        import_failure_type(&error),
        ImportFailureType::SourceDatabase
    );
}

#[test]
fn typed_native_source_failures_keep_stable_classification() {
    let cases = [
        (
            ProviderSourceFailureKind::NotFound,
            ImportFailureType::NotFound,
        ),
        (
            ProviderSourceFailureKind::Permission,
            ImportFailureType::Permission,
        ),
        (
            ProviderSourceFailureKind::Locked,
            ImportFailureType::SourceDatabase,
        ),
        (
            ProviderSourceFailureKind::SchemaIncompatible,
            ImportFailureType::UnsupportedSchema,
        ),
        (
            ProviderSourceFailureKind::InvalidSource,
            ImportFailureType::MalformedSource,
        ),
        (
            ProviderSourceFailureKind::SourceChanged,
            ImportFailureType::Other,
        ),
    ];
    for (kind, expected) in cases {
        let error = anyhow::Error::new(CaptureError::ProviderSource {
            provider: "test",
            path: Path::new("provider.sqlite").to_path_buf(),
            kind,
            detail: "typed failure".to_owned(),
        });
        assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
        assert_eq!(import_failure_type(&error), expected);
    }
}

#[test]
fn typed_source_backed_route_failures_keep_stable_classification() {
    let cases = [
        (
            SourceBackedRouteErrorKind::Unsupported,
            ImportFailureType::UnsupportedSchema,
        ),
        (
            SourceBackedRouteErrorKind::InvalidSource,
            ImportFailureType::MalformedSource,
        ),
    ];
    for (kind, expected) in cases {
        let error = anyhow::Error::new(SourceBackedRouteError::new(kind, "typed failure"));
        assert_eq!(import_error_scope(&error), ImportFailureScope::Source);
        assert_eq!(import_failure_type(&error), expected);
    }
}

#[test]
fn ctx_owned_io_is_system_scoped() {
    let error = anyhow::Error::new(CaptureError::SystemIo {
        operation: "publish source generation",
        source: std::io::Error::other("disk failure"),
    });
    assert_eq!(import_error_scope(&error), ImportFailureScope::System);
    assert_eq!(import_failure_type(&error), ImportFailureType::SystemIo);
}
