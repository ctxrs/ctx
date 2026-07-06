#[allow(unused_imports)]
use super::*;

pub(crate) const MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn history_source_plugin_import_json(
    source: &HistorySourcePluginSource,
    stats: &SourceStats,
    summary: &ProviderImportSummary,
) -> Value {
    json!({
        "status": "imported",
        "provider": CaptureProvider::Custom.as_str(),
        "kind": "history_source_plugin",
        "plugin": source.plugin_name,
        "history_source": source.label(),
        "provider_key": source.provider_key,
        "source_id": source.source_id,
        "source_format": source.source_format,
        "manifest_path": source.manifest_path,
        "source_files": stats.files,
        "source_bytes": stats.bytes,
        "imported_sessions": summary.imported_sessions,
        "imported_events": summary.imported_events,
        "imported_edges": summary.imported_edges,
        "skipped_sessions": summary.skipped_sessions,
        "skipped_events": summary.skipped_events,
        "skipped_edges": summary.skipped_edges,
        "skipped": summary.skipped,
        "failed": summary.failed,
        "failures": provider_failures_json(summary),
    })
}

pub(crate) fn history_source_plugin_failure_json(
    source: &HistorySourcePluginSource,
    error: &str,
) -> Value {
    json!({
        "status": "failed",
        "provider": CaptureProvider::Custom.as_str(),
        "kind": "history_source_plugin",
        "plugin": source.plugin_name,
        "history_source": source.label(),
        "provider_key": source.provider_key,
        "source_id": source.source_id,
        "source_format": source.source_format,
        "manifest_path": source.manifest_path,
        "source_files": 0,
        "source_bytes": 0,
        "error": one_line_error(error),
    })
}

pub(crate) fn print_history_source_plugin_imported(
    source: &HistorySourcePluginSource,
    summary: &ProviderImportSummary,
) {
    println!(
        "imported history source plugin {}: sessions={} events={} edges={} skipped={} failed={}",
        source.label(),
        summary.imported_sessions,
        summary.imported_events,
        summary.imported_edges,
        summary.skipped,
        summary.failed
    );
}

pub(crate) fn print_history_source_plugin_failed(source: &HistorySourcePluginSource, error: &str) {
    println!(
        "skipped history source plugin {}: {}",
        source.label(),
        one_line_error(error)
    );
    println!("  manifest: {}", source.manifest_path.display());
}

