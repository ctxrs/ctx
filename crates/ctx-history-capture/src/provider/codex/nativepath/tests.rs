use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};

use ctx_history_core::{AgentType, CaptureProvider, EventType};
use crate::provider::codex::catalog::CatalogSession;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;
use crate::{
    complete_content::{
        jsonl::JSONL_COMPLETE_CONTENT_LOCATOR_KIND, CompleteContentBodyDigest,
        VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    observe_ordinary_file, CODEX_SESSION_SOURCE_FORMAT,
};

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
    let observation = observe_ordinary_file(path).unwrap();
    let modified_at_ms = observation
        .modified_at()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap())
        .unwrap_or_default();
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
        file_size_bytes: observation.len(),
        file_modified_at_ms: modified_at_ms,
        cataloged_at_ms: 1,
        metadata: json!({
            "inventory_file_change_token_v1": observation.token_hex(),
        }),
    }
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
    rows: Vec<CodexEventRow>,
    pro_outputs: Vec<crate::ProOutputObservation>,
    pages: Vec<(usize, usize)>,
    pro_pages: Vec<(usize, usize)>,
    physical_records: Vec<u64>,
    frontiers: Vec<(CodexNativeFrontier, CodexNativeFrontier)>,
    pro_frontiers: Vec<(CodexNativeFrontier, CodexNativeFrontier)>,
    core_receipts: Vec<CodexNativePageReceipt>,
    pro_receipts: Vec<CodexNativeProOutputPageReceipt>,
    owner_ids: BTreeSet<String>,
}

fn scan_collect(
    source: CodexCatalogSource,
    proof: Option<&CodexAppendProof>,
) -> (CodexSourceScan, CollectingSink) {
    scan_collect_profile(source, proof, CodexNativeProfile::CoreOnly)
}

fn scan_collect_profile(
    source: CodexCatalogSource,
    proof: Option<&CodexAppendProof>,
    profile: CodexNativeProfile,
) -> (CodexSourceScan, CollectingSink) {
    let mut scanner = CodexNativeScanner::new(source, proof, profile).unwrap();
    let mut collected = CollectingSink::default();
    while let Some(page) = scanner.next_page().unwrap() {
        match page {
            CodexNativeOwnedPage::Core(page) => {
                let units = page.core_rows.len();
                assert!(units <= MAX_CODEX_PAGE_ROWS);
                assert!(page.physical_records <= MAX_CODEX_PAGE_ROWS as u64);
                assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
                assert_eq!(
                    page.next_safe_frontier
                        .next_raw_ordinal
                        .saturating_sub(page.expected_frontier.next_raw_ordinal),
                    page.physical_records
                );
                if let Some(owner) = page.owner.as_ref() {
                    collected.owner_ids.insert(owner.native_session_id.clone());
                }
                let receipt = page.receipt();
                collected.pages.push((units, page.serialized_bytes));
                collected.physical_records.push(page.physical_records);
                collected
                    .frontiers
                    .push((page.expected_frontier, page.next_safe_frontier));
                collected.core_receipts.push(receipt);
                collected.rows.extend(page.core_rows);
            }
            CodexNativeOwnedPage::Pro(page) => {
                assert!(page.outputs.len() <= MAX_CODEX_PAGE_ROWS);
                assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
                let receipt = page.receipt();
                collected
                    .pro_pages
                    .push((page.outputs.len(), page.serialized_bytes));
                collected
                    .pro_frontiers
                    .push((page.expected_frontier, page.next_safe_frontier));
                collected.pro_receipts.push(receipt);
                collected.pro_outputs.extend(page.outputs);
            }
        }
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
    let error = CodexNativeScanner::new(
        discover_one(path, native_session_id),
        Some(&proof),
        CodexNativeProfile::CoreOnly,
    )
    .unwrap_err();
    assert!(format!("{error}").contains("invalid Codex append proof"));
}

fn known_source(route_live: bool, proof: CodexAppendProof) -> CodexKnownSource {
    CodexKnownSource { proof, route_live }
}
