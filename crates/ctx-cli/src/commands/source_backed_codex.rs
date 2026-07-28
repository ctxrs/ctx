use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::{
    ingest_codex_source_backed_v0, CodexHydratedRecordV0, CodexLocatorResolverV0,
    CodexSourceBackedIngestReceiptV0,
};
use ctx_history_core::CaptureProvider;
use ctx_history_index::{EventRecord, SessionRecord, VerifiedIndex};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::{
        count_bucket, duration_bucket, text_length_bucket, SearchTelemetry, ShowTelemetry,
    },
    complete_content::{ContentPolicy, CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES},
    output::{compact_json, print_json, JsonOutputFormat, OutputFormat},
    provider_sources::discovered_sources_for_provider_report,
    transcript::{write_output, TranscriptMode},
    RefreshArg, SearchArgs, SearchBackendArg, ShowArgs, ShowTarget,
};

const INDEX_DIRECTORY: &str = "source-backed-lexical-v0";
const CODEX_SESSION_SOURCE_FORMAT: &str = "codex_session_jsonl_tree";

pub(crate) fn should_run_search(args: &SearchArgs) -> bool {
    args.provider
        .is_some_and(|provider| provider.capture_provider() == CaptureProvider::Codex)
        && matches!(args.backend, Some(SearchBackendArg::Lexical))
        && matches!(args.refresh, RefreshArg::Wait | RefreshArg::Off)
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    telemetry: &mut SearchTelemetry,
) -> Result<()> {
    reject_unsupported_search_options(&args)?;
    let query = args.query.as_deref().unwrap_or_default().trim();
    if query.is_empty() {
        return Err(anyhow!(
            "source-backed Codex V0 requires a non-empty natural-text query"
        ));
    }

    let index_root = index_root(&data_root);
    let mut refresh_receipts = Vec::new();
    if args.refresh == RefreshArg::Wait {
        fs::create_dir_all(&data_root)
            .with_context(|| format!("create ctx data root {}", data_root.display()))?;
        for root in codex_session_roots()? {
            refresh_receipts.push(
                ingest_codex_source_backed_v0(&root, &index_root).with_context(|| {
                    format!("refresh source-backed Codex tree {}", root.display())
                })?,
            );
        }
    } else if !index_root.join("meta.json").is_file() {
        return Err(anyhow!(
            "the source-backed Codex index does not exist; retry with `--refresh wait`"
        ));
    }

    let index = VerifiedIndex::open(&index_root).with_context(|| {
        format!(
            "open verified source-backed lexical index {}",
            index_root.display()
        )
    })?;
    let query_started = Instant::now();
    let candidates = index.search_event_candidates(query, args.limit)?;
    let query_duration = query_started.elapsed();

    telemetry.query_duration = Some(duration_bucket(query_duration));
    telemetry.query_length = Some(text_length_bucket(query.chars().count()));
    telemetry.query_term_count = Some(count_bucket(
        query
            .split_whitespace()
            .filter(|term| !term.is_empty())
            .count() as u64,
    ));
    telemetry.result_count = Some(count_bucket(candidates.len() as u64));
    telemetry.zero_result = Some(candidates.is_empty());
    telemetry.backend_requested = Some(SearchBackendArg::Lexical);

    if args.format == JsonOutputFormat::Json {
        let phase_attribution = phase_attribution(&refresh_receipts, query_duration);
        let results = candidates
            .iter()
            .map(|candidate| {
                compact_json(json!({
                    "ctx_event_id": candidate.event.event_id.to_string(),
                    "ctx_session_id": candidate.event.session_id.to_string(),
                    "provider": candidate.event.provider,
                    "provider_session_id": candidate.event.provider_session_id,
                    "event_sequence": candidate.event.event_sequence,
                    "occurred_at_unix_ms": candidate.event.occurred_at_unix_ms,
                    "event_type": candidate.event.event_type,
                    "role": candidate.event.role,
                    "preview": candidate.event.preview,
                    "workspace": candidate.event.workspace,
                    "cwd": candidate.event.cwd,
                    "touched_files": candidate.event.touched_files,
                    "score": candidate.score,
                }))
            })
            .collect::<Vec<_>>();
        print_json(compact_json(json!({
            "schema_version": 1,
            "payload_type": "source_backed_codex_search_v0",
            "query": query,
            "freshness": {
                "mode": args.refresh.as_str(),
                "refreshed_roots": refresh_receipts.len(),
            },
            "retrieval": {
                "backend": "tantivy_lexical",
                "generation_id": index.generation_id(),
                "indexed_documents": index.document_count(),
                "phase_attribution": phase_attribution,
            },
            "phase_attribution": phase_attribution,
            "results": results,
        })))?;
    } else {
        if candidates.is_empty() {
            println!("no results for {query:?}");
        }
        for (position, candidate) in candidates.iter().enumerate() {
            println!(
                "{}. {}  {}",
                position + 1,
                candidate.event.event_id,
                candidate.event.preview
            );
            println!(
                "   ctx show event {} --content complete",
                candidate.event.event_id
            );
        }
    }
    Ok(())
}

