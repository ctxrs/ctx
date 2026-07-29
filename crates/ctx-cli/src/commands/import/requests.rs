use std::{fs, io::Cursor, path::Path};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use ctx_history_capture::{
    decode_custom_history_jsonl_v1_cursor, import_custom_history_jsonl_v1_reader,
    provider_source_spec, CaptureError, CaptureWorkLimit, CustomHistoryJsonlV1ImportOptions,
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ImportProfile, ProviderImportSummary,
    ProviderImportSupport, ProviderSourceStatus,
};
use ctx_history_core::{CaptureProvider, CtxHistoryJsonlRecord};
use ctx_history_store::Store;

use crate::commands::import::catalog::{
    import_record_for_history_source_plugin, import_record_for_source,
};
use crate::commands::import::native::validate_source_import_supported;
use crate::commands::import::{
    cleanup_rejected_history_record, history_record_exists,
    import_custom_history_with_canonical_pro_progression, provider_summary_has_imported_content,
    rejected_source_error, CanonicalProSourceProgression, SourceStats,
};
use crate::history_source_plugins::{
    discover_history_source_plugins, run_history_source_plugin, HistorySourcePluginRunOptions,
    HistorySourcePluginSource,
};
use crate::provider_sources::{
    discovered_sources, discovered_sources_for_provider_report, explicit_path_source,
    manual_path_guidance, provider_cli_name, SourceInfo,
};
use crate::{ImportArgs, MAX_HISTORY_SOURCE_PLUGIN_JSONL_LINE_BYTES};

pub(crate) fn validate_import_args(args: &ImportArgs) -> Result<()> {
    if args.input_format.is_some() && args.path.is_none() {
        return Err(anyhow!(
            "ctx import --input-format requires --path for a source-backed catalog entry"
        ));
    }
    if args.path.is_some() && args.input_format.is_none() && args.provider.is_none() {
        return Err(anyhow!(
            "ctx import --path requires --provider for native provider history; use `ctx import --provider codex --path <path>` or `ctx import --input-format ctx-history-jsonl-v1 --path <file>`"
        ));
    }
    if args.history_source.is_some() || !args.history_source_manifest.is_empty() {
        return Err(anyhow!(
            "history source plugin imports have no source-backed adapter; no legacy import fallback was used"
        ));
    }
    Ok(())
}

pub(crate) fn import_requests(args: &ImportArgs, store: Option<&Store>) -> Result<Vec<SourceInfo>> {
    if args.history_source.is_some() || !args.history_source_manifest.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(path) = &args.path {
        let provider = args
            .provider
            .context("ctx import --path requires --provider for native provider history")?
            .capture_provider();
        let source = explicit_path_source(provider, path.clone());
        validate_source_import_supported(&source)?;
        let missing = !source
            .path
            .try_exists()
            .with_context(|| format!("check import path {}", source.path.display()))?;
        let authorized = if missing {
            store
                .map(|store| missing_explicit_path_has_prior_route_authority(store, &source))
                .transpose()?
                .unwrap_or(false)
        } else {
            false
        };
        if missing && !authorized {
            return Err(invalid_missing_explicit_path(&source));
        }
        return Ok(vec![source]);
    }
    if args.all || args.provider.is_none() {
        return Ok(discovered_sources()
            .into_iter()
            .filter(|source| {
                source.exists
                    && source.import_support.is_auto_importable()
                    && source.status == ProviderSourceStatus::Available
            })
            .collect());
    }
    let provider = args.provider.expect("checked provider").capture_provider();
    let report = discovered_sources_for_provider_report(provider);
    let sources = report
        .sources
        .iter()
        .filter(|source| {
            source.provider == provider
                && source.exists
                && source.import_support.is_importable()
                && source.status == ProviderSourceStatus::Available
        })
        .cloned()
        .collect::<Vec<_>>();
    if sources.is_empty() {
        let spec = provider_source_spec(provider);
        if spec
            .is_some_and(|spec| matches!(spec.import_support, ProviderImportSupport::Unsupported))
        {
            let reason = spec
                .and_then(|spec| spec.unsupported_reason)
                .unwrap_or("no native local-history parser is implemented");
            return Err(anyhow!(
                "{} native import is unsupported: {reason}",
                provider.as_str()
            ));
        }
        return Err(no_importable_provider_sources_error(provider, &report));
    }
    for source in &sources {
        validate_source_import_supported(source)?;
    }
    Ok(sources)
}

