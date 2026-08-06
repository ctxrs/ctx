use super::*;
use ctx_history_core::CoreDiscoveryExclusion;
use std::{fs::OpenOptions, io::Write, path::Path};

fn projected(values: &[Value]) -> Vec<ctx_history_core::CoreRecord> {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut transcript = vec![header("retrieval-session", "main")];
    transcript.extend_from_slice(values);
    let path = write_transcript(&root, &transcript);
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    project_gemini_test_events(&source, rows).unwrap()
}

fn excluded(record: &ctx_history_core::CoreRecord) -> bool {
    record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
}

fn registry(root: &Path) -> crate::provider::source_backed::SourceBackedProviderRegistry {
    use crate::provider::source_backed::{
        register_landed_source_backed_route, SourceBackedProviderRegistry,
        SourceBackedRouteSelection,
    };
    use crate::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus,
    };

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: ctx_history_core::CaptureProvider::Gemini,
            path: root.to_path_buf(),
            exists: true,
            source_format: crate::GEMINI_CLI_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn indexed_records(index: &Path) -> Vec<ctx_history_core::CoreRecord> {
    let verified = ctx_history_index::VerifiedIndex::open(index).unwrap();
    let source = verified.manifest().sources[0]
        .observation()
        .source()
        .clone();
    verified
        .core_source_event_page(&source, None, 64)
        .unwrap()
        .items
        .into_iter()
        .map(|item| {
            verified
                .core_record_by_id(item.event_id.as_uuid())
                .unwrap()
                .unwrap()
        })
        .collect()
}

#[test]
fn exact_cli_call_and_structural_success_envelope_are_excluded() {
    let records = projected(&[
        json!({
            "id": "call-record",
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "call-1",
                "name": "run_shell_command",
                "args": {"command": "ctx search needle"}
            }]
        }),
        json!({
            "id": "result-record",
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "call-1",
                "name": "run_shell_command",
                "result": {"content": "exact payload", "exitCode": 0}
            }]
        }),
    ]);

    assert_eq!(records.len(), 2);
    assert!(records.iter().all(excluded));
    assert_eq!(
        records[1].content.normalized_body.as_deref(),
        Some("exact payload")
    );
}

#[test]
fn duplicate_result_terminals_fail_open_including_the_earlier_result() {
    let records = projected(&[
        json!({
            "id": "call-record",
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "duplicate-result",
                "name": "run_shell_command",
                "args": {"command": "ctx search duplicate-result"}
            }]
        }),
        json!({
            "id": "first-result-record",
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "duplicate-result",
                "name": "run_shell_command",
                "result": {"content": "first duplicate Gemini payload", "exitCode": 0}
            }]
        }),
        json!({
            "id": "second-result-record",
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "duplicate-result",
                "name": "run_shell_command",
                "result": {"content": "second duplicate Gemini payload", "exitCode": 0}
            }]
        }),
    ]);

    assert_eq!(records.len(), 3);
    assert!(excluded(&records[0]));
    assert!(!excluded(&records[1]));
    assert!(!excluded(&records[2]));
    assert_eq!(
        records[1].content.normalized_body.as_deref(),
        Some("first duplicate Gemini payload")
    );
    assert_eq!(
        records[2].content.normalized_body.as_deref(),
        Some("second duplicate Gemini payload")
    );
}

#[test]
fn malformed_duplicate_result_terminal_invalidates_source_wide_uniqueness() {
    use crate::provider::source_backed::refresh_source_backed_generation;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[
        header("gemini-malformed-duplicate-terminal", "main"),
        json!({
            "id": "call-record",
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "duplicate-terminal-call",
                "name": "run_shell_command",
                "args": {"command": "ctx search malformed-duplicate-terminal"}
            }]
        }),
        json!({
            "id": "first-result-record",
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "gemini",
            "toolCalls": [{
                "id": "duplicate-terminal-call",
                "name": "run_shell_command",
                "result": {"content": "first authoritative payload", "exitCode": 0}
            }]
        }),
    ]);
    bytes.extend_from_slice(
        br#"{"id":"malformed-result-record","timestamp":"2026-01-01T00:00:03Z","type":"gemini","toolCalls":[{"id":"other-call","id":"duplicate-terminal-call","name":"run_shell_command","result":{"content":"ambiguous duplicate terminal payload","exitCode":0}}]}"#,
    );
    bytes.push(b'\n');
    std::fs::write(&path, bytes).unwrap();

    let registry = registry(&root);
    let index = temp.path().join("index");
    refresh_source_backed_generation(
        &index,
        &registry,
        ctx_history_index::WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let records = indexed_records(&index);

    assert_eq!(records.len(), 3);
    let record_with_body = |needle: &str| {
        records
            .iter()
            .find(|record| {
                record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains(needle))
            })
            .unwrap()
    };
    assert!(excluded(record_with_body(
        "ctx search malformed-duplicate-terminal"
    )));
    assert!(!excluded(record_with_body("first authoritative payload")));
    assert!(!excluded(record_with_body(
        "ambiguous duplicate terminal payload"
    )));
}

#[test]
fn malformed_terminal_candidates_poison_result_uniqueness_authority() {
    use super::super::parser::gemini_result_terminal_authority_is_ambiguous;

    assert!(gemini_result_terminal_authority_is_ambiguous(
        br#"{"type":"gemini","toolCalls":[{"id":"call","result":{"content":"payload"}}]} trailing"#,
    ));
    assert!(gemini_result_terminal_authority_is_ambiguous(
        br#"{"type":"gemini","toolCalls":[],"toolCalls":[{"id":"call","result":{"content":"payload"}}]}"#,
    ));
}