pub(crate) fn try_run_show(
    args: &ShowArgs,
    data_root: &Path,
    telemetry: &mut ShowTelemetry,
) -> Result<bool> {
    let index_root = index_root(data_root);
    if !index_root.join("meta.json").is_file() {
        return Ok(false);
    }
    let index = VerifiedIndex::open(&index_root).with_context(|| {
        format!(
            "open verified source-backed lexical index {}",
            index_root.display()
        )
    })?;

    match &args.target {
        ShowTarget::Event(args) => {
            let Some(selected) = resolve_event(&index, &args.id)? else {
                return Ok(false);
            };
            let mut events = index.events_for_session(selected.session_id.as_uuid())?;
            let selected_position = events
                .iter()
                .position(|event| event.event_id == selected.event_id)
                .ok_or_else(|| {
                    anyhow!("selected source-backed event is absent from its session")
                })?;
            let (before, after) = args
                .window
                .map(|window| (window, window))
                .unwrap_or((args.before, args.after));
            let start = selected_position.saturating_sub(before);
            let end = selected_position
                .saturating_add(after)
                .saturating_add(1)
                .min(events.len());
            events = events.drain(start..end).collect();
            telemetry.events_returned = Some(count_bucket(events.len() as u64));
            render_event_window(&selected, &events, args.content, args.format)?;
            Ok(true)
        }
        ShowTarget::Session(args) => {
            if args
                .provider
                .is_some_and(|provider| provider.capture_provider() != CaptureProvider::Codex)
                || args.provider_session.is_some()
            {
                return Ok(false);
            }
            let Some(id) = args.id.as_deref() else {
                return Ok(false);
            };
            let Some(session) = resolve_session(&index, id)? else {
                return Ok(false);
            };
            let events = index.events_for_session(session.session_id.as_uuid())?;
            let selected = select_session_events(&events, args.mode);
            telemetry.events_returned = Some(count_bucket(selected.len() as u64));
            render_session(
                &session,
                &selected,
                args.mode,
                args.content,
                args.format,
                args.out.clone(),
            )?;
            Ok(true)
        }
    }
}

fn reject_unsupported_search_options(args: &SearchArgs) -> Result<()> {
    if !args.term.is_empty()
        || args.history_source.is_some()
        || args.provider_key.is_some()
        || args.source_id.is_some()
        || args.source_format.is_some()
        || args.workspace.is_some()
        || args.since.is_some()
        || args.primary_only
        || args.include_subagents
        || args.event_type.is_some()
        || args.file.is_some()
        || args.session.is_some()
        || args.include_current_session
    {
        return Err(anyhow!(
            "the source-backed Codex V0 route currently supports a natural-text query, --limit, --events, --format, and --refresh wait|off only"
        ));
    }
    Ok(())
}

fn codex_session_roots() -> Result<Vec<PathBuf>> {
    let report = discovered_sources_for_provider_report(CaptureProvider::Codex);
    let mut roots = report
        .sources
        .into_iter()
        .filter(|source| {
            source.exists
                && source.source_format == CODEX_SESSION_SOURCE_FORMAT
                && source.status == ctx_history_capture::ProviderSourceStatus::Available
        })
        .map(|source| source.path)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        let detail = report
            .issues
            .first()
            .map(|issue| issue.reason)
            .unwrap_or("no ordinary Codex rollout/session JSONL tree was discovered");
        return Err(anyhow!("cannot discover Codex session sources: {detail}"));
    }
    Ok(roots)
}

fn index_root(data_root: &Path) -> PathBuf {
    data_root.join(INDEX_DIRECTORY)
}

