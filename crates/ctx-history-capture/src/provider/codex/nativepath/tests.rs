use std::{
    collections::BTreeSet,
    fs,
    hint::black_box,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::provider::codex::catalog::CatalogSession;
use ctx_history_core::{AgentType, CaptureProvider, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::rows::CodexSourceBackedRowV0;
use super::*;
use crate::{common::io::open_provider_source_file, CODEX_SESSION_SOURCE_FORMAT};

fn jsonl(value: Value) -> String {
    let mut line = serde_json::to_string(&value).unwrap();
    line.push('\n');
    line
}

pub(super) fn session_meta(id: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": id,
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    }))
}

pub(super) fn message(role: &str, text: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": role,
            "content": [{"type": "input_text", "text": text}]
        }
    }))
}

fn tool_call(call_id: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": {"cmd": "printf retained"}
        }
    }))
}

fn tool_output(call_id: &str, output: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": output
        }
    }))
}

fn successful_tool_output(call_id: &str, output: &str) -> String {
    tool_output(
        call_id,
        &format!("Script completed\nProcess exited with code 0\n{output}"),
    )
}

fn failed_tool_output(call_id: &str, output: &str) -> String {
    tool_output(
        call_id,
        &format!("Process exited with code 7\nWall time: 0.25 seconds\n{output}"),
    )
}

fn timed_out_tool_output(call_id: &str, output: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "timed_out": true,
            "duration_ms": 9_000,
            "output": format!("command timed out\n{output}")
        }
    }))
}

fn reasoning(text: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": text}]
        }
    }))
}

fn catalog_session(path: &Path, native_session_id: &str) -> CatalogSession {
    let opened = open_provider_source_file(path).unwrap();
    let observation = opened_codex_file_observation(path, opened.file()).unwrap();
    CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        source_root: path.parent().unwrap().display().to_string(),
        source_path: path.display().to_string(),
        external_session_id: Some(native_session_id.to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        external_agent_id: None,
        cwd: Some("/workspace".to_owned()),
        session_started_at_ms: Some(0),
        file_size_bytes: observation.len,
        file_modified_at_ms: observation.modified_at_ms,
        cataloged_at_ms: 1,
        metadata: json!({
            "inventory_file_change_token_v1": hex_token(&observation.change_token),
            "inventory_file_stable_token_v1": observation.stable_token.as_ref().map(hex_token),
        }),
    }
}

fn hex_token(token: &[u8; 32]) -> String {
    token.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn discover_one(path: &Path, native_session_id: &str) -> CodexCatalogSource {
    let discovery = discover_codex_catalog_sources(&[catalog_session(path, native_session_id)]);
    assert!(discovery.rejections.is_empty());
    assert_eq!(discovery.sources.len(), 1);
    discovery.sources.into_iter().next().unwrap()
}

pub(super) fn write_source(contents: &str) -> (TempDir, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("rollout.jsonl");
    fs::write(&path, contents).unwrap();
    (temp, path)
}

#[derive(Default)]
struct CollectingSink {
    rows: Vec<CodexSourceBackedRowV0>,
    pages: Vec<(usize, usize)>,
    physical_records: Vec<u64>,
    frontiers: Vec<(CodexNativeFrontier, CodexNativeFrontier)>,
    owner_ids: BTreeSet<String>,
}

fn scan_collect(
    source: CodexCatalogSource,
    proof: Option<&CodexAppendProof>,
) -> (CodexSourceScan, CollectingSink) {
    let mut scanner = CodexNativeScanner::new_source_backed_v0(source, proof).unwrap();
    let mut collected = CollectingSink::default();
    while let Some(page) = scanner.next_page().unwrap() {
        let CodexNativeOwnedPage::Core(page) = page;
        assert!(page.core_rows.is_empty());
        let units = page.source_backed_rows.len();
        assert!(units <= MAX_CODEX_PAGE_ROWS);
        assert!(
            page.serialized_bytes <= MAX_CODEX_PAGE_BYTES
                || (units == 1
                    && page.serialized_bytes <= MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES)
        );
        assert_eq!(
            page.next_safe_frontier
                .next_raw_ordinal
                .saturating_sub(page.expected_frontier.next_raw_ordinal),
            page.physical_records
        );
        if let Some(owner) = page.owner.as_ref() {
            collected.owner_ids.insert(owner.native_session_id.clone());
        }
        collected.pages.push((units, page.serialized_bytes));
        collected.physical_records.push(page.physical_records);
        collected
            .frontiers
            .push((page.expected_frontier, page.next_safe_frontier));
        collected.rows.extend(page.source_backed_rows);
    }
    let scan = scanner.finish().unwrap();
    (scan, collected)
}

use super::record;

mod bounds;
mod lifecycle;
mod paging;
mod profiles;
mod structure;

fn quickbench_fixture_paths(fixture_root: &Path) -> Vec<PathBuf> {
    let mut directories = vec![fixture_root.join("sessions")];
    let mut paths = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
            {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    paths
}

fn quickbench_fixture_hash(fixture_root: &Path, paths: &[PathBuf]) -> (u64, String) {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-codex-nativepath-quickbench-fixture-v1\0");
    let mut byte_size = 0_u64;
    for path in paths {
        let relative = path.strip_prefix(fixture_root).unwrap().to_string_lossy();
        let bytes = fs::read(path).unwrap();
        hasher.update((relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
        byte_size = byte_size.saturating_add(bytes.len() as u64);
    }
    let digest = hasher.finalize();
    let sha256 = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    (byte_size, sha256)
}

fn assert_checkpoint_replay_rejected(
    path: &Path,
    native_session_id: &str,
    checkpoint: CodexNativeCheckpoint,
) {
    let identity = CodexSourceIdentity::new(
        "canonical-tampered",
        path.parent().unwrap().display().to_string(),
        path.to_path_buf(),
    )
    .unwrap();
    let proof = CodexAppendProof::new(identity, CodexCheckpointGeneration::new(88), checkpoint);
    let error = CodexNativeScanner::new_source_backed_v0(
        discover_one(path, native_session_id),
        Some(&proof),
    )
    .unwrap_err();
    assert!(format!("{error}").contains("invalid Codex append proof"));
}
