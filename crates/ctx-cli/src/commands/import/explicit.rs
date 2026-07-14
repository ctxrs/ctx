use super::*;

pub(crate) fn run_explicit_format_import(
    args: &ImportArgs,
    format: ImportFormatArg,
    db_path: PathBuf,
    mut store: Store,
    admission: &IndexingAdmission,
    analytics_properties: &mut AnalyticsProperties,
    options: ImportRunOptions,
) -> Result<ImportReport> {
    let path = args
        .path
        .as_ref()
        .context("--format requires an explicit --path")?;
    let stats = match source_stats(path)
        .with_context(|| format!("scan import source {}", path.display()))
    {
        Ok(stats) => stats,
        Err(error) if import_error_scope(&error) == ImportFailureScope::System => {
            return Err(error);
        }
        Err(error) => {
            return Ok(explicit_format_failure_report(
                args,
                format,
                path,
                SourceStats::default(),
                &error,
                analytics_properties,
            ));
        }
    };
    analytics::insert_count_bucket(analytics_properties, "sources_seen_bucket", 1);
    analytics::insert_bytes_bucket(analytics_properties, "source_bytes_bucket", stats.bytes);

    let progress = ProgressReporter::new(
        options.progress,
        options.json,
        options.operation,
        stats.bytes,
    );
    progress.message(
        "discovering",
        format!(
            "Found 1 {} source ({}).",
            format.as_str(),
            format_bytes(stats.bytes)
        ),
    );
    if let Some(warning) = low_disk_space_warning(&db_path, stats.bytes) {
        progress.warning(warning);
    }
    store.ensure_disk_headroom(
        super::estimated_bulk_import_write_bytes(stats.bytes),
        "explicit history import",
    )?;
    if (stats.files >= LARGE_IMPORT_SOURCE_FILES_WARNING
        || stats.bytes >= LARGE_IMPORT_SOURCE_BYTES_WARNING)
        && stats.files > 0
    {
        let notice = format!(
            "Large first import: scanning {} existing history {} ({}). This may take a while.",
            format_count(stats.files),
            plural(stats.files, "file", "files"),
            format_bytes(stats.bytes)
        );
        progress.notice(notice);
    }

    let record = import_record_for_custom_history(path, format);
    let record_id = record.id;
    let record_existed = history_record_exists(&store, record_id)?;
    store.upsert_record(&record)?;
    progress.message("indexing", format!("importing {}", format.as_str()));
    let import_result = match format {
        ImportFormatArg::CtxHistoryJsonlV1 => import_custom_history_jsonl_v1(
            path,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(path.clone()),
                history_record_id: Some(record_id),
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from),
    };
    let mut summary = match import_result {
        Ok(summary) => summary,
        Err(error) if import_error_scope(&error) == ImportFailureScope::System => {
            return Err(error);
        }
        Err(error) => {
            cleanup_rejected_history_record(&store, record_id, record_existed)?;
            return Ok(explicit_format_failure_report(
                args,
                format,
                path,
                stats,
                &error,
                analytics_properties,
            ));
        }
    };
    let mut totals = ImportTotals::default();
    if !provider_summary_has_imported_content(&summary) {
        if summary.failed == 0 {
            summary.failed = 1;
            summary.failures.push(ProviderImportFailure {
                line: 0,
                error: format!(
                    "{} source contained no usable history content",
                    format.as_str()
                ),
            });
        }
        cleanup_rejected_history_record(&store, record_id, record_existed)?;
        totals.add_rejected_source(&summary, &stats);
    } else {
        totals.add(&summary, &stats);
    }
    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        progress.message("finalizing", "checkpointing search database");
        Store::open_admitted(&db_path, admission)?.checkpoint_wal_for_pressure()?;
    }
    if options.print_human {
        progress.finish_line();
    }
    progress.done(
        "finalizing",
        format!("processed 1 {} source file", format.as_str()),
        stats.bytes,
    );
    insert_explicit_format_analytics(analytics_properties, &stats, &totals);
    Ok(ImportReport {
        resume: args.resume,
        totals,
        inventory: InventoryTotals {
            sources: 1,
            source_files: stats.files,
            source_bytes: stats.bytes,
            ..InventoryTotals::default()
        },
        catalog: CatalogTotals::default(),
        catalog_sources: Vec::new(),
        sources: vec![custom_format_import_json(format, path, &stats, &summary)],
    })
}

fn explicit_format_failure_report(
    args: &ImportArgs,
    format: ImportFormatArg,
    path: &Path,
    stats: SourceStats,
    error: &anyhow::Error,
    analytics_properties: &mut AnalyticsProperties,
) -> ImportReport {
    let mut totals = ImportTotals::default();
    totals.add_source_failure(&stats);
    insert_explicit_format_analytics(analytics_properties, &stats, &totals);
    ImportReport {
        resume: args.resume,
        totals,
        inventory: InventoryTotals {
            sources: 1,
            source_files: stats.files,
            source_bytes: stats.bytes,
            ..InventoryTotals::default()
        },
        catalog: CatalogTotals::default(),
        catalog_sources: Vec::new(),
        sources: vec![custom_format_failure_json(
            format,
            path,
            &stats,
            &error_summary(error),
            import_failure_type(error),
        )],
    }
}

fn insert_explicit_format_analytics(
    analytics_properties: &mut AnalyticsProperties,
    stats: &SourceStats,
    totals: &ImportTotals,
) {
    analytics::insert_count_bucket(
        analytics_properties,
        "source_files_bucket",
        stats.files as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "failed_sources_bucket",
        totals.failed_sources as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "sessions_imported_bucket",
        totals.imported_sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "events_imported_bucket",
        totals.imported_events as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "edges_imported_bucket",
        totals.imported_edges as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "skipped_bucket",
        totals.skipped as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "rejected_records_bucket",
        totals.failed as u64,
    );
}