fn phase_attribution(receipts: &[CodexSourceBackedIngestReceiptV0], query: Duration) -> Value {
    let seconds = |select: fn(&CodexSourceBackedIngestReceiptV0) -> Duration| {
        receipts.iter().map(select).sum::<Duration>().as_secs_f64()
    };
    json!({
        "discovery_seconds": seconds(|receipt| receipt.timings.discovery),
        "writer_open_seconds": seconds(|receipt| receipt.timings.writer_open),
        "scan_and_stage_seconds": seconds(|receipt| receipt.timings.scan_and_stage),
        "certification_seconds": seconds(|receipt| receipt.timings.certification),
        "index_commit_seconds": seconds(|receipt| receipt.timings.commit),
        "refresh_total_seconds": seconds(|receipt| receipt.timings.total),
        "query_seconds": query.as_secs_f64(),
        "catalog_sources": receipts.iter().map(|receipt| receipt.counters.catalog_sources).sum::<u64>(),
        "catalog_source_bytes": receipts.iter().map(|receipt| receipt.counters.catalog_source_bytes).sum::<u64>(),
        "scanner_bytes_read": receipts.iter().map(|receipt| receipt.counters.scanner_bytes_read).sum::<u64>(),
        "structural_json_parses": receipts.iter().map(|receipt| receipt.counters.structural_json_parses).sum::<u64>(),
        "typed_json_parses": receipts.iter().map(|receipt| receipt.counters.typed_json_parses).sum::<u64>(),
        "staged_documents": receipts.iter().map(|receipt| receipt.counters.staged_documents).sum::<u64>(),
        "legacy_json_serializations": receipts.iter().map(|receipt| {
            receipt.counters.scanner_legacy_body_json_serializations
                .saturating_add(receipt.counters.scanner_legacy_row_json_serializations)
        }).sum::<u64>(),
        "legacy_json_serialized_bytes": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_json_serialized_bytes).sum::<u64>(),
        "legacy_normalized_payload_hashes": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_normalized_payload_hashes).sum::<u64>(),
        "legacy_file_touch_rows": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_file_touch_rows).sum::<u64>(),
        "legacy_complete_content_locators": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_complete_content_locators).sum::<u64>(),
        "legacy_duplicate_preview_allocations": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_duplicate_preview_allocations).sum::<u64>(),
        "legacy_page_owner_json_serializations": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_page_owner_json_serializations).sum::<u64>(),
        "legacy_page_identity_owner_json_serializations": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_page_identity_owner_json_serializations).sum::<u64>(),
        "legacy_page_identity_row_json_serializations": receipts.iter().map(|receipt| receipt.counters.scanner_legacy_page_identity_row_json_serializations).sum::<u64>(),
    })
}

fn resolve_event(index: &VerifiedIndex, id: &str) -> Result<Option<EventRecord>> {
    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(index.event_by_id(uuid)?);
    }
    let matches = match index.events_by_id_prefix(id) {
        Ok(matches) => matches,
        Err(ctx_history_index::IndexError::InvalidIdPrefix) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    match matches.as_slice() {
        [] => Ok(None),
        [event] => Ok(Some(event.clone())),
        _ => Err(anyhow!("source-backed event ID prefix {id:?} is ambiguous")),
    }
}

fn resolve_session(index: &VerifiedIndex, id: &str) -> Result<Option<SessionRecord>> {
    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(index.session_by_id(uuid)?);
    }
    let matches = match index.sessions_by_id_prefix(id) {
        Ok(matches) => matches,
        Err(ctx_history_index::IndexError::InvalidIdPrefix) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    match matches.as_slice() {
        [] => Ok(None),
        [session] => Ok(Some(session.clone())),
        _ => Err(anyhow!(
            "source-backed session ID prefix {id:?} is ambiguous"
        )),
    }
}

fn render_event_window(
    selected: &EventRecord,
    events: &[EventRecord],
    content: ContentPolicy,
    format: OutputFormat,
) -> Result<()> {
    let resolver = resolver_for(content)?;
    let values = render_event_values(events.iter(), content, resolver.as_ref())?;
    let body = match format {
        OutputFormat::Json => serde_json::to_string_pretty(&compact_json(json!({
            "schema_version": 1,
            "payload_type": "source_backed_codex_event_window_v0",
            "selected_ctx_event_id": selected.event_id.to_string(),
            "content_policy": content.as_str(),
            "events": values,
        })))?,
        OutputFormat::Jsonl => values
            .iter()
            .map(serde_json::to_string)
            .collect::<serde_json::Result<Vec<_>>>()?
            .join("\n"),
        OutputFormat::Text | OutputFormat::Markdown => render_text_events(&values, format),
    };
    enforce_show_bound(&body)?;
    write_output(body, None)
}

