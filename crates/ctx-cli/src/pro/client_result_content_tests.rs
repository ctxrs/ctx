use std::{collections::BTreeMap, fs, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_capture::{
    complete_content::{jsonl::JsonlCompleteContentResolver, ResultContentResolverRegistry},
    import_codex_session_jsonl, stable_capture_uuid, CodexSessionImportOptions,
};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;
use ctx_pro_host_protocol::{
    canonical_payload_bytes, initial_journal_digest, journal_record_digest,
    journal_sync_envelope_bytes, sha256_hex, JournalCheckpoint, JournalPosition, JournalSyncMode,
    JournalSyncRequest, ResultContentSidecar, MAX_JOURNAL_SYNC_ENVELOPE_BYTES,
    MAX_RESULT_CONTENT_BYTES_PER_ITEM, PROTOCOL_FINGERPRINT,
};
use serde_json::json;
use uuid::Uuid;

use super::super::{protocol_checkpoint, protocol_journal_record};
use super::{hydrate_result_contents, ResultHydrationCounts};

fn tempdir() -> std::io::Result<tempfile::TempDir> {
    let temp_root = fs::canonicalize(std::env::temp_dir())?;
    tempfile::Builder::new()
        .prefix("ctx-pro-result-content-")
        .tempdir_in(temp_root)
}

fn now() -> DateTime<Utc> {
    "2026-07-23T00:00:00Z".parse().expect("valid fixture time")
}

fn write_codex_source(path: &Path, index: usize, output: &str) {
    let mut records = vec![
        json!({
            "timestamp": "2026-07-23T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": format!("envelope-budget-session-{index}"),
                "timestamp": "2026-07-23T00:00:00Z",
                "cwd": "/workspace/project",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-07-23T00:00:00.500Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("envelope fixture {index}")
                }]
            }
        }),
    ];
    records.extend([
        json!({
            "timestamp": "2026-07-23T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": format!("envelope-budget-call-{index}"),
                "arguments": "{\"cmd\":\"emit bounded result\"}"
            }
        }),
        json!({
            "timestamp": "2026-07-23T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": format!("envelope-budget-call-{index}"),
                "output": output
            }
        }),
    ]);
    let transcript = records
        .into_iter()
        .map(|value| {
            format!(
                "{}\n",
                serde_json::to_string(&value).expect("serialize fixture")
            )
        })
        .collect::<String>();
    fs::write(path, transcript).expect("write Codex fixture");
}

fn import_codex_source(store: &mut Store, path: &Path, index: usize) -> (Uuid, Uuid) {
    let imported = import_codex_session_jsonl(
        path,
        store,
        CodexSessionImportOptions {
            imported_at: now(),
            ..CodexSessionImportOptions::default()
        },
    )
    .expect("import Codex fixture");
    assert_eq!(imported.failed, 0, "{:?}", imported.failures);
    let session = store
        .session_by_external_session(
            CaptureProvider::Codex,
            &format!("envelope-budget-session-{index}"),
        )
        .expect("query imported session")
        .expect("imported session");
    let result = store
        .events_for_session(session.id)
        .expect("query imported events")
        .into_iter()
        .find(|event| event.event_type == EventType::CommandOutput)
        .expect("imported command result");
    (
        result.id,
        result.capture_source_id.expect("result capture source"),
    )
}

fn expected_codex_source_id(path: &Path, index: usize) -> Uuid {
    let raw_path = path.display().to_string();
    let identity = serde_json::to_string(&(
        "provider-source-v2",
        CaptureProvider::Codex.as_str(),
        format!("envelope-budget-session-{index}"),
        "codex_session_jsonl",
        Some(raw_path.as_str()),
    ))
    .expect("serialize source identity");
    stable_capture_uuid(&identity, "source")
}

fn rechain_request(request: &mut JournalSyncRequest) {
    let mut prior = request.prior_checkpoint.cumulative_digest.clone();
    for record in &mut request.records {
        let payload = record
            .canonical_payload
            .as_ref()
            .expect("upsert fixture payload");
        record.payload_sha256 =
            sha256_hex(&canonical_payload_bytes(payload).expect("canonical fixture payload"));
        record.cumulative_digest =
            journal_record_digest(&prior, record).expect("rechain fixture record");
        prior.clone_from(&record.cumulative_digest);
    }
    let last = request.records.last().expect("fixture records");
    request.frozen_through.position.sequence = last.sequence;
    request
        .frozen_through
        .cumulative_digest
        .clone_from(&last.cumulative_digest);
}

