mod support;

use std::{fs, path::Path};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreContentPolicyStatus, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use serde_json::Value;

use support::{ctx, data_root, tempdir, TempDir};

fn source(provider: &str, source_path: &Path) -> SourceKey {
    SourceKey::derive(
        provider,
        format!("{provider}_session_jsonl"),
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8(source_path.display().to_string()).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn record(source: &SourceKey, nonce: u64, occurred_at_unix_ms: i64, body: &str) -> CoreRecord {
    let session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8(format!("session-{nonce}")).unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let item_key = NativeItemKey::native_id("message", TypedKey::U64(nonce)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        nonce % 2,
        "message",
        "primary",
        true,
        "event-export-integration-v1",
        body,
    )
    .unwrap();
    record.occurred_at_unix_ms = Some(occurred_at_unix_ms);
    record.provider_session_id = Some(format!("provider-session-{nonce}"));
    record.native_event_id = Some(TypedKey::U64(nonce));
    record.role = Some("user".to_owned());
    record.workspace = Some("工作区/ctx".to_owned());
    record.cwd = Some("/workspace/ctx".to_owned());
    record
}

fn publish(data_root: &Path, revision: u8, sources: &[(SourceKey, Vec<CoreRecord>)]) -> String {
    let index_root = data_root.join("search/lexical");
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default()).unwrap();
    for (source, records) in sources {
        writer.begin_source(source.clone()).unwrap();
        for record in records {
            writer.add_core_record(record.clone()).unwrap();
        }
        let observation =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    "event-export-integration-v1",
                    [revision; 32],
                    ScannedSourceCounts {
                        complete_records: records.len() as u64,
                        retained_records: records.len() as u64,
                        indexed_documents: records.len() as u64,
                        certified_bytes: records.len() as u64 * 10,
                        ..ScannedSourceCounts::default()
                    },
                )
                .unwrap(),
            )
            .unwrap();
    }
    writer.commit(|_| true).unwrap().generation_id
}

fn fixture() -> (TempDir, Vec<CoreRecord>) {
    let temp = tempdir();
    let codex_path = temp.path().join("deleted-codex-source.jsonl");
    let claude_path = temp.path().join("deleted-claude-source.jsonl");
    fs::write(&codex_path, b"original source").unwrap();
    fs::write(&claude_path, b"original source").unwrap();
    let codex = source("codex", &codex_path);
    let claude = source("claude", &claude_path);
    let mut records = vec![
        record(&codex, 1, 1_700_000_000_000, "héllo 🦀"),
        record(&claude, 2, 1_700_000_000_000, "structured"),
        record(&codex, 3, 1_700_000_000_001, "[redacted]"),
        record(&claude, 4, 1_700_000_000_002, "omitted"),
        record(&codex, 5, 1_700_000_000_003, &"large雪".repeat(700)),
    ];
    records[1].content.structured_content =
        Some(serde_json::json!({"nested": [1, true, {"emoji": "🧭"}]}));
    records[2].content.policy_status = CoreContentPolicyStatus::Redacted {
        reason: "provider_secret".to_owned(),
    };
    records[3].content.policy_status = CoreContentPolicyStatus::Omitted {
        reason: "unsupported_binary".to_owned(),
    };
    records[3].content.normalized_body = None;
    records[3].agent_type = "subagent".to_owned();
    records[3].is_primary = false;
    let codex_records = records
        .iter()
        .filter(|record| record.source.provider() == "codex")
        .cloned()
        .collect::<Vec<_>>();
    let claude_records = records
        .iter()
        .filter(|record| record.source.provider() == "claude")
        .cloned()
        .collect::<Vec<_>>();
    publish(
        &data_root(&temp),
        1,
        &[(codex, codex_records), (claude, claude_records)],
    );
    fs::remove_file(codex_path).unwrap();
    fs::remove_file(claude_path).unwrap();
    (temp, records)
}

fn base_args() -> Vec<&'static str> {
    vec![
        "export",
        "events",
        "--since",
        "2023-11-14T22:13:20Z",
        "--until",
        "2023-11-14T22:13:21Z",
    ]
}

