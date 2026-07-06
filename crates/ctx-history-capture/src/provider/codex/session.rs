use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
    thread,
};

use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use serde_json::Value;

use crate::CodexSessionJsonlAdapter;

use crate::common::io::{
    collect_jsonl_paths, ensure_regular_provider_transcript_file, read_provider_jsonl_line,
};
use crate::provider::importer::{
    import_normalized_provider_captures, import_provider_capture_line,
    import_provider_capture_lines, import_provider_file_touched_line,
    resolve_pending_provider_edges, ProviderImportCaches,
};
use crate::{
    CaptureError, CodexEventImportMode, CodexSessionImportOptions, CodexToolOutputMode,
    NormalizedProviderImportOptions, ProviderAdapterContext, ProviderCaptureAdapter,
    ProviderImportFailure, ProviderImportSummary, ProviderNormalizationResult, Result,
    CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES,
};

use crate::provider::codex::events::{
    codex_session_capture, codex_session_header, codex_session_line_capture,
    codex_session_line_timestamp, CodexSessionLineContext, CodexToolCallContext,
};
use crate::provider::codex::fast_import::{
    codex_session_paths_total_bytes, import_codex_provider_event_fast,
    import_codex_session_paths_fast, report_codex_import_progress,
};

impl ProviderCaptureAdapter for CodexSessionJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Codex
    }

    fn source_format(&self) -> &str {
        "codex_session_jsonl"
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut header = None;
        let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
        let raw_source_path = context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());

        let mut line_number = 0usize;
        let mut line = Vec::new();
        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if !should_parse_codex_session_line(&line, context.event_mode) {
                continue;
            }
            if should_skip_codex_tool_output_line(&line, context.tool_output_mode) {
                result.summary.skipped += 1;
                result.summary.skipped_events += 1;
                continue;
            }

            let value: Value = match serde_json::from_slice(&line) {
                Ok(value) => value,
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            let entry_type = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if entry_type == "session_meta" {
                match codex_session_header(value) {
                    Ok(parsed) => {
                        let capture = codex_session_capture(
                            &parsed,
                            None,
                            line_number,
                            parsed.timestamp,
                            context,
                        );
                        call_contexts.clear();
                        header = Some(parsed);
                        result.captures.push((line_number, capture));
                    }
                    Err(err) => {
                        result.summary.failed += 1;
                        result.summary.failures.push(ProviderImportFailure {
                            line: line_number,
                            error: err.to_string(),
                        });
                    }
                }
                continue;
            }

            let Some(header) = header.as_ref() else {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: "codex session entry appeared before session_meta".to_owned(),
                });
                continue;
            };
            let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
                Ok(occurred_at) => occurred_at,
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            let mut line_capture = codex_session_line_capture(
                header,
                &value,
                &mut call_contexts,
                CodexSessionLineContext {
                    line_number,
                    occurred_at,
                    tool_output_mode: context.tool_output_mode,
                    event_mode: context.event_mode,
                    raw_source_path: raw_source_path.as_deref(),
                    source_root: context.source_root_display().as_deref(),
                },
            );
            if let Some(event) = line_capture.event.take() {
                if !context.include_notices && event.event_type == EventType::Notice {
                    result.summary.skipped += 1;
                    result.summary.skipped_events += 1;
                } else {
                    result.captures.push((
                        line_number,
                        codex_session_capture(
                            header,
                            Some(event),
                            line_number,
                            occurred_at,
                            context,
                        ),
                    ));
                }
            }
            result.files_touched.append(&mut line_capture.files_touched);
        }

        Ok(result)
    }
}
pub(crate) fn should_parse_codex_session_line(
    line: &[u8],
    event_mode: CodexEventImportMode,
) -> bool {
    if contains_bytes(line, br#""type":"session_meta""#)
        || contains_bytes(line, br#""type":"compacted""#)
    {
        return true;
    }

    if event_mode == CodexEventImportMode::Rich && contains_bytes(line, br#""type":"event_msg""#) {
        return true;
    }

    if !contains_bytes(line, br#""type":"response_item""#) {
        return false;
    }

    if contains_bytes(line, br#""type":"message""#)
        && (contains_bytes(line, br#""role":"user""#)
            || contains_bytes(line, br#""role":"assistant""#))
    {
        return true;
    }

    if codex_session_line_may_touch_file(line) {
        return true;
    }

    event_mode == CodexEventImportMode::Rich
        && (contains_bytes(line, br#""type":"function_call""#)
            || contains_bytes(line, br#""type":"custom_tool_call""#)
            || contains_bytes(line, br#""type":"web_search_call""#)
            || contains_bytes(line, br#""type":"tool_search_call""#)
            || contains_bytes(line, br#""type":"function_call_output""#)
            || contains_bytes(line, br#""type":"custom_tool_call_output""#)
            || contains_bytes(line, br#""type":"tool_search_output""#)
            || contains_bytes(line, br#""type":"reasoning""#))
}
pub(crate) fn codex_session_line_may_touch_file(line: &[u8]) -> bool {
    contains_bytes(line, br#""type":"response_item""#)
        && (contains_bytes(line, b"apply_patch")
            || contains_bytes(line, b"*** Begin Patch")
            || contains_bytes(line, b"write_file")
            || contains_bytes(line, b"edit_file")
            || contains_bytes(line, b"str_replace")
            || contains_bytes(line, b"file_path")
            || contains_bytes(line, b"TargetFile"))
}
pub(crate) fn is_codex_tool_output_line(line: &[u8]) -> bool {
    contains_bytes(line, br#""type":"function_call_output""#)
        || contains_bytes(line, br#""type":"custom_tool_call_output""#)
        || contains_bytes(line, br#""type":"tool_search_output""#)
}
pub(crate) fn should_skip_codex_tool_output_line(line: &[u8], mode: CodexToolOutputMode) -> bool {
    if !is_codex_tool_output_line(line) {
        return false;
    }
    match mode {
        CodexToolOutputMode::Full | CodexToolOutputMode::Metadata => false,
        CodexToolOutputMode::Skip => true,
        CodexToolOutputMode::Failures => !codex_tool_output_line_looks_important(line),
    }
}
pub(crate) fn codex_tool_output_line_looks_important(line: &[u8]) -> bool {
    contains_bytes(line, br#""timed_out":true"#)
        || contains_bytes(line, b"timed_out=true")
        || contains_bytes(line, b"timed out")
        || codex_tool_output_line_has_nonzero_exit_code(line)
}
pub(crate) fn codex_tool_output_line_has_nonzero_exit_code(line: &[u8]) -> bool {
    let marker = b"Process exited with code ";
    let mut offset = 0usize;
    while let Some(index) = find_bytes(&line[offset..], marker) {
        let code_start = offset + index + marker.len();
        let mut code_end = code_start;
        if line.get(code_end) == Some(&b'-') {
            code_end += 1;
        }
        while line.get(code_end).is_some_and(|byte| byte.is_ascii_digit()) {
            code_end += 1;
        }
        if let Ok(text) = std::str::from_utf8(&line[code_start..code_end]) {
            if text.parse::<i32>().is_ok_and(|code| code != 0) {
                return true;
            }
        }
        offset = code_end.max(offset + index + marker.len());
        if offset >= line.len() {
            break;
        }
    }
    false
}
pub(crate) fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}
pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
pub fn import_codex_session_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    if options.fast_event_inserts {
        return import_codex_session_paths_fast(vec![path.to_path_buf()], store, options, 0);
    }
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = CodexSessionJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
            tool_output_mode: options.tool_output_mode,
            event_mode: options.event_mode,
            include_notices: options.include_notices,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: false,
            wrap_transaction: true,
            fast_event_inserts: options.fast_event_inserts,
        },
    )
}
pub fn import_codex_session_jsonl_tail(
    path: impl AsRef<Path>,
    start_offset: u64,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    if start_offset == 0 {
        return import_codex_session_jsonl(path, store, options);
    }
    ensure_regular_provider_transcript_file(path)?;
    let total_bytes = fs::metadata(path)?.len();
    if start_offset >= total_bytes {
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = ProviderImportSummary::default();
    let mut caches = ProviderImportCaches::default();
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: options.imported_at,
        tool_output_mode: options.tool_output_mode,
        event_mode: options.event_mode,
        include_notices: options.include_notices,
    };
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        allow_partial_failures: options.allow_partial_failures,
        persist_cursors: false,
        wrap_transaction: false,
        fast_event_inserts: true,
    };
    let raw_source_path = context
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());

    report_codex_import_progress(
        &options,
        1,
        total_bytes - start_offset,
        0,
        0,
        &summary,
        false,
    );

    let mut began_transaction = false;
    let import = (|| -> Result<ProviderImportSummary> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut line_number = 0usize;
        let mut position = 0u64;

        if !read_provider_jsonl_line(&mut reader, &mut line)? {
            return Ok(summary);
        }
        line_number += 1;
        let read = line.len();
        position = position.saturating_add(read as u64);
        let header_value: Value = serde_json::from_slice(&line)?;
        let header = codex_session_header(header_value)?;

        while position < start_offset {
            if !read_provider_jsonl_line(&mut reader, &mut line)? {
                return Ok(summary);
            }
            line_number += 1;
            let read = line.len();
            position = position.saturating_add(read as u64);
        }

        store.begin_immediate_batch()?;
        began_transaction = true;
        let header_capture =
            codex_session_capture(&header, None, line_number, header.timestamp, &context);
        summary.merge(import_provider_capture_line(
            store,
            &header_capture,
            &import_options,
            line_number,
            &mut caches,
        )?);

        let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
        let mut completed_bytes = 0u64;
        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            let read = line.len();
            completed_bytes = completed_bytes.saturating_add(read as u64);
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if !should_parse_codex_session_line(&line, options.event_mode) {
                continue;
            }
            if should_skip_codex_tool_output_line(&line, options.tool_output_mode) {
                summary.skipped += 1;
                summary.skipped_events += 1;
                continue;
            }

            let value: Value = match serde_json::from_slice(&line) {
                Ok(value) => value,
                Err(err) => {
                    summary.failed += 1;
                    summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    if !options.allow_partial_failures {
                        return Ok(summary);
                    }
                    continue;
                }
            };
            if value
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|entry_type| entry_type == "session_meta")
            {
                continue;
            }
            let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
                Ok(occurred_at) => occurred_at,
                Err(err) => {
                    summary.failed += 1;
                    summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    if !options.allow_partial_failures {
                        return Ok(summary);
                    }
                    continue;
                }
            };
            let mut line_capture = codex_session_line_capture(
                &header,
                &value,
                &mut call_contexts,
                CodexSessionLineContext {
                    line_number,
                    occurred_at,
                    tool_output_mode: options.tool_output_mode,
                    event_mode: options.event_mode,
                    raw_source_path: raw_source_path.as_deref(),
                    source_root: context.source_root_display().as_deref(),
                },
            );
            if let Some(event) = line_capture.event.take() {
                if !options.include_notices && event.event_type == EventType::Notice {
                    summary.skipped += 1;
                    summary.skipped_events += 1;
                } else {
                    let source_root = context.source_root_display();
                    summary.merge(import_codex_provider_event_fast(
                        store,
                        &header,
                        &event,
                        options.history_record_id,
                        line_number,
                        context.imported_at,
                        raw_source_path.as_deref(),
                        source_root.as_deref(),
                    )?);
                }
            }
            for (_, file) in line_capture.files_touched {
                import_provider_file_touched_line(store, &file, &import_options)?;
            }
            report_codex_import_progress(
                &options,
                1,
                total_bytes - start_offset,
                0,
                completed_bytes,
                &summary,
                false,
            );
        }

        resolve_pending_provider_edges(store, &mut summary, &mut caches)?;
        Ok(summary)
    })();

    match import {
        Ok(summary) => {
            if summary.failed > 0 && !options.allow_partial_failures {
                if began_transaction {
                    let _ = store.rollback_batch();
                }
                report_codex_import_progress(
                    &options,
                    1,
                    total_bytes - start_offset,
                    1,
                    total_bytes - start_offset,
                    &summary,
                    true,
                );
                return Ok(summary);
            }
            if began_transaction {
                store.commit_batch()?;
            }
            report_codex_import_progress(
                &options,
                1,
                total_bytes - start_offset,
                1,
                total_bytes - start_offset,
                &summary,
                true,
            );
            Ok(summary)
        }
        Err(err) => {
            if began_transaction {
                let _ = store.rollback_batch();
            }
            Err(err)
        }
    }
}
pub fn import_codex_session_paths(
    paths: Vec<PathBuf>,
    store: &mut Store,
    mut options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    for path in &paths {
        ensure_regular_provider_transcript_file(path)?;
    }
    if options.source_path.is_none() {
        options.source_path = codex_common_source_root(&paths);
    }
    if options.fast_event_inserts && paths.len() <= 1 {
        return import_codex_session_paths_fast(paths, store, options, 0);
    }

    import_codex_session_paths_parallel_normalized(paths, store, options, 0)
}
pub fn import_codex_session_tree(
    root: impl AsRef<Path>,
    store: &mut Store,
    mut options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let root = root.as_ref();
    if options.source_path.is_none() {
        options.source_path = Some(root.to_path_buf());
    }
    let mut paths = Vec::new();
    collect_jsonl_paths(root, &mut paths)?;
    let skipped_by_bounds = apply_codex_session_import_bounds(
        &mut paths,
        options.max_session_files,
        options.max_total_bytes,
    )?;
    if options.fast_event_inserts && paths.len() <= 1 {
        return import_codex_session_paths_fast(paths, store, options, skipped_by_bounds);
    }

    import_codex_session_paths_parallel_normalized(paths, store, options, skipped_by_bounds)
}

fn codex_common_source_root(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut parents = paths.iter().filter_map(|path| path.parent());
    let mut root = parents.next()?.to_path_buf();
    for parent in parents {
        while !parent.starts_with(&root) {
            if !root.pop() {
                return None;
            }
        }
    }
    Some(root)
}
pub(crate) fn import_codex_session_paths_parallel_normalized(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    merged.skipped_sessions += skipped_by_bounds;
    merged.skipped += skipped_by_bounds;
    let mut in_transaction = false;
    if !paths.is_empty() {
        store.begin_immediate_batch()?;
        in_transaction = true;
    }
    let total_files = paths.len();
    let total_bytes = codex_session_paths_total_bytes(&paths);
    let mut completed_files = 0usize;
    let mut completed_bytes = 0u64;
    report_codex_import_progress(
        &options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &merged,
        false,
    );

    let parallelism = import_parallelism(paths.len());
    let chunk_size = parallelism.saturating_mul(8).max(16);
    for chunk in paths.chunks(chunk_size) {
        let normalized = match normalize_codex_session_paths_parallel(chunk, &options, parallelism)
        {
            Ok(normalized) => normalized,
            Err(err) => {
                if in_transaction {
                    let _ = store.rollback_batch();
                }
                return Err(err);
            }
        };
        let mut chunk_summary = ProviderImportSummary::default();
        let mut chunk_captures = Vec::new();
        let mut chunk_files_touched = Vec::new();
        let mut chunk_bytes = 0u64;
        for (_, path, normalization) in normalized {
            chunk_bytes = chunk_bytes.saturating_add(
                fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            );
            chunk_summary.merge(normalization.summary);
            chunk_captures.extend(normalization.captures);
            chunk_files_touched.extend(normalization.files_touched);
        }
        let summary = match import_provider_capture_lines(
            store,
            NormalizedProviderImportOptions {
                history_record_id: options.history_record_id,
                allow_partial_failures: options.allow_partial_failures,
                persist_cursors: false,
                wrap_transaction: false,
                fast_event_inserts: options.fast_event_inserts,
            },
            chunk_summary,
            chunk_captures,
            chunk_files_touched,
        ) {
            Ok(summary) => summary,
            Err(err) => {
                if in_transaction {
                    let _ = store.rollback_batch();
                }
                return Err(err);
            }
        };
        merged.merge(summary);
        if merged.failed > 0 && !options.allow_partial_failures {
            if in_transaction {
                let _ = store.rollback_batch();
            }
            report_codex_import_progress(
                &options,
                total_files,
                total_bytes,
                completed_files,
                completed_bytes,
                &merged,
                true,
            );
            return Ok(merged);
        }
        completed_files += chunk.len();
        completed_bytes = completed_bytes.saturating_add(chunk_bytes);
        report_codex_import_progress(
            &options,
            total_files,
            total_bytes,
            completed_files,
            completed_bytes,
            &merged,
            false,
        );
    }
    if in_transaction {
        store.commit_batch()?;
    }
    store.checkpoint_wal_passive_if_larger_than(CODEX_FAST_IMPORT_PASSIVE_CHECKPOINT_MIN_BYTES)?;
    report_codex_import_progress(
        &options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &merged,
        true,
    );
    Ok(merged)
}
pub(crate) fn normalize_codex_session_paths_parallel(
    paths: &[PathBuf],
    options: &CodexSessionImportOptions,
    parallelism: usize,
) -> Result<Vec<(usize, PathBuf, ProviderNormalizationResult)>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if parallelism <= 1 || paths.len() == 1 {
        let mut normalized = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            normalized.push((
                index,
                path.clone(),
                normalize_codex_session_path(path, options)?,
            ));
        }
        return Ok(normalized);
    }

    let chunk_size = paths.len().div_ceil(parallelism).max(1);
    let mut batches = thread::scope(|scope| {
        let mut handles = Vec::new();
        for (chunk_index, chunk) in paths.chunks(chunk_size).enumerate() {
            let chunk = chunk.to_vec();
            handles.push(scope.spawn(move || {
                let mut normalized = Vec::with_capacity(chunk.len());
                let base_index = chunk_index * chunk_size;
                for (offset, path) in chunk.iter().enumerate() {
                    normalized.push((
                        base_index + offset,
                        path.clone(),
                        normalize_codex_session_path(path, options)?,
                    ));
                }
                Result::<Vec<_>>::Ok(normalized)
            }));
        }
        let mut batches = Vec::with_capacity(handles.len());
        for handle in handles {
            batches.push(handle.join().map_err(|_| {
                CaptureError::InvalidPayload("Codex import worker panicked".into())
            })??);
        }
        Result::<Vec<_>>::Ok(batches)
    })?;
    let total = batches.iter().map(Vec::len).sum();
    let mut normalized = Vec::with_capacity(total);
    for batch in batches.drain(..) {
        normalized.extend(batch);
    }
    normalized.sort_by_key(|(index, _, _)| *index);
    Ok(normalized)
}
pub(crate) fn normalize_codex_session_path(
    path: &Path,
    options: &CodexSessionImportOptions,
) -> Result<ProviderNormalizationResult> {
    CodexSessionJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id.clone(),
            source_path: Some(path.to_path_buf()),
            source_root: options.source_path.clone(),
            imported_at: options.imported_at,
            tool_output_mode: options.tool_output_mode,
            event_mode: options.event_mode,
            include_notices: options.include_notices,
        },
    )
}
pub(crate) fn import_parallelism(path_count: usize) -> usize {
    if path_count <= 1 {
        return 1;
    }
    thread::available_parallelism()
        .ok()
        .map(usize::from)
        .unwrap_or(1)
        .min(path_count)
        .min(8)
}
pub(crate) fn apply_codex_session_import_bounds(
    paths: &mut Vec<PathBuf>,
    max_files: Option<usize>,
    max_total_bytes: Option<u64>,
) -> Result<usize> {
    paths.sort();
    if max_files.is_none() && max_total_bytes.is_none() {
        return Ok(0);
    }

    let original_len = paths.len();
    let mut selected = Vec::new();
    let mut total_bytes = 0u64;
    for path in paths.iter().rev() {
        if max_files.is_some_and(|limit| selected.len() >= limit) {
            continue;
        }
        let len = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if max_total_bytes.is_some_and(|limit| total_bytes.saturating_add(len) > limit) {
            continue;
        }
        total_bytes = total_bytes.saturating_add(len);
        selected.push(path.clone());
    }
    selected.sort();
    let skipped = original_len.saturating_sub(selected.len());
    *paths = selected;
    Ok(skipped)
}