#[test]
fn late_duplicate_result_replacement_corrects_the_earlier_result() {
    use crate::provider::source_backed::refresh_source_backed_generation;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let native_session_id = "gemini-late-duplicate";
    let call_id = "late-duplicate-result";
    let path = write_transcript(
        &root,
        &[
            header(native_session_id, "main"),
            json!({
                "id": "call-record",
                "timestamp": "2026-01-01T00:00:01Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": call_id,
                    "name": "run_shell_command",
                    "args": {"command": "ctx search late-duplicate"}
                }]
            }),
            json!({
                "id": "first-result-record",
                "timestamp": "2026-01-01T00:00:02Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": call_id,
                    "name": "run_shell_command",
                    "result": {"content": "first late duplicate Gemini payload", "exitCode": 0}
                }]
            }),
        ],
    );
    let registry = registry(&root);
    let index = temp.path().join("index");
    let writer_options = ctx_history_index::WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    refresh_source_backed_generation(&index, &registry, writer_options.clone()).unwrap();
    let initial = indexed_records(&index);
    assert_eq!(initial.len(), 2);
    assert!(initial.iter().all(excluded));
    let initial_ids = initial
        .iter()
        .map(|record| record.event_id)
        .collect::<Vec<_>>();

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    serde_json::to_writer(
        &mut file,
        &json!({
            "id": "second-result-record",
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "gemini",
            "toolCalls": [{
                "id": call_id,
                "name": "run_shell_command",
                "result": {"content": "second late duplicate Gemini payload", "exitCode": 0}
            }]
        }),
    )
    .unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    drop(file);

    refresh_source_backed_generation(&index, &registry, writer_options).unwrap();
    let corrected = indexed_records(&index);
    assert_eq!(corrected.len(), 3);
    assert!(initial_ids
        .iter()
        .all(|event_id| corrected.iter().any(|record| record.event_id == *event_id)));
    assert_eq!(
        corrected.iter().filter(|record| excluded(record)).count(),
        1
    );
    for result_body in [
        "first late duplicate Gemini payload",
        "second late duplicate Gemini payload",
    ] {
        let result = corrected
            .iter()
            .find(|record| record.content.normalized_body.as_deref() == Some(result_body))
            .unwrap();
        assert!(!excluded(result));
    }
}

#[test]
fn generic_foreign_shell_aliases_are_not_provider_attested() {
    for tool_name in ["command", "bash", "shell", "exec", "exec_command"] {
        let records = projected(&[
            json!({
                "id": "call-record",
                "timestamp": "2026-01-01T00:00:01Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "foreign-call",
                    "name": tool_name,
                    "args": {"command": "ctx search needle"}
                }]
            }),
            json!({
                "id": "result-record",
                "timestamp": "2026-01-01T00:00:02Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "foreign-call",
                    "name": tool_name,
                    "result": {"content": "foreign payload", "exitCode": 0}
                }]
            }),
        ]);
        assert_eq!(records.len(), 2, "{tool_name}");
        assert!(
            records.iter().all(|record| !excluded(record)),
            "{tool_name}"
        );
    }
}

#[test]
fn aggregate_and_result_ambiguities_fail_open_without_losing_bodies() {
    let records = projected(&[
        json!({
            "id": "mixed-call-record",
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "gemini",
            "toolCalls": [
                {"id": "derived", "name": "run_shell_command", "args": {"command": "ctx search needle"}},
                {"id": "ordinary", "name": "run_shell_command", "args": {"command": "ctx status"}}
            ]
        }),
        json!({
            "id": "mixed-result-record",
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "gemini",
            "toolCalls": [
                {"id": "derived", "result": {"content": "derived payload", "exitCode": 0}},
                {"id": "ordinary", "result": {"content": "ordinary payload", "exitCode": 0}}
            ]
        }),
        json!({
            "id": "diagnostic-call-record",
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "gemini",
            "toolCalls": [{"id": "diagnostic", "name": "run_shell_command", "args": {"command": "ctx show event deadbeef"}}]
        }),
        json!({
            "id": "diagnostic-result-record",
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "gemini",
            "toolCalls": [{"id": "diagnostic", "result": {"content": "kept diagnostic payload", "stderr": "warning", "exitCode": 0}}]
        }),
        json!({
            "id": "unknown-call-record",
            "timestamp": "2026-01-01T00:00:05Z",
            "type": "gemini",
            "toolCalls": [{"id": "unknown", "name": "run_shell_command", "args": {"command": "ctx search another"}}]
        }),
        json!({
            "id": "unknown-result-record",
            "timestamp": "2026-01-01T00:00:06Z",
            "type": "gemini",
            "toolCalls": [{"id": "unknown", "result": {"content": "kept unknown payload", "mystery": true, "exitCode": 0}}]
        }),
    ]);

    assert_eq!(records.len(), 7);
    assert!(!excluded(&records[0]));
    assert!(!excluded(&records[1]));
    assert!(!excluded(&records[2]));
    assert!(excluded(&records[3]));
    assert!(!excluded(&records[4]));
    assert!(excluded(&records[5]));
    assert!(!excluded(&records[6]));
    assert_eq!(
        records[4].content.normalized_body.as_deref(),
        Some("kept diagnostic payload")
    );
    assert_eq!(
        records[6].content.normalized_body.as_deref(),
        Some("kept unknown payload")
    );
}