pub(crate) fn refresh_sources_for_search(
    data_root: &Path,
    sources: Vec<SourceInfo>,
    plugin_sources: Vec<HistorySourcePluginSource>,
    refresh: RefreshArg,
    json_output: bool,
) -> Result<ImportTotals> {
    fs::create_dir_all(data_root)?;
    config::write_default_config(data_root)?;
    let db_path = database_path(data_root.to_path_buf());
    let planned_sources = sources
        .into_iter()
        .map(|source| (source, SourceStats::default()))
        .collect::<Vec<_>>();
    if planned_sources.is_empty() && plugin_sources.is_empty() {
        return Ok(ImportTotals::default());
    }

    let progress_arg = match refresh {
        RefreshArg::Strict if json_output => ProgressArg::Json,
        RefreshArg::Strict => ProgressArg::Auto,
        RefreshArg::Auto | RefreshArg::Off => ProgressArg::None,
    };
    let progress = ProgressReporter::new(progress_arg, json_output, "search-refresh", 0);
    let mut totals = ImportTotals::default();
    if should_parallelize_import(&planned_sources) {
        let source_states = Arc::new(Mutex::new(
            planned_sources
                .iter()
                .map(|(_, stats)| SourceProgressSnapshot {
                    completed_bytes: 0,
                    total_bytes: stats.bytes,
                })
                .collect::<Vec<_>>(),
        ));
        let handles = planned_sources
            .into_iter()
            .enumerate()
            .map(|(index, (source, stats))| {
                let db_path = db_path.clone();
                let progress_callback = progress.parallel_codex_import_callback(
                    &source,
                    index,
                    Arc::clone(&source_states),
                );
                thread::spawn(move || -> Result<ImportSourceOutcome> {
                    let mut store = Store::open(&db_path)?;
                    let summary = import_one_source_without_search_refresh(
                        &mut store,
                        &source,
                        progress_callback,
                        false,
                    )?;
                    Ok(ImportSourceOutcome {
                        index,
                        source,
                        stats,
                        summary,
                    })
                })
            })
            .collect::<Vec<_>>();

        let mut outcomes = Vec::with_capacity(handles.len());
        for handle in handles {
            let outcome = handle
                .join()
                .map_err(|_| anyhow!("provider import worker panicked"))??;
            outcomes.push(outcome);
        }
        outcomes.sort_by_key(|outcome| outcome.index);
        for outcome in outcomes {
            totals.add(&outcome.summary, &outcome.stats);
        }
    } else {
        let mut store = Store::open(&db_path)?;
        let mut completed_source_bytes = 0u64;
        for (source, stats) in planned_sources {
            progress.message(
                "refreshing",
                format!("importing {}", source.provider.as_str()),
            );
            let source_progress = progress.codex_import_callback(&source, completed_source_bytes);
            completed_source_bytes = completed_source_bytes.saturating_add(stats.bytes);
            let summary = import_one_source_without_search_refresh(
                &mut store,
                &source,
                source_progress,
                false,
            )?;
            totals.add(&summary, &stats);
            progress.done(
                "refreshing",
                format!("refreshed {}", source.provider.as_str()),
                completed_source_bytes,
            );
        }
    }

    if !plugin_sources.is_empty() {
        let mut store = Store::open(&db_path)?;
        for plugin_source in plugin_sources {
            progress.message(
                "refreshing",
                format!("running history source plugin {}", plugin_source.label()),
            );
            let (summary, stats) =
                import_history_source_plugin(&mut store, &plugin_source, data_root, false)
                    .with_context(|| {
                        format!("refresh history source plugin {}", plugin_source.label())
                    })?;
            totals.add(&summary, &stats);
            progress.done(
                "refreshing",
                format!("refreshed history source plugin {}", plugin_source.label()),
                0,
            );
        }
    }

    Store::open(&db_path)?.checkpoint_wal_truncate_if_larger_than(WAL_TRUNCATE_MIN_BYTES)?;
    Ok(totals)
}

pub(crate) fn history_source_plugin_import_requests(
    args: &ImportArgs,
    data_root: &Path,
    include_plugins: bool,
) -> Result<Vec<HistorySourcePluginSource>> {
    if !include_plugins {
        return Ok(Vec::new());
    }
    if !args.all && args.history_source.is_none() && args.history_source_manifest.is_empty() {
        return Ok(Vec::new());
    }
    let sources = discover_history_source_plugins(data_root, &args.history_source_manifest)?;
    if let Some(selector) = &args.history_source {
        let matches = sources
            .into_iter()
            .filter(|source| source.matches_selector(selector))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(anyhow!(
                "no history source plugin matched `{selector}`; use `ctx sources` to inspect configured plugins"
            ));
        }
        if matches.len() > 1 {
            let labels = matches
                .iter()
                .map(HistorySourcePluginSource::label)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(anyhow!(
                "history source plugin selector `{selector}` matched multiple sources ({labels}); use plugin/source or provider_key/source_id"
            ));
        }
        return Ok(matches);
    }
    if args.all {
        return Ok(sources
            .into_iter()
            .filter(|source| source.enabled)
            .collect());
    }
    Ok(sources
        .into_iter()
        .filter(|source| {
            args.history_source_manifest
                .iter()
                .any(|path| manifest_arg_matches_source(path, &source.manifest_path))
        })
        .collect())
}