fn render_session(
    session: &SessionRecord,
    events: &[&EventRecord],
    mode: TranscriptMode,
    content: ContentPolicy,
    format: OutputFormat,
    out: Option<PathBuf>,
) -> Result<()> {
    let resolver = resolver_for(content)?;
    let values = render_event_values(events.iter().copied(), content, resolver.as_ref())?;
    let body = match format {
        OutputFormat::Json => serde_json::to_string_pretty(&compact_json(json!({
            "schema_version": 1,
            "payload_type": "source_backed_codex_session_v0",
            "content_policy": content.as_str(),
            "mode": mode.as_str(),
            "session": {
                "ctx_session_id": session.session_id.to_string(),
                "provider": session.provider,
                "provider_session_id": session.provider_session_id,
                "workspace": session.workspace,
                "cwd": session.cwd,
            },
            "events": values,
        })))?,
        OutputFormat::Jsonl => values
            .iter()
            .map(serde_json::to_string)
            .collect::<serde_json::Result<Vec<_>>>()?
            .join("\n"),
        OutputFormat::Text | OutputFormat::Markdown => render_text_events(&values, format),
    };
    enforce_show_bound(&body)?;
    write_output(body, out)
}

fn resolver_for(content: ContentPolicy) -> Result<Option<CodexLocatorResolverV0>> {
    match content {
        ContentPolicy::Indexed => Ok(None),
        ContentPolicy::Complete => Ok(Some(CodexLocatorResolverV0::discover(
            codex_session_roots()?,
        )?)),
    }
}

fn render_event_values<'a>(
    events: impl IntoIterator<Item = &'a EventRecord>,
    content: ContentPolicy,
    resolver: Option<&CodexLocatorResolverV0>,
) -> Result<Vec<Value>> {
    events
        .into_iter()
        .map(|event| {
            let hydrated = match (content, resolver) {
                (ContentPolicy::Complete, Some(resolver)) => {
                    Some(resolver.hydrate(&event.locator).with_context(|| {
                        format!("hydrate source-backed Codex event {}", event.event_id)
                    })?)
                }
                (ContentPolicy::Complete, None) => {
                    return Err(anyhow!("complete content resolver is unavailable"))
                }
                (ContentPolicy::Indexed, _) => None,
            };
            Ok(event_value(event, content, hydrated.as_ref()))
        })
        .collect()
}

fn event_value(
    event: &EventRecord,
    content: ContentPolicy,
    hydrated: Option<&CodexHydratedRecordV0>,
) -> Value {
    let exact_text = hydrated
        .and_then(|record| record.decoded_display_text.clone())
        .unwrap_or_else(|| event.preview.clone());
    let source_record = hydrated.and_then(|record| {
        serde_json::from_slice::<Value>(&record.provider_bytes)
            .ok()
            .or_else(|| {
                Some(Value::String(
                    String::from_utf8_lossy(&record.provider_bytes).into_owned(),
                ))
            })
    });
    compact_json(json!({
        "ctx_event_id": event.event_id.to_string(),
        "ctx_session_id": event.session_id.to_string(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "event_sequence": event.event_sequence,
        "occurred_at_unix_ms": event.occurred_at_unix_ms,
        "event_type": event.event_type,
        "role": event.role,
        "content": exact_text,
        "content_origin": match content {
            ContentPolicy::Indexed => "ctx_index_preview",
            ContentPolicy::Complete => "provider_source",
        },
        "source_record": source_record,
    }))
}

fn render_text_events(events: &[Value], format: OutputFormat) -> String {
    let mut output = String::new();
    for event in events {
        let event_id = event
            .get("ctx_event_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let role = event
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                event
                    .get("event_type")
                    .and_then(Value::as_str)
                    .unwrap_or("event")
            });
        let content = event
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if format == OutputFormat::Markdown {
            output.push_str(&format!("### {role} · `{event_id}`\n\n{content}\n\n"));
        } else {
            output.push_str(&format!("[{role}] {event_id}\n{content}\n\n"));
        }
    }
    output
}

fn select_session_events(events: &[EventRecord], mode: TranscriptMode) -> Vec<&EventRecord> {
    match mode {
        TranscriptMode::Log => events.iter().collect(),
        TranscriptMode::Full => events
            .iter()
            .filter(|event| {
                event.event_type == "message"
                    && matches!(event.role.as_deref(), Some("user" | "assistant" | "system"))
            })
            .collect(),
        TranscriptMode::Lite => {
            let mut selected = Vec::new();
            let mut pending_assistant = None;
            for event in events {
                if event.event_type != "message" {
                    continue;
                }
                match event.role.as_deref() {
                    Some("user") => {
                        if let Some(assistant) = pending_assistant.take() {
                            selected.push(assistant);
                        }
                        selected.push(event);
                    }
                    Some("assistant") => pending_assistant = Some(event),
                    _ => {}
                }
            }
            if let Some(assistant) = pending_assistant {
                selected.push(assistant);
            }
            selected
        }
    }
}

fn enforce_show_bound(body: &str) -> Result<()> {
    if body.len() > CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES {
        return Err(anyhow!(
            "source-backed show output is {} bytes, above the {}-byte CLI limit",
            body.len(),
            CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES
        ));
    }
    Ok(())
}
