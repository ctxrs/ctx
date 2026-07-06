use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use ctx_history_capture::{
    catalog_codex_session_tree, CodexSessionCatalogOptions, ProviderSourceStatus,
};
use ctx_history_core::{database_path, CaptureProvider};
use ctx_history_store::Store;

use crate::analytics::AnalyticsProperties;
use crate::commands::import::{
    import_totals_json, run_import_internal, CatalogTotals, ImportReport, ImportRunOptions,
};
use crate::config::CONFIG_FILE;
use crate::output::print_json;
use crate::progress::ProgressReporter;
use crate::provider_sources::{discovered_sources, sources_json, SourceInfo};
use crate::{analytics, config, ImportArgs, SetupArgs};

pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    fs::create_dir_all(&data_root)?;
    let db_path = database_path(data_root.clone());
    let store = Store::open(&db_path)?;
    let config_path = data_root.join(CONFIG_FILE);
    config::write_default_config(&data_root)?;
    let sources = discovered_sources();
    let progress = ProgressReporter::new(args.progress, args.json, "setup", 0);
    progress.message("cataloging", "cataloging discovered Codex sessions");
    let (catalog, catalog_sources) = catalog_available_sources(&store, &sources)?;
    progress.done(
        "cataloging",
        format!("cataloged {} Codex sessions", catalog.cataloged_sessions),
        catalog.source_bytes,
    );
    let catalog_counts = store.catalog_session_counts()?;
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_detected_bucket",
        sources.len() as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "cataloged_sessions_bucket",
        catalog.cataloged_sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "pending_sessions_bucket",
        catalog_counts.pending as u64,
    );
    analytics::insert_bytes_bucket(
        analytics_properties,
        "catalog_source_bytes_bucket",
        catalog.source_bytes,
    );
    let import_report = if args.catalog_only {
        None
    } else {
        drop(store);
        let import_args = ImportArgs {
            provider: None,
            path: None,
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            format: None,
            all: true,
            resume: false,
            json: args.json,
            progress: args.progress,
        };
        Some(run_import_internal(
            &import_args,
            data_root.clone(),
            analytics_properties,
            ImportRunOptions {
                progress: args.progress,
                json: args.json,
                print_human: false,
                allow_empty_sources: true,
                include_history_source_plugins: false,
                operation: "setup",
            },
        )?)
    };
    let setup_store = Store::open(&db_path)?;
    let catalog_counts = setup_store.catalog_session_counts()?;
    let indexed_items = indexed_history_item_count(&setup_store)?;

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "mode": if args.catalog_only { "catalog_only" } else { "ready" },
            "indexed_items": indexed_items,
            "sources": sources_json(&sources),
            "catalog": {
                "sources": catalog.sources,
                "source_files": catalog.source_files,
                "source_bytes": catalog.source_bytes,
                "cataloged_sessions": catalog.cataloged_sessions,
                "cached_sessions": catalog.cached_sessions,
                "parsed_sessions": catalog.parsed_sessions,
                "indexed_sessions": catalog_counts.indexed,
                "pending_sessions": catalog_counts.pending,
                "skipped_sessions": catalog.skipped_sessions,
                "failed_sessions": catalog.failed_sessions,
                "failed_index_sessions": catalog_counts.failed,
                "stale_sessions": catalog_counts.stale,
            },
            "catalog_sources": catalog_sources,
            "import": setup_import_json(import_report.as_ref()),
            "network_required": false,
            "repo_writes": false,
        }))?;
    } else {
        progress.finish_line();
        print_setup_status_line(
            import_report.as_ref(),
            args.catalog_only,
            catalog_counts.pending,
            indexed_items,
        );
        if !setup_has_indexed_content(indexed_items) && catalog.cataloged_sessions > 0 {
            println!("Cataloged {} session(s).", catalog.cataloged_sessions);
        }
        if let Some(report) = &import_report {
            if report.totals.imported_sources > 0
                || report.totals.imported_sessions > 0
                || report.totals.imported_events > 0
            {
                println!(
                    "Imported {} session(s), {} event(s) from {} source(s).",
                    report.totals.imported_sessions,
                    report.totals.imported_events,
                    report.totals.imported_sources
                );
            }
            if report.totals.failed_sources > 0 {
                println!("Skipped {} source(s).", report.totals.failed_sources);
            }
        }
        println!("Data: {}", data_root.display());
        println!();
        println!("Get started:");
        if args.catalog_only {
            println!("  ctx import --all");
            println!("  ctx sources");
        } else if setup_has_indexed_content(indexed_items) {
            println!("  ctx search \"test failure\"");
            println!("  ctx show event <event-id> --window 3");
            println!("  ctx show session <session-id>");
            println!("  ctx sources");
            if setup_has_failed_sources(import_report.as_ref()) {
                println!("  ctx import --provider <provider>");
            }
        } else {
            println!("  ctx sources");
            println!("  ctx import --all");
        }
    }
    Ok(())
}

