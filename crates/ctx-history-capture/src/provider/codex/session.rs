use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
    thread,
};

use ctx_history_core::{CaptureProvider, EventRole, EventType, ProviderEventEnvelope};
use ctx_history_store::Store;
use serde_json::Value;

use crate::CodexSessionJsonlAdapter;

use crate::common::io::{
    collect_jsonl_paths, ensure_regular_provider_transcript_file,
    read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead,
};
use crate::provider::importer::{
    import_normalized_provider_captures, import_provider_capture_line,
    import_provider_capture_lines, import_provider_file_touched_line,
    resolve_pending_provider_edges_batched, validate_provider_event_for_import,
    ProviderImportCaches, ProviderImportTransaction,
};
use crate::provider::native::provider_output_event_is_failure;
use crate::{
    CaptureError, CodexSessionImportOptions, NormalizedProviderImportOptions,
    ProviderAdapterContext, ProviderCaptureAdapter, ProviderImportFailure, ProviderImportSummary,
    ProviderNormalizationResult, Result,
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
        let mut has_real_message_content = false;
        let mut skipped_oversized_events = 0usize;
        let raw_source_path = context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());

        let mut line_number = 0usize;
        let mut line = Vec::new();
        loop {
            match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
                ProviderJsonlLineRead::Eof => break,
                ProviderJsonlLineRead::Line { .. } => {
                    line_number += 1;
                }
                ProviderJsonlLineRead::Oversized { .. } => {
                    line_number += 1;
                    result.summary.skipped += 1;
                    if header.is_none() {
                        result.summary.skipped_sessions += 1;
                        return Ok(result);
                    }
                    skipped_oversized_events = skipped_oversized_events.saturating_add(1);
                    result.summary.skipped_events += 1;
                    continue;
                }
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if !should_parse_codex_session_line(&line) {
                continue;
            }
            if should_skip_codex_tool_output_line(&line) {
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
                    raw_source_path: raw_source_path.as_deref(),
                    source_root: context.source_root_display().as_deref(),
                },
            );
            if let Some(event) = line_capture.event.take() {
                if codex_event_has_real_conversation_content(&event) {
                    has_real_message_content = true;
                }
                if event.event_type == EventType::Notice {
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

        if !has_real_message_content {
            result.captures.clear();
            result.files_touched.clear();
            if skipped_oversized_events == 0 && result.summary.failed == 0 {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: line_number,
                    error: "codex session JSONL contained no real message content".to_owned(),
                });
            }
        }

        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexSessionConversationScan {
    pub(crate) has_real_conversation: bool,
    pub(crate) has_malformed_header: bool,
    pub(crate) has_malformed_relevant_line: bool,
    pub(crate) oversized_required_header: bool,
    pub(crate) oversized_events: usize,
}

pub(crate) fn codex_event_has_real_conversation_content(event: &ProviderEventEnvelope) -> bool {
    event.event_type == EventType::Message
        && matches!(
            event.role,
            Some(EventRole::User | EventRole::Assistant | EventRole::System)
        )
        && event
            .payload
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
}