fn journal_request(store: &Store) -> JournalSyncRequest {
    let genesis = store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate projection journal");
    let snapshot = store
        .projection_journal_snapshot(None)
        .expect("read projection journal");
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: snapshot.canonical_schema_version,
        canonical_schema_identity: snapshot.canonical_schema_identity,
        projection_contract_version: snapshot.projection_contract_version,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation: genesis.position.generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial_journal_digest(genesis.position.generation),
        },
        frozen_through: protocol_checkpoint(snapshot.frozen_through),
        authorized_repository_roots: snapshot.authorized_repository_roots,
        records: snapshot
            .records
            .into_iter()
            .map(protocol_journal_record)
            .collect(),
        result_contents: Vec::new(),
    }
}

fn result_ids_in_sequence(request: &JournalSyncRequest) -> Vec<Uuid> {
    request
        .records
        .iter()
        .filter_map(|record| {
            serde_json::from_value::<ctx_history_core::ContentRef>(
                record
                    .canonical_payload
                    .as_ref()?
                    .pointer("/result/content_ref")?
                    .clone(),
            )
            .ok()?;
            Some(record.stable_entity_id)
        })
        .collect()
}

#[test]
fn envelope_rejection_releases_capacity_for_later_result() {
    let temp = tempdir().expect("temp dir");
    let mut store = Store::open(temp.path().join("ctx.db")).expect("open Store");
    let mut sources = Vec::new();

    for index in 0..5 {
        let path = temp.path().join(format!("rollout-{index}.jsonl"));
        let source_id = expected_codex_source_id(&path, index);
        sources.push((source_id, index, path));
    }
    sources.sort_by_key(|(source_id, _, _)| *source_id);

    let later_marker = "LATER-ENVELOPE-BACKFILL:";
    let later_output = format!("{later_marker}{}", "x".repeat(16 * 1024));
    let mut expected_results = BTreeMap::new();
    for (ordinal, (_, index, path)) in sources.iter().enumerate() {
        let output = match ordinal {
            0 => "\"".repeat(MAX_RESULT_CONTENT_BYTES_PER_ITEM),
            1..=3 => "x".repeat(MAX_RESULT_CONTENT_BYTES_PER_ITEM),
            4 => later_output.clone(),
            _ => unreachable!("five fixtures"),
        };
        write_codex_source(path, *index, &output);
        let (event_id, source_id) = import_codex_source(&mut store, path, *index);
        assert_eq!(source_id, sources[ordinal].0);
        expected_results.insert(source_id, (event_id, output.len()));
    }

    let mut request = journal_request(&store);

    let result_ids_in_sequence = result_ids_in_sequence(&request);
    let expected_in_source_order = expected_results
        .values()
        .map(|(event_id, _)| *event_id)
        .collect::<Vec<_>>();
    assert_eq!(result_ids_in_sequence, expected_in_source_order);
    assert!(expected_results
        .values()
        .take(4)
        .all(|(_, bytes)| *bytes == MAX_RESULT_CONTENT_BYTES_PER_ITEM));

    let padding_record = request
        .records
        .iter_mut()
        .find(|record| {
            record
                .canonical_payload
                .as_ref()
                .and_then(|payload| payload.pointer("/result/content_ref"))
                .and_then(|value| {
                    serde_json::from_value::<ctx_history_core::ContentRef>(value.clone()).ok()
                })
                .is_none()
        })
        .expect("non-result record for envelope padding");
    padding_record.canonical_payload = Some(json!({"padding": ""}));
    rechain_request(&mut request);
    let target_base_bytes = MAX_JOURNAL_SYNC_ENVELOPE_BYTES - 64 * 1024;
    let unpadded_bytes = journal_sync_envelope_bytes(&request).expect("measure unpadded request");
    let padding_bytes = target_base_bytes
        .checked_sub(unpadded_bytes)
        .expect("fixture leaves room for envelope padding");
    request
        .records
        .iter_mut()
        .find(|record| {
            record
                .canonical_payload
                .as_ref()
                .is_some_and(|payload| payload.get("padding").is_some())
        })
        .expect("padding record")
        .canonical_payload = Some(json!({"padding": "p".repeat(padding_bytes)}));
    rechain_request(&mut request);
    assert_eq!(
        journal_sync_envelope_bytes(&request).expect("measure padded request"),
        target_base_bytes
    );
    request.validate().expect("base request is valid");

    let mut resolver_registry = ResultContentResolverRegistry::new();
    resolver_registry.register(JsonlCompleteContentResolver::new());
    let mut admitted = BTreeMap::new();
    for result_id in result_ids_in_sequence.iter().take(4) {
        let record = request
            .records
            .iter()
            .find(|record| record.stable_entity_id == *result_id)
            .expect("early result record");
        let content_ref: ctx_history_core::ContentRef = serde_json::from_value(
            record
                .canonical_payload
                .as_ref()
                .and_then(|payload| payload.pointer("/result/content_ref"))
                .expect("early result content reference")
                .clone(),
        )
        .expect("valid early result content reference");
        let (_, source_request) =
            super::result_content_request(&store, *result_id, content_ref.clone(), &mut admitted)
                .expect("early result route admission");
        let resolved = resolver_registry
            .resolve(&[source_request])
            .pop()
            .expect("one early resolver response")
            .expect("early result resolves");
        assert_eq!(resolved.content.len(), MAX_RESULT_CONTENT_BYTES_PER_ITEM);
        let mut with_early_result = request.clone();
        with_early_result
            .result_contents
            .push(ResultContentSidecar {
                journal_sequence: record.sequence,
                stable_entity_id: *result_id,
                content_ref,
                content: resolved.content,
            });
        assert!(
            journal_sync_envelope_bytes(&with_early_result).expect("measure early result envelope")
                > MAX_JOURNAL_SYNC_ENVELOPE_BYTES,
            "each early result must be rejected only by final envelope admission"
        );
    }

    let counts = hydrate_result_contents(&store, &mut request);
    assert_eq!(
        counts,
        ResultHydrationCounts {
            hydrated: 1,
            omitted: 4,
            resolver_batches: 5,
        }
    );
    assert_eq!(request.result_contents.len(), 1);
    assert!(request.result_contents[0].content.starts_with(later_marker));
    assert!(
        journal_sync_envelope_bytes(&request).expect("measure final request")
            <= MAX_JOURNAL_SYNC_ENVELOPE_BYTES
    );
    request.validate().expect("final hydrated request is valid");
}

