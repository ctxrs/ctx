use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufReader, Write},
};

use crate::test_support_paths::tempdir;
use ctx_history_core::{ContentRef, EventType, Fidelity};
use serde_json::{json, Value};

use super::header::codex_session_header;
use super::projection::{CodexCapturedBatchProjector, CodexParserCheckpoint};
use super::source_file::CodexFrozenFileMetadata;
use super::*;
use crate::captured_batch::jsonl::{jsonl_position_offset, JsonlBatchProducer};
use crate::captured_batch::{
    CapturedRecord, ProviderRecordKind, SourceObservation, CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
    CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::file_touches::PROVIDER_FILE_TOUCH_LIMIT_REJECTION;
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchProjector, CertifiedProviderCursor, ExistingSessionEventOutcome,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::{
    ProviderAdapterContext, ProviderNormalizationResult, CODEX_SESSION_SOURCE_FORMAT,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

fn jsonl_line(value: Value) -> String {
    let mut encoded = serde_json::to_string(&value).unwrap();
    encoded.push('\n');
    encoded
}

fn session_meta(id: &str, parent: Option<&str>) -> String {
    jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": id,
            "timestamp": "2026-07-18T12:00:00Z",
            "cwd": "/workspace/ctx",
            "originator": "codex-cli",
            "source": parent.map(|parent| json!({"parent_thread_id": parent}))
        }
    }))
}

fn root_rollout_session_meta(id: &str) -> String {
    jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": id,
            "timestamp": "2026-07-18T12:00:00Z",
            "cwd": "/workspace/ctx",
            "originator": "Codex Desktop",
            "source": "vscode"
        }
    }))
}

fn child_rollout_session_meta(id: &str, parent: &str) -> String {
    jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": id,
            "timestamp": "2026-07-18T12:00:00Z",
            "cwd": "/workspace/ctx",
            "originator": "Codex Desktop",
            "source": {
                "subagent": {"thread_spawn": {"parent_thread_id": parent}}
            }
        }
    }))
}

fn eventless_patch(path: &str) -> String {
    jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:02Z",
        "type": "event_msg",
        "payload": {
            "type": "patch_apply_end",
            "patch": format!(
                "*** Begin Patch\n*** Update File: {path}\n*** End Patch"
            )
        }
    }))
}

fn message(index: usize) -> String {
    jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": if index.is_multiple_of(2) { "user" } else { "assistant" },
            "content": [{
                "type": if index.is_multiple_of(2) { "input_text" } else { "output_text" },
                "text": format!("bounded Codex message {index}")
            }]
        }
    }))
}

fn file_touch_call(path: &str) -> String {
    jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "edit_file",
            "call_id": "multi-session-touch",
            "arguments": {
                "files": [{"path": path}]
            }
        }
    }))
}

mod correlation;
mod custom_exec;
mod ownership;
mod projection;
mod resume;
mod source_file;