fn json_ids(value: &Value) -> Vec<String> {
    value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["ctx_event_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn json_and_jsonl_export_identical_complete_core_ids_without_source_io() {
    let (temp, records) = fixture();
    let mut json_args = base_args();
    json_args.extend(["--max-items", "100", "--format", "json"]);
    let json_output = ctx(&temp).args(&json_args).output().unwrap();
    assert!(
        json_output.status.success(),
        "{}",
        String::from_utf8_lossy(&json_output.stderr)
    );
    assert!(json_output.stderr.is_empty());
    let page: Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert!(page["terminal"].as_bool().unwrap());
    assert!(page["next_cursor"].is_null());
    assert_eq!(page["usage"]["items"], records.len());
    assert_eq!(page["usage"]["bytes"], json_output.stdout.len());
    assert!(page["events"].as_array().unwrap().iter().all(|event| {
        event["schema_version"] == 1
            && event.get("item_id").is_none()
            && event.get("sequence").is_none()
    }));
    let representative = &page["events"][0];
    for field in [
        "ctx_event_id",
        "ctx_source_id",
        "ctx_session_id",
        "root_ctx_session_id",
        "provider",
        "source_format",
        "provider_session_id",
        "native_event_id",
        "event_sequence",
        "occurred_at_unix_ms",
        "event_type",
        "role",
        "workspace",
        "cwd",
        "content",
    ] {
        assert!(representative.get(field).is_some(), "missing {field}");
    }
    assert!(page["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["agent_type"] == "subagent" && event["is_primary"] == false }));
    assert!(page["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["structured_content"]["nested"][2]["emoji"] == "🧭" }));
    assert!(page["events"].as_array().unwrap().iter().any(|event| {
        event["content"]["policy_status"] == "redacted" && event["content"]["complete"] == false
    }));
    assert!(page["events"].as_array().unwrap().iter().any(|event| {
        event["content"]["policy_status"] == "omitted" && event.get("text").is_none()
    }));

    let mut jsonl_args = base_args();
    jsonl_args.extend(["--max-items", "1", "--format", "jsonl"]);
    let jsonl_output = ctx(&temp).args(&jsonl_args).output().unwrap();
    assert!(
        jsonl_output.status.success(),
        "{}",
        String::from_utf8_lossy(&jsonl_output.stderr)
    );
    assert!(jsonl_output.stderr.is_empty());
    let jsonl_ids = String::from_utf8(jsonl_output.stdout)
        .unwrap()
        .lines()
        .map(|line| {
            let event: Value = serde_json::from_str(line).unwrap();
            event["ctx_event_id"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(json_ids(&page), jsonl_ids);

    let mut provider_args = base_args();
    provider_args.extend([
        "--provider",
        "codex",
        "--provider",
        "codex",
        "--max-items",
        "100",
    ]);
    let provider_output = ctx(&temp).args(&provider_args).output().unwrap();
    assert!(provider_output.status.success());
    assert!(provider_output.stderr.is_empty());
    let provider_page: Value = serde_json::from_slice(&provider_output.stdout).unwrap();
    assert!(provider_page["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["provider"] == "codex"));
    assert!(!data_root(&temp).join("usage.sqlite").exists());
}

#[test]
fn json_pages_resume_exactly_and_enforce_final_wire_boundaries() {
    let (temp, _) = fixture();
    let mut one_page_args = base_args();
    one_page_args.extend(["--max-items", "100"]);
    let full = ctx(&temp).args(&one_page_args).output().unwrap();
    assert!(full.status.success());
    let expected: Value = serde_json::from_slice(&full.stdout).unwrap();

    let exact_limit = full.stdout.len().to_string();
    let mut exact_args = base_args();
    exact_args.extend(["--max-items", "100", "--max-bytes", &exact_limit]);
    let exact = ctx(&temp).args(&exact_args).output().unwrap();
    assert!(exact.status.success());
    assert_eq!(exact.stdout.len(), full.stdout.len());

    let mut cursor: Option<String> = None;
    let mut ids = Vec::new();
    loop {
        let mut owned = base_args()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        owned.extend(["--max-items".to_owned(), "2".to_owned()]);
        if let Some(cursor) = &cursor {
            owned.extend(["--cursor".to_owned(), cursor.clone()]);
        }
        let output = ctx(&temp).args(&owned).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let page: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(page["usage"]["bytes"], output.stdout.len());
        ids.extend(json_ids(&page));
        if page["terminal"].as_bool().unwrap() {
            break;
        }
        cursor = Some(page["next_cursor"].as_str().unwrap().to_owned());
    }
    assert_eq!(ids, json_ids(&expected));

    let smaller_limit = (full.stdout.len() - 1).to_string();
    let mut smaller_args = base_args();
    smaller_args.extend(["--max-items", "100", "--max-bytes", &smaller_limit]);
    let smaller = ctx(&temp).args(&smaller_args).output().unwrap();
    assert!(smaller.status.success());
    assert!(smaller.stdout.len() < full.stdout.len());
    let page: Value = serde_json::from_slice(&smaller.stdout).unwrap();
    assert!(!page["terminal"].as_bool().unwrap());
    assert!(page["next_cursor"].is_string());
}

#[test]
fn cursor_tamper_mismatch_eviction_and_oversized_singleton_are_typed() {
    let (temp, records) = fixture();
    for args in [
        [
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20Z",
            "--until",
            "2023-11-14T22:13:20Z",
        ],
        [
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20",
            "--until",
            "2023-11-14T22:13:21Z",
        ],
    ] {
        let output = ctx(&temp).args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error_code"], "invalid_range");
    }
    let mut first_args = base_args();
    first_args.extend(["--max-items", "1"]);
    let first = ctx(&temp).args(&first_args).output().unwrap();
    let first_page: Value = serde_json::from_slice(&first.stdout).unwrap();
    let cursor = first_page["next_cursor"].as_str().unwrap();

    let mut tampered = cursor.to_owned();
    let replacement = if tampered.ends_with('A') { 'B' } else { 'A' };
    tampered.pop();
    tampered.push(replacement);
    let mut tamper_args = base_args();
    tamper_args.extend(["--cursor", &tampered]);
    let tamper = ctx(&temp).args(&tamper_args).output().unwrap();
    assert!(!tamper.status.success());
    assert!(tamper.stdout.is_empty());
    let error: Value = serde_json::from_slice(&tamper.stderr).unwrap();
    assert_eq!(error["error_code"], "invalid_cursor");

    let mismatch = ctx(&temp)
        .args([
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20Z",
            "--until",
            "2023-11-14T22:13:22Z",
            "--cursor",
            cursor,
        ])
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    assert!(mismatch.stdout.is_empty());
    let error: Value = serde_json::from_slice(&mismatch.stderr).unwrap();
    assert_eq!(error["error_code"], "cursor_request_mismatch");

    let before_large = ctx(&temp)
        .args([
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20.002Z",
            "--until",
            "2023-11-14T22:13:20.004Z",
            "--max-items",
            "1",
        ])
        .output()
        .unwrap();
    let before_large: Value = serde_json::from_slice(&before_large.stdout).unwrap();
    let before_large_cursor = before_large["next_cursor"].as_str().unwrap();
    let oversized_continuation = ctx(&temp)
        .args([
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20.002Z",
            "--until",
            "2023-11-14T22:13:20.004Z",
            "--cursor",
            before_large_cursor,
            "--max-bytes",
            "512",
        ])
        .output()
        .unwrap();
    assert!(!oversized_continuation.status.success());
    assert!(oversized_continuation.stdout.is_empty());
    let error: Value = serde_json::from_slice(&oversized_continuation.stderr).unwrap();
    assert_eq!(error["error_code"], "event_too_large");
    assert_eq!(error["cursor"], before_large_cursor);

    let jsonl_partial = ctx(&temp)
        .args([
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20.002Z",
            "--until",
            "2023-11-14T22:13:20.004Z",
            "--format",
            "jsonl",
            "--max-bytes",
            "1024",
        ])
        .output()
        .unwrap();
    assert!(!jsonl_partial.status.success());
    assert!(jsonl_partial.stdout.len() <= 1024);
    let emitted: Value = serde_json::from_slice(&jsonl_partial.stdout).unwrap();
    assert_eq!(
        emitted["ctx_event_id"],
        records[3].event_id.as_uuid().to_string()
    );
    let error: Value = serde_json::from_slice(&jsonl_partial.stderr).unwrap();
    assert_eq!(error["error_code"], "event_too_large");
    let restart_cursor = error["cursor"].as_str().unwrap();
    let resumed = ctx(&temp)
        .args([
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20.002Z",
            "--until",
            "2023-11-14T22:13:20.004Z",
            "--format",
            "jsonl",
            "--cursor",
            restart_cursor,
            "--max-bytes",
            "8192",
        ])
        .output()
        .unwrap();
    assert!(resumed.status.success());
    assert!(resumed.stderr.is_empty());
    let resumed: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(
        resumed["ctx_event_id"],
        records[4].event_id.as_uuid().to_string()
    );

    let codex = records[0].source.clone();
    let claude = records[1].source.clone();
    let codex_records = records
        .iter()
        .filter(|record| record.source.provider() == "codex")
        .cloned()
        .collect::<Vec<_>>();
    let claude_records = records
        .iter()
        .filter(|record| record.source.provider() == "claude")
        .cloned()
        .collect::<Vec<_>>();
    publish(
        &data_root(&temp),
        2,
        &[
            (codex.clone(), codex_records.clone()),
            (claude.clone(), claude_records.clone()),
        ],
    );
    let mut retained_args = base_args();
    retained_args.extend(["--cursor", cursor]);
    let retained = ctx(&temp).args(&retained_args).output().unwrap();
    assert!(retained.status.success());
    assert!(retained.stderr.is_empty());
    let retained: Value = serde_json::from_slice(&retained.stdout).unwrap();
    assert_eq!(retained["generation_id"], first_page["generation_id"]);

    publish(
        &data_root(&temp),
        3,
        &[(codex, codex_records), (claude, claude_records)],
    );
    let mut evicted_args = base_args();
    evicted_args.extend(["--cursor", cursor]);
    let evicted = ctx(&temp).args(&evicted_args).output().unwrap();
    assert!(!evicted.status.success());
    assert!(evicted.stdout.is_empty());
    let error: Value = serde_json::from_slice(&evicted.stderr).unwrap();
    assert_eq!(error["error_code"], "generation_not_retained");

    let oversized = ctx(&temp)
        .args([
            "export",
            "events",
            "--since",
            "2023-11-14T22:13:20.003Z",
            "--until",
            "2023-11-14T22:13:20.004Z",
            "--max-bytes",
            "512",
        ])
        .output()
        .unwrap();
    assert!(!oversized.status.success());
    assert!(oversized.stdout.is_empty());
    let error: Value = serde_json::from_slice(&oversized.stderr).unwrap();
    assert_eq!(error["error_code"], "event_too_large");
    assert!(error["cursor"].is_null());
}
