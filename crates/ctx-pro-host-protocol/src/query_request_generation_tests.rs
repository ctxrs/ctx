use crate::SourceKey;
use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, TypedKey,
};

use super::*;

fn source() -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([3; 32]),
    )
    .unwrap()
}

fn resource(id: &str, kind: ResourceKind, display: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        kind,
        display: display.to_owned(),
    }
}

fn citation(generation: &str) -> EvidenceCitation {
    let source = source();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", TypedKey::U64(1)).unwrap(),
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(2)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    EvidenceCitation {
        core_generation_id: generation.to_owned(),
        source,
        session_id,
        event_id,
        event_sequence: 7,
        byte_range: None,
        evidence_sha256: Some("e".repeat(64)),
    }
}

fn snapshot(generation: char, materializer_revision: &str) -> QuerySnapshotExpectation {
    QuerySnapshotExpectation::Core {
        receipt: CoreMaterializationReceiptIdentity {
            core_generation_id: generation.to_string().repeat(64),
            materializer_revision: materializer_revision.to_owned(),
        },
    }
}

fn file_request(generation: char) -> BlameRequest {
    BlameRequest {
        target: BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: None,
            lines: Some(LineRange { start: 1, end: 1 }),
        },
        limit: 1,
        cursor: None,
        expected_snapshot: snapshot(generation, "materializer-v1"),
    }
}

fn file_result(result_snapshot: QuerySnapshotExpectation, generations: &[String]) -> BlameResult {
    let evidence = generations
        .iter()
        .enumerate()
        .map(|(index, generation)| NumberedEvidence {
            number: u32::try_from(index + 1).unwrap(),
            citation: citation(generation),
        })
        .collect::<Vec<_>>();
    let line_evidence_numbers = evidence.iter().map(|evidence| evidence.number).collect();
    BlameResult {
        snapshot: result_snapshot,
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(
                "repository:fixture",
                ResourceKind::Repository,
                "fixture/repository",
            ),
            requested_lines: Some(LineRange { start: 1, end: 1 }),
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: "file-match:1".to_owned(),
            lines: LineRange { start: 1, end: 1 },
            commit: resource(
                "commit:0123456",
                ResourceKind::Commit,
                "0123456789abcdef0123456789abcdef01234567",
            ),
            line_evidence_numbers,
            production: Vec::new(),
        })],
        evidence,
        next: None,
    }
}

fn empty_file_result(result_snapshot: QuerySnapshotExpectation) -> BlameResult {
    BlameResult {
        snapshot: result_snapshot,
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(
                "repository:fixture",
                ResourceKind::Repository,
                "fixture/repository",
            ),
            requested_lines: Some(LineRange { start: 1, end: 1 }),
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    }
}

#[test]
fn every_citation_generation_matches_the_expected_request_snapshot() {
    let request = file_request('a');
    let matching = file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64), "a".repeat(64)],
    );
    matching.validate_for_request(&request).unwrap();

    let mismatch = file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64), "b".repeat(64)],
    );
    let error = mismatch.validate_for_request(&request).unwrap_err();
    assert_eq!(error.class, ErrorClass::Corrupt);
    assert_eq!(
        error.message,
        "blame evidence generation does not match the requested Core snapshot"
    );
}

#[test]
fn result_snapshot_must_exactly_match_the_request_even_when_the_result_is_empty() {
    let request = file_request('a');
    empty_file_result(snapshot('a', "materializer-v1"))
        .validate_for_request(&request)
        .unwrap();

    let mismatch = empty_file_result(snapshot('a', "materializer-v2"));
    let error = mismatch.validate_for_request(&request).unwrap_err();
    assert_eq!(error.class, ErrorClass::Corrupt);
    assert_eq!(
        error.message,
        "blame result snapshot does not match the requested Core snapshot"
    );
}

#[test]
fn missing_or_malformed_result_snapshot_fails_deserialization() {
    let matching = empty_file_result(snapshot('a', "materializer-v1"));
    let mut missing = serde_json::to_value(&matching).unwrap();
    missing.as_object_mut().unwrap().remove("snapshot");
    assert!(serde_json::from_value::<BlameResult>(missing).is_err());

    let mut malformed = serde_json::to_value(matching).unwrap();
    malformed["snapshot"]["receipt"]["materializer_revision"] =
        serde_json::Value::String(String::new());
    assert!(serde_json::from_value::<BlameResult>(malformed).is_err());
}

#[test]
fn malformed_or_missing_citation_generation_fails_typed_response_validation() {
    let request = file_request('a');
    let malformed = file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64), "not-a-generation".to_owned()],
    );
    assert_eq!(
        malformed.validate_for_request(&request).unwrap_err().class,
        ErrorClass::Corrupt
    );

    let mut missing = serde_json::to_value(file_result(
        snapshot('a', "materializer-v1"),
        &["a".repeat(64)],
    ))
    .unwrap();
    missing["evidence"][0]["citation"]
        .as_object_mut()
        .unwrap()
        .remove("core_generation_id");
    assert!(serde_json::from_value::<BlameResult>(missing).is_err());
}