pub(crate) fn import_history_source_plugin(
    store: &mut Store,
    source: &HistorySourcePluginSource,
    data_root: &Path,
    full_rescan: bool,
) -> Result<(ProviderImportSummary, SourceStats)> {
    let record = import_record_for_history_source_plugin(source);
    let record_id = record.id;
    let options = CustomHistoryJsonlV1ImportOptions::default();
    let machine_id = options.machine_id.clone();
    let cursor_stream = source.cursor_stream();
    let previous_cursor = if full_rescan {
        None
    } else {
        store
            .get_sync_cursor(None, &machine_id, &cursor_stream)?
            .map(|cursor| cursor.cursor)
    };
    let run = run_history_source_plugin(
        source,
        HistorySourcePluginRunOptions {
            data_root,
            machine_id: &machine_id,
            cursor: previous_cursor.as_deref(),
            cursor_stream: &cursor_stream,
            full_rescan,
        },
    )?;
    let _plugin_stderr = &run.stderr;
    validate_history_source_plugin_output(source, &run.stdout, &machine_id, full_rescan)?;
    let stdout = annotate_history_source_plugin_output(source, &run.stdout)?;
    let validation = validate_custom_history_jsonl_v1_reader(Cursor::new(stdout.as_slice()))
        .map_err(anyhow::Error::from)?;
    if validation.failed > 0 {
        return Err(history_source_plugin_import_failure(source, &validation));
    }
    let stats = SourceStats {
        files: 1,
        bytes: stdout.len() as u64,
    };
    store.upsert_record(&record)?;
    let summary = import_custom_history_jsonl_v1_reader(
        Cursor::new(stdout),
        store,
        CustomHistoryJsonlV1ImportOptions {
            machine_id,
            source_path: Some(source.manifest_path.clone()),
            history_record_id: Some(record_id),
            allow_partial_failures: false,
            ..options
        },
    )
    .map_err(anyhow::Error::from)?;
    if summary.failed > 0 {
        return Err(history_source_plugin_import_failure(source, &summary));
    }
    Ok((summary, stats))
}

pub(crate) fn annotate_history_source_plugin_output(
    source: &HistorySourcePluginSource,
    stdout: &[u8],
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(stdout.len());
    for (line_number, line) in history_source_plugin_stdout_lines(source, stdout)? {
        if line.trim().is_empty() {
            continue;
        }
        let mut record: CtxHistoryJsonlRecord = serde_json::from_str(line).with_context(|| {
            format!(
                "history source plugin {} emitted invalid ctx-history-jsonl-v1 at line {line_number}",
                source.label()
            )
        })?;
        if let CtxHistoryJsonlRecord::Source(source_record) = &mut record {
            let mut metadata = match std::mem::take(&mut source_record.metadata) {
                Value::Object(map) => map,
                Value::Null => serde_json::Map::new(),
                other => {
                    let mut map = serde_json::Map::new();
                    map.insert("metadata".to_owned(), other);
                    map
                }
            };
            metadata.insert(
                "ctx_history_plugin".to_owned(),
                json!({
                    "plugin_name": source.plugin_name,
                    "plugin_source_id": source.id,
                    "history_source": source.label(),
                    "plugin_display_name": source.plugin_display_name,
                    "plugin_version": source.plugin_version,
                    "manifest_path": source.manifest_path,
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "source_format": source.source_format,
                }),
            );
            source_record.metadata = Value::Object(metadata);
        }
        serde_json::to_writer(&mut out, &record).with_context(|| {
            format!(
                "serialize annotated history source plugin {} record at line {line_number}",
                source.label()
            )
        })?;
        out.push(b'\n');
    }
    Ok(out)
}

pub(crate) fn history_source_plugin_stdout_lines<'a>(
    source: &HistorySourcePluginSource,
    stdout: &'a [u8],
) -> Result<Vec<(usize, &'a str)>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_number = 1usize;
    for (index, byte) in stdout.iter().enumerate() {
        let len = index.saturating_add(1).saturating_sub(start);
        if len > MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES {
            return Err(anyhow!(
                "history source plugin {} emitted ctx-history-jsonl-v1 line {line_number} exceeding max bytes ({MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES})",
                source.label()
            ));
        }
        if *byte == b'\n' {
            let line = std::str::from_utf8(&stdout[start..index]).with_context(|| {
                format!(
                    "history source plugin {} emitted non-UTF-8 ctx-history-jsonl-v1 output at line {line_number}",
                    source.label()
                )
            })?;
            lines.push((line_number, line));
            start = index + 1;
            line_number += 1;
        }
    }
    if start < stdout.len() {
        let len = stdout.len().saturating_sub(start);
        if len > MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES {
            return Err(anyhow!(
                "history source plugin {} emitted ctx-history-jsonl-v1 line {line_number} exceeding max bytes ({MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES})",
                source.label()
            ));
        }
        let line = std::str::from_utf8(&stdout[start..]).with_context(|| {
            format!(
                "history source plugin {} emitted non-UTF-8 ctx-history-jsonl-v1 output at line {line_number}",
                source.label()
            )
        })?;
        lines.push((line_number, line));
    }
    Ok(lines)
}

