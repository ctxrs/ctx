use super::*;

pub(crate) fn run_explicit_format_import(
    args: &ImportArgs,
    format: ImportFormatArg,
    db_path: PathBuf,
    mut store: Store,
    analytics_properties: &mut AnalyticsProperties,
    options: ImportRunOptions,
) -> Result<ImportReport> {
    let path = args
        .path
        .as_ref()
        .context("--format requires an explicit --path")?;
    if !path
        .try_exists()
        .with_context(|| format!("check import path {}", path.display()))?
    {
        return Err(anyhow!("import path does not exist: {}", path.display()));
    }
    let stats =
        source_stats(path).with_context(|| format!("scan import source {}", path.display()))?;
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
            "found 1 {} source, {}",
            format.as_str(),
            format_bytes(stats.bytes)
        ),
    );
    if let Some(warning) = low_disk_space_warning(&db_path, stats.bytes) {
        progress.warning(warning);
    }
    if (stats.files >= LARGE_IMPORT_SOURCE_FILES_WARNING
        || stats.bytes >= LARGE_IMPORT_SOURCE_BYTES_WARNING)
        && stats.files > 0
    {
        let warning = format!(
            "large import: {} source file(s), {}; initial indexing may use sustained CPU and disk",
            stats.files,
            format_bytes(stats.bytes)
        );
        progress.warning(warning);
    }

    let validation = match format {
        ImportFormatArg::CtxHistoryJsonlV1 => {
            validate_custom_history_jsonl_v1(path).map_err(anyhow::Error::from)?
        }
    };
    if validation.failed > 0 {
        return Err(explicit_format_import_failure(format, &validation));
    }

    let record = import_record_for_custom_history(path, format);
    let record_id = record.id;
    store.upsert_record(&record)?;
    progress.message("indexing", format!("importing {}", format.as_str()));
    let summary = match format {
        ImportFormatArg::CtxHistoryJsonlV1 => import_custom_history_jsonl_v1(
            path,
            &mut store,
            CustomHistoryJsonlV1ImportOptions {
                source_path: Some(path.clone()),
                history_record_id: Some(record_id),
                allow_partial_failures: false,
                ..CustomHistoryJsonlV1ImportOptions::default()
            },
        )
        .map_err(anyhow::Error::from)?,
    };
    if summary.failed > 0 {
        return Err(explicit_format_import_failure(format, &summary));
    }

    let mut totals = ImportTotals::default();
    totals.add(&summary, &stats);
    if totals.imported_sessions > 0 || totals.imported_events > 0 || totals.imported_edges > 0 {
        progress.message("finalizing", "optimizing search index");
        Store::open(&db_path)?.optimize_search_index()?;
    }
    progress.message("finalizing", "checkpointing search database");
    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    if options.print_human {
        progress.finish_line();
    }
    progress.done(
        "finalizing",
        format!("indexed 1 {} source file", format.as_str()),
        stats.bytes,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "source_files_bucket",
        stats.files as u64,
    );
    analytics::insert_count_bucket(analytics_properties, "failed_sources_bucket", 0);
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
    analytics::insert_count_bucket(analytics_properties, "failed_bucket", totals.failed as u64);
    Ok(ImportReport {
        resume: args.resume,
        totals,
        sources: vec![custom_format_import_json(format, path, &stats, &summary)],
    })
}

pub(crate) fn explicit_format_import_failure(
    format: ImportFormatArg,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    let detail = summary
        .failures
        .first()
        .map(|failure| format!("line {}: {}", failure.line, failure.error))
        .unwrap_or_else(|| "unknown validation failure".to_owned());
    anyhow!(
        "{} import failed with {} failure(s); first failure: {detail}",
        format.as_str(),
        summary.failed
    )
}