#[test]
fn aggregate_reservation_precedes_source_admission_and_reads() {
    let temp = tempdir().expect("temp dir");
    let mut store = Store::open(temp.path().join("ctx.db")).expect("open Store");
    let mut sources = (0..5)
        .map(|index| {
            let path = temp.path().join(format!("admission-{index}.jsonl"));
            (expected_codex_source_id(&path, index), index, path)
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|(source_id, _, _)| *source_id);

    let mut expected_results = BTreeMap::new();
    for (ordinal, (_, index, path)) in sources.iter().enumerate() {
        let prefix = format!("admission-{ordinal}:");
        let output = format!(
            "{prefix}{}",
            "x".repeat(MAX_RESULT_CONTENT_BYTES_PER_ITEM - prefix.len())
        );
        write_codex_source(path, *index, &output);
        let (event_id, source_id) = import_codex_source(&mut store, path, *index);
        assert_eq!(source_id, sources[ordinal].0);
        expected_results.insert(source_id, event_id);
    }

    let mut request = journal_request(&store);
    let result_ids = result_ids_in_sequence(&request);
    let expected_ids = expected_results.values().copied().collect::<Vec<_>>();
    assert_eq!(result_ids, expected_ids);

    let mut first_attempts = Vec::new();
    let first =
        super::hydrate_result_contents_with_admission_observer(&store, &mut request, |event_id| {
            first_attempts.push(event_id)
        });
    assert_eq!(
        first,
        ResultHydrationCounts {
            hydrated: 4,
            omitted: 1,
            resolver_batches: 4,
        }
    );
    assert_eq!(first_attempts, result_ids[..4]);
    assert!(!first_attempts.contains(&result_ids[4]));
    assert!(!request
        .result_contents
        .iter()
        .any(|sidecar| sidecar.stable_entity_id == result_ids[4]));
    request.validate().expect("aggregate-bounded request");

    fs::remove_file(&sources[0].2).expect("remove first selected source");
    let mut retry_attempts = Vec::new();
    let retry =
        super::hydrate_result_contents_with_admission_observer(&store, &mut request, |event_id| {
            retry_attempts.push(event_id)
        });
    assert_eq!(
        retry,
        ResultHydrationCounts {
            hydrated: 4,
            omitted: 1,
            resolver_batches: 4,
        }
    );
    assert_eq!(retry_attempts, result_ids);
    assert!(request
        .result_contents
        .iter()
        .any(|sidecar| sidecar.stable_entity_id == result_ids[4]));
    assert!(request
        .result_contents
        .windows(2)
        .all(|pair| pair[0].journal_sequence < pair[1].journal_sequence));
    request.validate().expect("backfilled request");
}