pub(crate) fn history_source_plugin_import_failure(
    source: &HistorySourcePluginSource,
    summary: &ProviderImportSummary,
) -> anyhow::Error {
    let detail = summary
        .failures
        .first()
        .map(|failure| format!("line {}: {}", failure.line, failure.error))
        .unwrap_or_else(|| "unknown validation failure".to_owned());
    anyhow!(
        "history source plugin {} import failed with {} failure(s); first failure: {detail}",
        source.label(),
        summary.failed
    )
}

pub(crate) fn import_record_for_history_source_plugin(
    source: &HistorySourcePluginSource,
) -> HistoryRecord {
    let key = format!(
        "history-source-plugin:{}:{}:{}:{}:{}",
        source.plugin_name, source.id, source.provider_key, source.source_id, source.source_format
    );
    let mut record = HistoryRecord::new(
        format!("history source plugin {}", source.label()),
        format!(
            "Indexed custom agent history from history source plugin {} ({})",
            source.label(),
            source.source_format
        ),
        vec![
            "agent-history".into(),
            "custom".into(),
            "history-source-plugin".into(),
            source.provider_key.clone(),
            source.source_format.clone(),
        ],
        "agent_history",
        source
            .manifest_path
            .parent()
            .map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}

pub(crate) fn plugin_sources_json(sources: &[HistorySourcePluginSource]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            json!({
                "provider": CaptureProvider::Custom.as_str(),
                "kind": "history_source_plugin",
                "plugin": source.plugin_name,
                "plugin_display_name": source.plugin_display_name,
                "plugin_version": source.plugin_version,
                "history_source": source.label(),
                "history_source_id": source.id,
                "display_name": source.display_name,
                "provider_key": source.provider_key,
                "source_id": source.source_id,
                "source_format": source.source_format,
                "manifest_path": source.manifest_path,
                "enabled": source.enabled,
                "refresh": history_source_plugin_refresh_json(source.refresh),
                "status": "available",
                "import_support": "history_source_plugin",
                "native_import": false,
                "importable": true,
                "raw_retention": "metadata_only",
                "unsupported_reason": null,
            })
        })
        .collect()
}

pub(crate) fn plugin_manifest_failures_json(
    failures: &[HistorySourcePluginManifestFailure],
) -> Vec<Value> {
    failures
        .iter()
        .map(|failure| {
            json!({
                "provider": CaptureProvider::Custom.as_str(),
                "kind": "history_source_plugin",
                "plugin": null,
                "plugin_display_name": null,
                "plugin_version": null,
                "history_source": null,
                "history_source_id": null,
                "display_name": null,
                "provider_key": null,
                "source_id": null,
                "source_format": null,
                "manifest_path": failure.manifest_path,
                "enabled": false,
                "refresh": null,
                "status": "invalid",
                "import_support": "history_source_plugin",
                "native_import": false,
                "importable": false,
                "raw_retention": "metadata_only",
                "unsupported_reason": failure.error,
                "error": failure.error,
            })
        })
        .collect()
}

pub(crate) fn history_source_plugin_refresh_json(
    refresh: HistorySourcePluginRefresh,
) -> &'static str {
    match refresh {
        HistorySourcePluginRefresh::Manual => "manual",
        HistorySourcePluginRefresh::Auto => "auto",
    }
}