pub(crate) fn missing_explicit_path_has_prior_route_authority(
    store: &Store,
    source: &SourceInfo,
) -> Result<bool> {
    let record_id = import_record_for_source(source).id;
    Ok(history_record_exists(store, record_id)?
        && store.has_prior_provider_source_route(
            source.provider,
            source.source_format,
            &source.path,
        )?)
}

pub(crate) fn invalid_missing_explicit_path(source: &SourceInfo) -> anyhow::Error {
    anyhow::Error::new(CaptureError::InvalidProviderTranscriptPath {
        path: source.path.clone(),
        reason: "missing explicit path has no matching prior provider route authority",
    })
}

pub(crate) fn no_importable_provider_sources_error(
    provider: CaptureProvider,
    report: &DiscoveryReport,
) -> anyhow::Error {
    let mut message = format!("no importable {} history found", provider.as_str());
    if let Some(source) = report.sources.iter().find(|source| {
        source.status == ProviderSourceStatus::Unsupported
            || source.import_support == ProviderImportSupport::Unsupported
    }) {
        message.push_str(&format!(
            "\ndetected unsupported history at {}",
            source.path.display()
        ));
        if let Some(reason) = source.unsupported_reason {
            message.push_str(&format!(": {reason}"));
        }
        message.push_str("\ncurrent ctx cannot import that path");
    } else if let Some(issue) = report.issues.first() {
        append_discovery_issue(&mut message, issue);
    } else if report.sources.is_empty() {
        message.push_str("; no default paths are registered for this provider");
    } else {
        message.push_str("\nchecked paths:");
        for source in &report.sources {
            message.push_str(&format!(
                "\n  {} ({})",
                source.path.display(),
                source.status.as_str()
            ));
            if let Some(reason) = source.unsupported_reason {
                message.push_str(&format!(" - {reason}"));
            }
        }
    }
    message.push_str(&format!(
        "\nuse `ctx sources` or `ctx sources --provider {}` to inspect discovery, or use `{}`",
        provider_cli_name(provider),
        manual_path_guidance(provider)
    ));
    anyhow!(message)
}

fn append_discovery_issue(message: &mut String, issue: &DiscoveryIssue) {
    match issue.kind {
        DiscoveryIssueKind::NoDiskHistory => {
            message.push_str(&format!("; no disk history is selected: {}", issue.reason))
        }
        DiscoveryIssueKind::SelectorUnreconstructible => message.push_str(&format!(
            "; the automatic history location cannot be safely reconstructed: {}",
            issue.reason
        )),
        DiscoveryIssueKind::InsufficientOfficialEvidence => {
            message.push_str("; no official automatic history location is established")
        }
    }
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
        return reject_history_source_plugin_fallback(matches);
    }
    if args.all {
        return reject_history_source_plugin_fallback(
            sources
                .into_iter()
                .filter(|source| source.enabled)
                .collect(),
        );
    }
    reject_history_source_plugin_fallback(
        sources
            .into_iter()
            .filter(|source| {
                args.history_source_manifest
                    .iter()
                    .any(|path| manifest_arg_matches_source(path, &source.manifest_path))
            })
            .collect(),
    )
}

fn reject_history_source_plugin_fallback(
    sources: Vec<HistorySourcePluginSource>,
) -> Result<Vec<HistorySourcePluginSource>> {
    if sources.is_empty() {
        return Ok(sources);
    }
    let labels = sources
        .iter()
        .take(3)
        .map(HistorySourcePluginSource::label)
        .collect::<Vec<_>>()
        .join(", ");
    Err(anyhow!(
        "history source plugin format(s) have no source-backed adapter ({labels}); no legacy import fallback was used"
    ))
}

pub(crate) fn manifest_arg_matches_source(arg: &Path, manifest_path: &Path) -> bool {
    if arg.is_file() {
        return same_pathish(arg, manifest_path);
    }
    if arg.is_dir() {
        return manifest_path.starts_with(arg);
    }
    same_pathish(arg, manifest_path)
}