pub(crate) fn setup_import_json(report: Option<&ImportReport>) -> Value {
    match report {
        Some(report) => json!({
            "ran": true,
            "resume": report.resume,
            "resume_mode": report.resume_mode(),
            "totals": import_totals_json(&report.totals),
            "sources": report.sources.clone(),
        }),
        None => json!({
            "ran": false,
            "reason": "catalog_only",
        }),
    }
}

pub(crate) fn print_setup_status_line(
    report: Option<&ImportReport>,
    catalog_only: bool,
    pending_catalog_sessions: usize,
    indexed_items: usize,
) {
    if catalog_only {
        if pending_catalog_sessions > 0 {
            println!("ctx catalog is ready; import is still pending");
        } else {
            println!("ctx catalog is ready");
        }
        return;
    }
    let Some(report) = report else {
        println!("ctx is initialized; no local history was indexed");
        return;
    };
    if setup_has_indexed_content(indexed_items) && report.totals.failed_sources > 0 {
        println!("ctx indexed available local agent history; some sources were skipped");
    } else if setup_has_indexed_content(indexed_items) {
        println!("ctx local agent history search is ready");
    } else {
        println!("ctx is initialized; no local history was indexed");
    }
}

pub(crate) fn setup_has_indexed_content(indexed_items: usize) -> bool {
    indexed_items > 0
}

pub(crate) fn indexed_history_item_count(store: &Store) -> Result<usize> {
    Ok(store.indexed_history_item_count()?)
}

pub(crate) fn insert_store_analytics_counts(
    analytics_properties: &mut AnalyticsProperties,
    store: &Store,
) -> Result<()> {
    let counts = store.indexed_history_counts()?;
    analytics::insert_count_bucket(
        analytics_properties,
        "indexed_sessions_bucket",
        counts.sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "indexed_events_bucket",
        counts.events as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "indexed_items_bucket",
        counts.items() as u64,
    );
    Ok(())
}

pub(crate) fn insert_db_size_bucket(
    analytics_properties: &mut AnalyticsProperties,
    db_path: &Path,
) {
    let bytes = fs::metadata(db_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    analytics::insert_bytes_bucket(analytics_properties, "db_size_bucket", bytes);
}

pub(crate) fn setup_has_failed_sources(report: Option<&ImportReport>) -> bool {
    report.is_some_and(|report| report.totals.failed_sources > 0)
}
pub(crate) fn catalog_available_sources(
    store: &Store,
    sources: &[SourceInfo],
) -> Result<(CatalogTotals, Vec<Value>)> {
    let mut totals = CatalogTotals::default();
    let mut catalog_sources = Vec::new();
    for source in sources {
        if source.provider != CaptureProvider::Codex
            || source.source_format != "codex_session_jsonl_tree"
            || !source.exists
            || source.status != ProviderSourceStatus::Available
        {
            continue;
        }
        let summary = catalog_codex_session_tree(
            &source.path,
            store,
            CodexSessionCatalogOptions {
                source_root: Some(source.path.clone()),
                allow_partial_failures: true,
                ..CodexSessionCatalogOptions::default()
            },
        )
        .with_context(|| format!("catalog Codex sessions from {}", source.path.display()))?;
        totals.add(&summary);
        catalog_sources.push(json!({
            "provider": source.provider.as_str(),
            "path": source.path,
            "source_format": source.source_format,
            "source_files": summary.source_files,
            "source_bytes": summary.source_bytes,
            "cataloged_sessions": summary.cataloged_sessions,
            "cached_sessions": summary.cached_sessions,
            "parsed_sessions": summary.parsed_sessions,
            "skipped_sessions": summary.skipped_sessions,
            "failed_sessions": summary.failed_sessions,
        }));
    }
    Ok((totals, catalog_sources))
}
