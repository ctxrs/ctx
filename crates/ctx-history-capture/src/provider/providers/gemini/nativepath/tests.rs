use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::discover_gemini_transcripts;
use super::discovery::{discover_gemini_transcripts_with_limits, DiscoveryBudget};
use super::dto::{
    GeminiCompleteness, GeminiEventBody, GeminiEventIdentity, GeminiPreviousSource,
    GeminiPublicationShape, GeminiRejectionKind, GeminiRetainedEvent, GeminiScanError,
    GeminiScanOutcome, GeminiSourceChange, GeminiTranscriptLayout, GeminiTranscriptSource,
};
use super::parser::{
    gemini_parse_counters, gemini_resume_work_counters, read_gemini_transcript_pages,
    read_gemini_transcript_pages_from_frontier, reset_gemini_parse_counters, GeminiNativeEventIds,
    MAX_GEMINI_FILE_TOUCHES_PER_EVENT, MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
    MAX_GEMINI_NATIVE_PAGE_BYTES, MAX_GEMINI_NATIVE_PAGE_RECORDS,
};
use crate::{CaptureError, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS};

fn fixture_root(temp: &TempDir) -> PathBuf {
    temp.path().join(".gemini")
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("tmp/project/chats/session-root.jsonl")
}

fn header(session_id: &str, kind: &str) -> Value {
    json!({
        "sessionId": session_id,
        "startTime": "2026-01-01T00:00:00.000Z",
        "lastUpdated": "2026-01-01T00:00:00.000Z",
        "kind": kind,
        "directories": ["/workspace/project"]
    })
}

fn jsonl(values: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn write_transcript(root: &Path, values: &[Value]) -> PathBuf {
    let path = transcript_path(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, jsonl(values)).unwrap();
    path
}

fn rediscover(root: &Path, expected_path: &Path) -> GeminiTranscriptSource {
    discover_gemini_transcripts(root)
        .unwrap()
        .transcripts
        .into_iter()
        .find(|source| source.path == fs::canonicalize(expected_path).unwrap())
        .unwrap()
}

fn scan_collect(
    source: &GeminiTranscriptSource,
    previous: Option<&GeminiPreviousSource>,
) -> (GeminiScanOutcome, Vec<GeminiRetainedEvent>) {
    let mut reader = read_gemini_transcript_pages(source, previous).unwrap();
    let mut rows = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        rows.extend(page.events);
    }
    let outcome = reader.outcome().cloned().unwrap();
    (outcome, rows)
}

fn previous(outcome: &GeminiScanOutcome, prior_route_still_live: bool) -> GeminiPreviousSource {
    GeminiPreviousSource {
        checkpoint: outcome.checkpoint.clone(),
        prior_route_still_live,
    }
}

mod discovery;
mod paging;
mod parsing;
mod resume;
mod retention;