pub(crate) fn same_pathish(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

pub(crate) fn import_history_source_plugin<P: CanonicalProSourceProgression + ?Sized>(
    store: &mut Store,
    source: &HistorySourcePluginSource,
    data_root: &Path,
    full_rescan: bool,
    requested_work_limit: CaptureWorkLimit,
    import_profile: &ImportProfile,
    pro_output: Option<&mut P>,
) -> Result<(ProviderImportSummary, SourceStats)> {
    let record = import_record_for_history_source_plugin(source);
    let record_id = record.id;
    let options = CustomHistoryJsonlV1ImportOptions {
        source_path: Some(source.manifest_path.clone()),
        history_record_id: Some(record_id),
        import_profile: import_profile.clone(),
        ..CustomHistoryJsonlV1ImportOptions::default()
    };
    let machine_id = options.machine_id.clone();
    let cursor_stream = source.cursor_stream();
    let previous_cursor = if full_rescan {
        None
    } else {
        store
            .get_sync_cursor(None, &machine_id, &cursor_stream)?
            .map(|cursor| decode_custom_history_jsonl_v1_cursor(&cursor.cursor))
            .transpose()?
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
    let stats = SourceStats {
        files: 1,
        bytes: stdout.len() as u64,
        change_token: None,
    };
    let record_existed = history_record_exists(store, record_id)?;
    store.upsert_record(&record)?;
    let summary = import_custom_history_with_canonical_pro_progression(
        requested_work_limit,
        pro_output,
        |capture_work_limit| {
            import_custom_history_jsonl_v1_reader(
                Cursor::new(stdout.as_slice()),
                store,
                CustomHistoryJsonlV1ImportOptions {
                    capture_work_limit,
                    ..options.clone()
                },
            )
            .map_err(anyhow::Error::from)
        },
    )?;
    if summary.failed > 0 && !provider_summary_has_imported_content(&summary) {
        cleanup_rejected_history_record(store, record_id, record_existed)?;
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
        let mut record: CtxHistoryJsonlRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(_) => {
                out.extend_from_slice(line.as_bytes());
                out.push(b'\n');
                continue;
            }
        };
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

pub(crate) fn validate_history_source_plugin_output(
    source: &HistorySourcePluginSource,
    stdout: &[u8],
    machine_id: &str,
    require_after_cursor: bool,
) -> Result<()> {
    let mut saw_source = false;
    let mut saw_after_cursor = false;
    for (_line_number, line) in history_source_plugin_stdout_lines(source, stdout)? {
        if line.trim().is_empty() {
            continue;
        }
        let record: CtxHistoryJsonlRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(_) => continue,
        };
        let CtxHistoryJsonlRecord::Source(source_record) = record else {
            continue;
        };
        saw_source = true;
        if source_record
            .cursor
            .as_ref()
            .and_then(|cursor| cursor.after.as_ref())
            .is_some()
        {
            saw_after_cursor = true;
        }
        if source_record.provider_key != source.provider_key
            || source_record.source_id != source.source_id
            || source_record.source_format != source.source_format
        {
            return Err(anyhow!(
                "history source plugin {} emitted source identity {}/{}/{} but manifest declares {}/{}/{}",
                source.label(),
                source_record.provider_key,
                source_record.source_id,
                source_record.source_format,
                source.provider_key,
                source.source_id,
                source.source_format
            ));
        }
        if let Some(source_machine_id) = source_record.machine_id {
            if source_machine_id != machine_id {
                return Err(anyhow!(
                    "history source plugin {} emitted machine_id `{source_machine_id}` but ctx is importing as `{machine_id}`; omit machine_id or set it to CTX_HISTORY_MACHINE_ID",
                    source.label()
                ));
            }
        }
    }
    if !saw_source {
        return Err(anyhow!(
            "history source plugin {} emitted no source record",
            source.label()
        ));
    }
    if require_after_cursor && !saw_after_cursor {
        return Err(anyhow!(
            "history source plugin {} was reset but emitted no source.cursor.after checkpoint; emit a fresh cursor after a full rescan",
            source.label()
        ));
    }
    Ok(())
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
    rejected_source_error(
        format!(
            "history source plugin {} import failed with {} failure(s); first failure: {detail}",
            source.label(),
            summary.failed
        ),
        summary,
    )
}