pub(crate) fn codex_session_file_conversation_scan(
    path: &Path,
) -> Result<CodexSessionConversationScan> {
    ensure_regular_provider_transcript_file(path)?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut scan = CodexSessionConversationScan::default();
    let mut header_seen = false;
    loop {
        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
            ProviderJsonlLineRead::Eof => break,
            ProviderJsonlLineRead::Line { .. } => {}
            ProviderJsonlLineRead::Oversized { .. } => {
                if header_seen {
                    scan.oversized_events = scan.oversized_events.saturating_add(1);
                    continue;
                }
                scan.oversized_required_header = true;
                return Ok(scan);
            }
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&line) {
            Ok(value) => value,
            Err(_) if should_parse_codex_session_line(&line) => {
                scan.has_malformed_relevant_line = true;
                return Ok(scan);
            }
            Err(_) => continue,
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if codex_session_header(value.clone()).is_ok() {
                header_seen = true;
            } else {
                scan.has_malformed_header = true;
                return Ok(scan);
            }
        }
        let Some(payload) = value
            .get("payload")
            .filter(|_| value.get("type").and_then(Value::as_str) == Some("response_item"))
        else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(role, "user" | "assistant" | "system" | "developer") {
            continue;
        }
        if payload
            .get("content")
            .and_then(crate::provider::codex::events::codex_content_text)
            .is_some_and(|text| !text.trim().is_empty())
        {
            scan.has_real_conversation = true;
            return Ok(scan);
        }
    }
    Ok(scan)
}
pub(crate) fn should_parse_codex_session_line(line: &[u8]) -> bool {
    if contains_bytes(line, br#""type":"session_meta""#)
        || contains_bytes(line, br#""type":"compacted""#)
    {
        return true;
    }

    if contains_bytes(line, br#""type":"event_msg""#) {
        return codex_session_event_msg_may_touch_file(line);
    }

    if !contains_bytes(line, br#""type":"response_item""#) {
        return false;
    }

    if contains_bytes(line, br#""type":"message""#)
        && (contains_bytes(line, br#""role":"user""#)
            || contains_bytes(line, br#""role":"assistant""#)
            || contains_bytes(line, br#""role":"system""#)
            || contains_bytes(line, br#""role":"developer""#))
    {
        return true;
    }

    if codex_session_line_may_touch_file(line) {
        return true;
    }

    contains_bytes(line, br#""type":"function_call""#)
        || contains_bytes(line, br#""type":"custom_tool_call""#)
        || contains_bytes(line, br#""type":"web_search_call""#)
        || contains_bytes(line, br#""type":"tool_search_call""#)
        || contains_bytes(line, br#""type":"function_call_output""#)
        || contains_bytes(line, br#""type":"custom_tool_call_output""#)
        || contains_bytes(line, br#""type":"tool_search_output""#)
        || contains_bytes(line, br#""type":"reasoning""#)
}
pub(crate) fn codex_session_event_msg_may_touch_file(line: &[u8]) -> bool {
    contains_bytes(line, br#""patch_apply_end""#)
        || contains_bytes(line, b"apply_patch")
        || contains_bytes(line, b"*** Begin Patch")
        || contains_bytes(line, b"changes")
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
pub(crate) fn should_skip_codex_tool_output_line(line: &[u8]) -> bool {
    if !is_codex_tool_output_line(line) {
        return false;
    }
    !codex_tool_output_line_looks_important(line)
}
pub(crate) fn codex_tool_output_line_looks_important(line: &[u8]) -> bool {
    contains_bytes(line, br#""timed_out":true"#)
        || contains_bytes(line, b"timed_out=true")
        || contains_bytes(line, b"timed out")
        || codex_tool_output_line_has_nonzero_exit_code(line)
        || serde_json::from_slice::<Value>(line)
            .ok()
            .and_then(|value| value.get("payload").cloned())
            .is_some_and(|payload| provider_output_event_is_failure(&payload))
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
    let normalization = CodexSessionJsonlAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(path.to_path_buf()),
            source_root: options.source_path,
            imported_at: options.imported_at,
        },
    )?;
    if codex_normalization_is_skipped_only(&normalization) {
        return Ok(normalization.summary);
    }

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
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

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let import_result =
        import_codex_session_jsonl_tail_bounded(path, start_offset, total_bytes, store, &options);
    let finish_result = store.finish_event_search_bulk_mode(&bulk_guard);
    match (import_result, finish_result) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(err)) => Err(err.into()),
        (Err(err), Ok(())) => Err(err),
    }
}

fn import_codex_session_jsonl_tail_bounded(
    path: &Path,
    start_offset: u64,
    total_bytes: u64,
    store: &mut Store,
    options: &CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    let mut caches = ProviderImportCaches::default();
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: options.source_path.clone(),
        imported_at: options.imported_at,
    };
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        persist_cursors: false,
        wrap_transaction: false,
        fast_event_inserts: true,
    };
    let raw_source_path = context
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());

    report_codex_import_progress(
        options,
        1,
        total_bytes - start_offset,
        0,
        0,
        &summary,
        false,
    );

    let mut transaction = None;
    let import = (|| -> Result<ProviderImportSummary> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut line_number = 0usize;
        let mut position = 0u64;

        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
            ProviderJsonlLineRead::Eof => return Ok(summary),
            ProviderJsonlLineRead::Line { bytes } => {
                line_number += 1;
                position = position.saturating_add(bytes as u64);
            }
            ProviderJsonlLineRead::Oversized { .. } => {
                summary.skipped += 1;
                summary.skipped_sessions += 1;
                return Ok(summary);
            }
        }
        let header_value: Value = serde_json::from_slice(&line)?;
        let header = codex_session_header(header_value)?;

        while position < start_offset {
            match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
                ProviderJsonlLineRead::Eof => return Ok(summary),
                ProviderJsonlLineRead::Line { bytes } => {
                    line_number += 1;
                    position = position.saturating_add(bytes as u64);
                }
                ProviderJsonlLineRead::Oversized { bytes } => {
                    line_number += 1;
                    position = position.saturating_add(bytes as u64);
                    summary.skipped += 1;
                    summary.skipped_events += 1;
                    continue;
                }
            }
        }

        transaction = Some(ProviderImportTransaction::begin_bounded(store, true)?);
        let transaction = transaction.as_mut().expect("transaction was initialized");
        let mut header_persisted = false;

        let mut call_contexts: BTreeMap<String, CodexToolCallContext> = BTreeMap::new();
        let mut completed_bytes = 0u64;
        loop {
            match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
                ProviderJsonlLineRead::Eof => break,
                ProviderJsonlLineRead::Line { bytes } => {
                    line_number += 1;
                    completed_bytes = completed_bytes.saturating_add(bytes as u64);
                }
                ProviderJsonlLineRead::Oversized { bytes } => {
                    line_number += 1;
                    completed_bytes = completed_bytes.saturating_add(bytes as u64);
                    summary.skipped += 1;
                    summary.skipped_events += 1;
                    report_codex_import_progress(
                        options,
                        1,
                        total_bytes - start_offset,
                        0,
                        completed_bytes,
                        &summary,
                        false,
                    );
                    continue;
                }
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if !should_parse_codex_session_line(&line) {
                continue;
            }
            if should_skip_codex_tool_output_line(&line) {
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
                    raw_source_path: raw_source_path.as_deref(),
                    source_root: context.source_root_display().as_deref(),
                },
            );
            let event = match line_capture.event.take() {
                Some(event) if event.event_type == EventType::Notice => {
                    summary.skipped += 1;
                    summary.skipped_events += 1;
                    None
                }
                Some(event) => {
                    if let Err(err) = validate_provider_event_for_import(&event) {
                        summary.failed += 1;
                        summary.failures.push(ProviderImportFailure {
                            line: line_number,
                            error: err.to_string(),
                        });
                        continue;
                    }
                    Some(event)
                }
                None => None,
            };
            let has_content = event.is_some() || !line_capture.files_touched.is_empty();
            if has_content {
                transaction.prepare_unit(store, line.len())?;
            }
            if !header_persisted && has_content {
                let header_capture =
                    codex_session_capture(&header, None, line_number, header.timestamp, &context);
                summary.merge(import_provider_capture_line(
                    store,
                    &header_capture,
                    &import_options,
                    line_number,
                    &mut caches,
                )?);
                header_persisted = true;
            }
            if let Some(event) = event {
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
            for (_, file) in line_capture.files_touched {
                import_provider_file_touched_line(store, &file, &import_options)?;
                summary.accepted_content_records += 1;
            }
            if has_content {
                transaction.record_unit(store, line.len())?;
            }
            report_codex_import_progress(
                options,
                1,
                total_bytes - start_offset,
                0,
                completed_bytes,
                &summary,
                false,
            );
        }

        resolve_pending_provider_edges_batched(store, &mut summary, &mut caches, transaction)?;
        transaction.commit(store)?;
        Ok(summary)
    })();

    match import {
        Ok(summary) => {
            report_codex_import_progress(
                options,
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
            if let Some(transaction) = transaction.as_mut() {
                transaction.rollback(store);
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
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let import_result = import_codex_session_paths_parallel_normalized_bounded(
        paths,
        store,
        &options,
        skipped_by_bounds,
    );
    let finish_result = store.finish_event_search_bulk_mode(&bulk_guard);
    match (import_result, finish_result) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(err)) => Err(err.into()),
        (Err(err), Ok(())) => Err(err),
    }
}

fn import_codex_session_paths_parallel_normalized_bounded(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: &CodexSessionImportOptions,
    skipped_by_bounds: usize,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    merged.skipped_sessions += skipped_by_bounds;
    merged.skipped += skipped_by_bounds;
    let total_files = paths.len();
    let total_bytes = codex_session_paths_total_bytes(&paths);
    let mut completed_files = 0usize;
    let mut completed_bytes = 0u64;
    report_codex_import_progress(
        options,
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
        let normalized = normalize_codex_session_paths_parallel(chunk, options, parallelism)?;
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
        if chunk_summary.failed == 0
            && chunk_summary.skipped > 0
            && chunk_captures.is_empty()
            && chunk_files_touched.is_empty()
        {
            merged.merge(chunk_summary);
            completed_files += chunk.len();
            completed_bytes = completed_bytes.saturating_add(chunk_bytes);
            report_codex_import_progress(
                options,
                total_files,
                total_bytes,
                completed_files,
                completed_bytes,
                &merged,
                false,
            );
            continue;
        }
        let summary = import_provider_capture_lines(
            store,
            NormalizedProviderImportOptions {
                history_record_id: options.history_record_id,
                persist_cursors: false,
                wrap_transaction: true,
                fast_event_inserts: options.fast_event_inserts,
            },
            chunk_summary,
            chunk_captures,
            chunk_files_touched,
        )?;
        merged.merge(summary);
        completed_files += chunk.len();
        completed_bytes = completed_bytes.saturating_add(chunk_bytes);
        report_codex_import_progress(
            options,
            total_files,
            total_bytes,
            completed_files,
            completed_bytes,
            &merged,
            false,
        );
    }
    report_codex_import_progress(
        options,
        total_files,
        total_bytes,
        completed_files,
        completed_bytes,
        &merged,
        true,
    );
    Ok(merged)
}

fn codex_normalization_is_skipped_only(normalization: &ProviderNormalizationResult) -> bool {
    normalization.summary.failed == 0
        && normalization.summary.skipped > 0
        && normalization.captures.is_empty()
        && normalization.files_touched.is_empty()
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
            batches.push(join_codex_import_worker(handle)?);
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

pub(crate) fn join_codex_import_worker<T>(
    handle: thread::ScopedJoinHandle<'_, Result<T>>,
) -> Result<T> {
    handle
        .join()
        .map_err(|_| CaptureError::WorkerPanicked("Codex import"))?
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
