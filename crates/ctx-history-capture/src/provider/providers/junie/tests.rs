use std::{
    fs::{self, File},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventType};
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, JsonlBatchProducer,
};
use crate::captured_batch::{
    CapturedBatch, CapturedBatchBuilder, CapturedRecord, NativeLocator, NativePosition,
    ProviderRecordKind, SourceObservation,
};
use crate::complete_content::jsonl::JsonlCompleteContentResolver;
use crate::complete_content::{
    AuthorizedSourceRoute, CompleteContentErrorKind, CompleteContentHashAuthority,
    CompleteContentResolver, CompleteContentSourceFamily, CompleteMessageRequest,
    ResultContentRequest, ResultContentResolver, SourceAccessBroker, SourceSnapshot,
    VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchProjector, CertifiedProviderCursor,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::test_support_paths::tempdir;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderNormalizationResult, JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
};

use super::{
    capture::JunieCapturedBatchProducer,
    checkpoint::{junie_parser_state_is_bounded, JunieCheckpointFailure, JunieParserCheckpoint},
    import_junie_session_events_batched,
    projector::JunieCapturedBatchProjector,
    session_tree::{visit_junie_session_event_paths, JunieIndexMeta, JunieSessionPath},
    JUNIE_CAPTURE_REVISION, JUNIE_END_RECORD_KIND, JUNIE_POLICY_REVISION, JUNIE_RECORD_KIND,
    MAX_JUNIE_CHECKPOINT_FAILURES, MAX_JUNIE_FAILURE_BYTES, MAX_JUNIE_INDEX_BYTES,
    MAX_JUNIE_INDEX_ENTRIES, MAX_JUNIE_PARSER_STATE_BYTES, MAX_JUNIE_TRANSIENT_TURN_BYTES,
};

#[test]
fn synthetic_end_is_the_only_source_exhausted_batch() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let bytes = b"{}\n";
    fs::write(&path, bytes).unwrap();
    let source = test_source();
    let inner = JsonlBatchProducer::new(
        BufReader::new(File::open(&path).unwrap()),
        source.clone(),
        path.display().to_string().into_bytes(),
        ProviderRecordKind::new(JUNIE_RECORD_KIND).unwrap(),
        bytes.len() as u64,
        0,
        0,
        false,
    )
    .unwrap();
    let mut producer = JunieCapturedBatchProducer {
        inner,
        source,
        source_item: path.display().to_string().into_bytes(),
        end_record_kind: ProviderRecordKind::new(JUNIE_END_RECORD_KIND).unwrap(),
        current_position: initial_jsonl_position().unwrap(),
        next_ordinal: 0,
        emit_end_record: true,
    };

    let content = producer.next_batch().unwrap().unwrap();
    assert!(!content.source_exhausted());
    let end = producer.next_batch().unwrap().unwrap();
    assert!(end.source_exhausted());
    assert_eq!(
        end.records()[0].record_kind().as_str(),
        JUNIE_END_RECORD_KIND
    );
    assert!(producer.next_batch().unwrap().is_none());
}

#[derive(Default)]
struct CanonicalProjectionOutput {
    units: Vec<String>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CanonicalProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        assert!(normalization.captures.len() <= 1);
        assert!(normalization.files_touched.len() <= 1);
        self.units.push(
            serde_json::to_string(&(normalization.captures, normalization.files_touched)).unwrap(),
        );
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

#[derive(Default)]
struct CountingProjectionOutput {
    emissions: usize,
    captures: usize,
    file_touches: usize,
    max_captures_per_emission: usize,
    max_file_touches_per_emission: usize,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CountingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.max_captures_per_emission = self
            .max_captures_per_emission
            .max(normalization.captures.len());
        self.max_file_touches_per_emission = self
            .max_file_touches_per_emission
            .max(normalization.files_touched.len());
        assert!(normalization.captures.len() <= 1);
        assert!(normalization.files_touched.len() <= 1);
        self.emissions = self.emissions.saturating_add(1);
        self.captures = self.captures.saturating_add(normalization.captures.len());
        self.file_touches = self
            .file_touches
            .saturating_add(normalization.files_touched.len());
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn test_session_path() -> JunieSessionPath {
    JunieSessionPath {
        events_path: PathBuf::from("/tmp/session-junie-streaming/events.jsonl"),
        index_meta: JunieIndexMeta {
            session_id: "session-junie-streaming".to_owned(),
            created_at: Some(1_783_339_200_000),
            updated_at: Some(1_783_339_800_000),
            task_name: Some("Junie streaming test".to_owned()),
            project_dir: Some("/workspace/junie-streaming".to_owned()),
            raw: json!({"sessionId": "session-junie-streaming"}),
        },
        require_supported_events: true,
    }
}

fn test_context() -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "junie-streaming-test-machine".to_owned(),
        source_path: Some(PathBuf::from("/tmp/session-junie-streaming/events.jsonl")),
        source_root: Some(PathBuf::from("/workspace/junie-streaming")),
        imported_at: DateTime::<Utc>::from_timestamp_millis(1_783_339_200_000).unwrap(),
    }
}

fn test_projector() -> JunieCapturedBatchProjector {
    JunieCapturedBatchProjector::fresh(&test_session_path(), test_context(), 0, 7).unwrap()
}

fn test_source() -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        "junie-streaming-test-source",
        "junie-streaming-test-revision",
        "junie-streaming-test-cursor",
        JUNIE_CAPTURE_REVISION,
        JUNIE_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_position(ordinal: u64) -> NativePosition {
    NativePosition::new("junie-test-position-v1", ordinal.to_be_bytes().to_vec()).unwrap()
}

fn test_batch(
    source: &SourceObservation,
    before: u64,
    record: CapturedRecord,
    after: u64,
) -> CapturedBatch {
    let mut builder = CapturedBatchBuilder::new(source.clone(), test_position(before));
    builder.push(record).unwrap();
    builder.finish(test_position(after)).unwrap()
}

fn test_record(ordinal: u64, value: Value) -> CapturedRecord {
    CapturedRecord::content(
        ordinal,
        NativeLocator::new("junie-test-line-v1", ordinal.to_string().into_bytes()).unwrap(),
        ProviderRecordKind::new(JUNIE_RECORD_KIND).unwrap(),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap()
}

fn test_end_record(ordinal: u64) -> CapturedRecord {
    CapturedRecord::content(
        ordinal,
        NativeLocator::new("junie-test-end-v1", b"end".to_vec()).unwrap(),
        ProviderRecordKind::new(JUNIE_END_RECORD_KIND).unwrap(),
        Vec::new(),
    )
    .unwrap()
}

fn representative_records() -> Vec<CapturedRecord> {
    vec![
        test_record(
            0,
            json!({
                "kind": "UserPromptEvent",
                "prompt": "JUNIE_BOUNDARY_USER exact"
            }),
        ),
        test_record(
            1,
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_320_000_i64,
                "event": {"agentEvent": {
                    "kind": "LlmResponseMetadataEvent",
                    "modelUsage": [{
                        "model": "provider/model-exact",
                        "inputTokens": 17,
                        "outputTokens": 23,
                        "cacheInputTokens": 3,
                        "cacheCreateTokens": 5
                    }]
                }}
            }),
        ),
        test_record(
            2,
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_380_000_i64,
                "event": {"agentEvent": {
                    "kind": "TerminalBlockUpdatedEvent",
                    "stepId": "terminal-boundary",
                    "command": "printf JUNIE_BOUNDARY_COMMAND",
                    "details": "JUNIE_BOUNDARY_DETAILS exact",
                    "status": "COMPLETED"
                }}
            }),
        ),
        test_record(
            3,
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_440_000_i64,
                "event": {"agentEvent": {
                    "kind": "FileChangesBlockUpdatedEvent",
                    "stepId": "edit-boundary",
                    "changes": [{
                        "beforeRelativePath": "src/before.rs",
                        "afterRelativePath": "src/after.rs",
                        "beforeContent": {"text": "JUNIE_BOUNDARY_OLD exact"},
                        "afterContent": {"text": "JUNIE_BOUNDARY_NEW exact"}
                    }],
                    "status": "COMPLETED"
                }}
            }),
        ),
        test_record(
            4,
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_500_000_i64,
                "event": {"agentEvent": {
                    "kind": "ResultBlockUpdatedEvent",
                    "stepId": "result-boundary",
                    "result": "JUNIE_BOUNDARY_RESULT exact"
                }}
            }),
        ),
        test_record(
            5,
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_500_001_i64,
                "event": {"agentEvent": {
                    "kind": "ResultBlockUpdatedEvent",
                    "stepId": "result-boundary-second",
                    "result": "JUNIE_BOUNDARY_RESULT_SECOND exact"
                }}
            }),
        ),
        test_end_record(6),
    ]
}

#[test]
fn junie_transient_turn_streams_exact_coalesced_units_with_content_free_checkpoint() {
    let records = representative_records();
    let mut projector = test_projector();
    let mut output = CanonicalProjectionOutput::default();
    for record in &records[..3] {
        projector.project_record(record, &mut output).unwrap();
    }
    assert!(projector.buffer.open);
    assert_eq!(output.units.len(), 1);
    let checkpoint = BoundedParserCheckpoint::from_serializable(&projector.state).unwrap();
    let serialized_checkpoint = String::from_utf8(checkpoint.as_bytes().to_vec()).unwrap();
    assert!(!serialized_checkpoint.contains("JUNIE_BOUNDARY_COMMAND"));
    assert!(!serialized_checkpoint.contains("JUNIE_BOUNDARY_DETAILS"));
    assert!(!serialized_checkpoint.contains("Junie streaming test"));
    assert!(!serialized_checkpoint.contains("/workspace/junie-streaming"));
    assert!(!serialized_checkpoint.contains("provider/model-exact"));
    for record in &records[3..] {
        projector.project_record(record, &mut output).unwrap();
    }

    assert_eq!(output.rejections, Vec::<(usize, String)>::new());
    assert_eq!(output.units.len(), 5);
    assert!(!projector.buffer.open);
    let rendered = output.units.join("\n");
    for marker in [
        "JUNIE_BOUNDARY_COMMAND",
        "src/before.rs",
        "src/after.rs",
        "JUNIE_BOUNDARY_RESULT",
        "JUNIE_BOUNDARY_RESULT_SECOND",
        "provider/model-exact",
    ] {
        assert!(rendered.contains(marker), "missing exact marker {marker}");
    }
    assert!(!rendered.contains("JUNIE_BOUNDARY_DETAILS"));
    assert!(!rendered.contains("JUNIE_BOUNDARY_OLD"));
    assert!(!rendered.contains("JUNIE_BOUNDARY_NEW"));
}

#[test]
fn junie_forced_batch_boundary_retains_prior_until_turn_closes() {
    let source = test_source();
    let mut projector = test_projector();
    let initial = projector
        .initial_cursor_candidate(&source, &test_position(0))
        .unwrap();
    assert_eq!(initial.native_position(), &test_position(0));

    let terminal = test_record(
        0,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_380_000_i64,
            "event": {"agentEvent": {
                "kind": "TerminalBlockUpdatedEvent",
                "stepId": "terminal-forced-boundary",
                "command": "printf JUNIE_SAFE_FRONTIER_COMMAND",
                "details": "JUNIE_SAFE_FRONTIER_DETAILS",
                "status": "COMPLETED"
            }}
        }),
    );
    let first_batch = test_batch(&source, 0, terminal, 1);
    let mut output = CanonicalProjectionOutput::default();
    projector
        .project_record(&first_batch.records()[0], &mut output)
        .unwrap();
    assert!(matches!(
        projector.finish_cursor(&first_batch).unwrap(),
        CapturedBatchCursorFinish::RetainPrior
    ));
    assert!(output.units.is_empty());

    let user = test_record(
        1,
        json!({
            "kind": "UserPromptEvent",
            "prompt": "JUNIE_SAFE_FRONTIER_NEXT_USER"
        }),
    );
    let second_batch = test_batch(&source, 1, user, 2);
    projector
        .project_record(&second_batch.records()[0], &mut output)
        .unwrap();
    let advanced = match projector.finish_cursor(&second_batch).unwrap() {
        CapturedBatchCursorFinish::Advance(cursor) => cursor,
        CapturedBatchCursorFinish::RetainPrior => panic!("closed Junie turn stayed unsafe"),
    };
    assert_eq!(advanced.native_position(), &test_position(2));
    let checkpoint = String::from_utf8(advanced.parser_checkpoint().as_bytes().to_vec()).unwrap();
    assert!(!checkpoint.contains("JUNIE_SAFE_FRONTIER_COMMAND"));
    assert!(!checkpoint.contains("JUNIE_SAFE_FRONTIER_DETAILS"));
    assert_eq!(output.units.len(), 3);
    let rendered = output.units.join("\n");
    assert!(rendered.contains("JUNIE_SAFE_FRONTIER_COMMAND"));
    assert!(!rendered.contains("JUNIE_SAFE_FRONTIER_DETAILS"));
    assert!(rendered.contains("JUNIE_SAFE_FRONTIER_NEXT_USER"));
}

#[test]
fn junie_large_steps_and_changes_stream_without_contentful_checkpoint_growth() {
    const STEP_COUNT: usize = 512;
    const CHANGE_COUNT: usize = 2_048;

    let mut projector = test_projector();
    let mut output = CountingProjectionOutput::default();
    for step_index in 0..STEP_COUNT {
        let record = test_record(
            step_index as u64,
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_320_000_i64 + step_index as i64,
                "event": {"agentEvent": {
                    "kind": "TerminalBlockUpdatedEvent",
                    "stepId": format!("step-{step_index}"),
                    "command": format!("JUNIE_LARGE_COMMAND_{step_index}"),
                    "details": format!("JUNIE_LARGE_DETAILS_{step_index}"),
                    "status": "COMPLETED"
                }}
            }),
        );
        projector.project_record(&record, &mut output).unwrap();
    }

    let changes = (0..CHANGE_COUNT)
        .map(|change_index| {
            json!({
                "beforeRelativePath": format!("src/JUNIE_LARGE_BEFORE_{change_index}.rs"),
                "afterRelativePath": format!("src/JUNIE_LARGE_AFTER_{change_index}.rs"),
                "beforeContent": {"text": format!("JUNIE_LARGE_OLD_{change_index}")},
                "afterContent": {"text": format!("JUNIE_LARGE_NEW_{change_index}")}
            })
        })
        .collect::<Vec<_>>();
    let change_record = test_record(
        STEP_COUNT as u64,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_500_000_i64,
            "event": {"agentEvent": {
                "kind": "FileChangesBlockUpdatedEvent",
                "stepId": "large-file-change-step",
                "changes": changes,
                "status": "COMPLETED"
            }}
        }),
    );
    projector
        .project_record(&change_record, &mut output)
        .unwrap();
    let result_record = test_record(
        STEP_COUNT as u64 + 1,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_560_000_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "large-result",
                "result": "JUNIE_LARGE_RESULT_CONTENT"
            }}
        }),
    );
    projector
        .project_record(&result_record, &mut output)
        .unwrap();

    assert_eq!(output.rejections, Vec::<(usize, String)>::new());
    assert_eq!(output.emissions, 0);
    assert!(projector.buffer.open);
    assert!(projector.buffer.retained_source_bytes <= MAX_JUNIE_TRANSIENT_TURN_BYTES);
    let end_record = test_end_record(STEP_COUNT as u64 + 2);
    projector.project_record(&end_record, &mut output).unwrap();

    assert_eq!(output.max_captures_per_emission, 1);
    assert_eq!(output.max_file_touches_per_emission, 1);
    assert_eq!(output.captures, STEP_COUNT * 2 + CHANGE_COUNT + 1);
    assert_eq!(output.file_touches, CHANGE_COUNT);
    assert_eq!(output.emissions, output.captures);
    assert!(!projector.buffer.open);
    projector.state.failures = (0..MAX_JUNIE_CHECKPOINT_FAILURES)
        .map(|line| JunieCheckpointFailure {
            line,
            error: "bounded diagnostic".repeat(MAX_JUNIE_FAILURE_BYTES / 18),
        })
        .collect();
    projector.state.rejected_records = MAX_JUNIE_CHECKPOINT_FAILURES as u64;
    assert!(junie_parser_state_is_bounded(&projector.state));
    let checkpoint = BoundedParserCheckpoint::from_serializable(&projector.state).unwrap();
    assert!(checkpoint.as_bytes().len() <= MAX_JUNIE_PARSER_STATE_BYTES);
    let serialized = String::from_utf8(checkpoint.as_bytes().to_vec()).unwrap();
    for forbidden in [
        "JUNIE_LARGE_COMMAND",
        "JUNIE_LARGE_DETAILS",
        "JUNIE_LARGE_BEFORE",
        "JUNIE_LARGE_AFTER",
        "JUNIE_LARGE_OLD",
        "JUNIE_LARGE_NEW",
        "JUNIE_LARGE_RESULT_CONTENT",
        "\"title\"",
        "\"cwd\"",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "checkpoint retained forbidden content {forbidden}"
        );
    }
}

#[test]
fn junie_over_limit_transient_turn_is_deterministically_cleared() {
    let mut projector = test_projector();
    let buffered = json!({
        "kind": "SessionA2uxEvent",
        "timestampMs": 1_783_339_320_000_i64,
        "event": {"agentEvent": {
            "kind": "ResultBlockUpdatedEvent",
            "stepId": "bounded-result",
            "result": "JUNIE_TRANSIENT_BOUND_MARKER"
        }}
    });
    let half_plus_one = MAX_JUNIE_TRANSIENT_TURN_BYTES / 2 + 1;
    assert!(projector.project_session_event(&buffered, half_plus_one, None, None));
    assert!(projector.buffer.open);
    assert!(!projector.project_session_event(&buffered, half_plus_one, None, None));
    assert!(!projector.buffer.open);
    assert_eq!(projector.buffer.retained_source_bytes, 0);
    assert!(projector.buffer.results.is_empty());
}

fn push_jsonl(contents: &mut String, value: Value) {
    contents.push_str(&serde_json::to_string(&value).unwrap());
    contents.push('\n');
}

#[derive(Debug, PartialEq, Eq)]
struct VisitedJunieSession {
    ordinal: usize,
    session_id: String,
    created_at: Option<i64>,
    updated_at: Option<i64>,
    task_name: Option<String>,
    project_dir: Option<String>,
    require_supported_events: bool,
    events_path: PathBuf,
}

fn write_junie_tree_session(root: &Path, session_id: &str) {
    let session = root.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(session.join("events.jsonl"), "{}\n").unwrap();
}

fn visit_junie_tree(root: &Path) -> (crate::Result<usize>, Vec<VisitedJunieSession>) {
    let mut visited = Vec::new();
    let result = visit_junie_session_event_paths(root, &mut |session, ordinal| {
        visited.push(VisitedJunieSession {
            ordinal,
            session_id: session.index_meta.session_id,
            created_at: session.index_meta.created_at,
            updated_at: session.index_meta.updated_at,
            task_name: session.index_meta.task_name,
            project_dir: session.index_meta.project_dir,
            require_supported_events: session.require_supported_events,
            events_path: session.events_path,
        });
        Ok(())
    });
    (result, visited)
}

fn assert_junie_index_limit(error: CaptureError, expected: &str) {
    match error {
        CaptureError::InvalidPayload(message) => assert_eq!(message, expected),
        other => panic!("expected invalid Junie index payload, got {other:?}"),
    }
}

#[test]
fn junie_index_duplicates_are_first_wins_in_source_order() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_junie_tree_session(&root, "duplicate");
    write_junie_tree_session(&root, "middle");

    let mut index = String::new();
    for value in [
        json!({"sessionId": "duplicate", "taskName": "first metadata"}),
        json!({"sessionId": "middle", "taskName": "middle metadata"}),
        json!({"sessionId": "duplicate", "taskName": "later metadata"}),
    ] {
        push_jsonl(&mut index, value);
    }
    fs::write(root.join("index.jsonl"), index).unwrap();

    let (result, visited) = visit_junie_tree(&root);
    assert_eq!(result.unwrap(), 2);
    assert_eq!(
        visited
            .iter()
            .map(|session| (
                session.session_id.as_str(),
                session.task_name.as_deref(),
                session.ordinal,
            ))
            .collect::<Vec<_>>(),
        vec![
            ("duplicate", Some("first metadata"), 0),
            ("middle", Some("middle metadata"), 1),
        ]
    );
}

#[test]
fn junie_index_and_orphan_order_survive_adversarial_directory_order() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    for session_id in ["orphan-z", "indexed-a", "orphan-b", "indexed-z"] {
        write_junie_tree_session(&root, session_id);
    }
    fs::create_dir_all(root.join("indexed-without-events")).unwrap();

    let mut index = String::new();
    push_jsonl(
        &mut index,
        json!({"sessionId": "indexed-z", "taskName": "z first"}),
    );
    index.push_str("not json\n");
    push_jsonl(
        &mut index,
        json!({"sessionId": "indexed-a", "taskName": "a second"}),
    );
    push_jsonl(&mut index, json!({"sessionId": "indexed-without-events"}));
    fs::write(root.join("index.jsonl"), index).unwrap();

    let (result, visited) = visit_junie_tree(&root);
    assert_eq!(result.unwrap(), 4);
    assert_eq!(
        visited
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["indexed-z", "indexed-a", "orphan-b", "orphan-z"]
    );
    assert!(visited[..2]
        .iter()
        .all(|session| session.require_supported_events));
    assert!(visited[2..]
        .iter()
        .all(|session| !session.require_supported_events));
}

#[test]
fn junie_index_entry_ceiling_is_inclusive_and_rejects_before_visiting() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_junie_tree_session(&root, "first");
    let mut index = format!("{}\n", json!({"sessionId": "first"}));
    index.push_str(&"{}\n".repeat(MAX_JUNIE_INDEX_ENTRIES - 1));
    let index_path = root.join("index.jsonl");
    fs::write(&index_path, index).unwrap();

    let (at_limit, visited) = visit_junie_tree(&root);
    assert_eq!(at_limit.unwrap(), 1);
    assert_eq!(visited.len(), 1);

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&index_path)
        .unwrap();
    file.write_all(b"{}\n").unwrap();
    file.sync_all().unwrap();
    drop(file);

    let (over_limit, visited) = visit_junie_tree(&root);
    assert!(visited.is_empty());
    assert_junie_index_limit(
        over_limit.unwrap_err(),
        &format!("Junie index exceeds the {MAX_JUNIE_INDEX_ENTRIES} entry limit"),
    );
}

#[test]
fn junie_index_byte_ceiling_is_inclusive_and_rejects_before_visiting() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_junie_tree_session(&root, "first");
    let mut index = serde_json::to_vec(&json!({"sessionId": "first"})).unwrap();
    assert!(index.len() < MAX_JUNIE_INDEX_BYTES);
    index.resize(MAX_JUNIE_INDEX_BYTES - 1, b' ');
    index.push(b'\n');
    let index_path = root.join("index.jsonl");
    fs::write(&index_path, index).unwrap();

    let (at_limit, visited) = visit_junie_tree(&root);
    assert_eq!(at_limit.unwrap(), 1);
    assert_eq!(visited.len(), 1);

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&index_path)
        .unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    drop(file);

    let (over_limit, visited) = visit_junie_tree(&root);
    assert!(visited.is_empty());
    assert_junie_index_limit(
        over_limit.unwrap_err(),
        &format!("Junie index exceeds the {MAX_JUNIE_INDEX_BYTES} byte limit"),
    );
}

#[test]
fn junie_valid_fixture_session_tree_has_identical_metadata_and_order() {
    const INDEX: &[u8] = include_bytes!(
        "../../../../../../tests/fixtures/provider-history/junie/sessions/index.jsonl"
    );
    const EVENTS: &[u8] = include_bytes!(
        "../../../../../../tests/fixtures/provider-history/junie/sessions/session-260607-100000-acme/events.jsonl"
    );

    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-260607-100000-acme");
    fs::create_dir_all(&session).unwrap();
    fs::write(root.join("index.jsonl"), INDEX).unwrap();
    fs::write(session.join("events.jsonl"), EVENTS).unwrap();
    let (result, visited) = visit_junie_tree(&root);

    assert_eq!(result.unwrap(), 1);
    assert_eq!(
        visited,
        vec![VisitedJunieSession {
            ordinal: 0,
            session_id: "session-260607-100000-acme".to_owned(),
            created_at: Some(1_783_339_200_000),
            updated_at: Some(1_783_339_440_000),
            task_name: Some("Junie fixture task".to_owned()),
            project_dir: Some("/workspace/junie-fixture".to_owned()),
            require_supported_events: true,
            events_path: root.join("session-260607-100000-acme").join("events.jsonl"),
        }]
    );
}

#[test]
fn junie_record_set_locators_reopen_exact_buffered_content_and_fail_closed() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_id = "session-junie-complete-content";
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": session_id,
                "createdAt": 1_783_339_200_000_i64,
                "updatedAt": 1_783_339_800_000_i64,
            })
        ),
    )
    .unwrap();
    let long_user = "JUNIE_LONG_USER snowman ☃ quoted \" body\n".repeat(600);
    let long_assistant = "JUNIE_LONG_ASSISTANT cedar compass\n".repeat(600);
    let exact_output = "JUNIE_EXACT_TOOL_OUTPUT saffron harbor";
    let mut source = String::new();
    push_jsonl(
        &mut source,
        json!({"kind": "UserPromptEvent", "prompt": long_user}),
    );
    push_jsonl(
        &mut source,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_260_000_i64,
            "event": {"agentEvent": {
                "kind": "LlmResponseMetadataEvent",
                "modelUsage": [{"model": "provider/model", "inputTokens": 1, "outputTokens": 2}]
            }}
        }),
    );
    push_jsonl(
        &mut source,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_320_000_i64,
            "event": {"agentEvent": {
                "kind": "TerminalBlockUpdatedEvent",
                "stepId": "terminal-exact",
                "command": "printf exact",
                "details": exact_output,
                "status": "COMPLETED"
            }}
        }),
    );
    push_jsonl(
        &mut source,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_380_000_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "assistant-exact",
                "result": long_assistant
            }}
        }),
    );
    push_jsonl(
        &mut source,
        json!({"kind": "UserPromptEvent", "prompt": "close the buffered turn"}),
    );
    let events_path = session_dir.join("events.jsonl");
    fs::write(&events_path, source.as_bytes()).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_junie_session_events_batched(
        &root,
        &mut store,
        ProviderAdapterContext {
            machine_id: "junie-complete-content-machine".to_owned(),
            source_path: Some(root.clone()),
            source_root: Some(root.clone()),
            imported_at: "2026-07-22T12:00:00Z".parse().unwrap(),
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some(session_id))
        .unwrap();
    let capture_source = store
        .get_capture_source(session.capture_source_id.unwrap())
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let output = events
        .iter()
        .find(|event| event.event_type == EventType::CommandOutput)
        .unwrap();
    let assistant = events
        .iter()
        .find(|event| {
            event.event_type == EventType::Message
                && event.payload["body"]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("JUNIE_LONG_ASSISTANT"))
        })
        .unwrap();
    let user = events
        .iter()
        .find(|event| {
            event.event_type == EventType::Message
                && event.payload["body"]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("JUNIE_LONG_USER"))
        })
        .unwrap();
    let output_locators = VerifiedContentLocatorsV1::from_metadata_value(
        &output.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let output_locator = output_locators
        .locator(VerifiedContentRole::ResultBody)
        .unwrap();
    let assistant_locators = VerifiedContentLocatorsV1::from_metadata_value(
        &assistant.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let assistant_locator = assistant_locators
        .locator(VerifiedContentRole::MessageBody)
        .unwrap();
    let user_locators = VerifiedContentLocatorsV1::from_metadata_value(
        &user.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let user_locator = user_locators
        .locator(VerifiedContentRole::MessageBody)
        .unwrap();
    let rendered_locators = format!(
        "{}{}{}",
        output_locators.to_metadata_value(),
        assistant_locators.to_metadata_value(),
        user_locators.to_metadata_value()
    );
    assert!(!rendered_locators.contains(exact_output));
    assert!(!rendered_locators.contains("JUNIE_LONG_ASSISTANT"));
    assert!(!rendered_locators.contains("JUNIE_LONG_USER"));
    assert!(!rendered_locators.contains(events_path.to_string_lossy().as_ref()));

    let source_snapshot = SourceSnapshot {
        size_bytes: Some(source.len() as u64),
        modified_at_ms: None,
        sha256: None,
    };
    let admit_source = |event_id, snapshot: SourceSnapshot| {
        SourceAccessBroker::new()
            .admit(
                AuthorizedSourceRoute {
                    source_id: uuid::Uuid::new_v4(),
                    provider: CaptureProvider::Junie,
                    source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
                    family: CompleteContentSourceFamily::Jsonl,
                    raw_source_path: events_path.clone(),
                    source_root: Some(root.clone()),
                    source_identity: capture_source.descriptor.source_identity.clone(),
                    source_snapshot: snapshot,
                },
                event_id,
            )
            .unwrap()
    };
    let coordinate = |event: &ctx_history_core::Event| {
        (
            event.sync.metadata["source_record_ordinal"]
                .as_u64()
                .unwrap(),
            event.sync.metadata["source_record_subrecord_index"]
                .as_u64()
                .unwrap() as u32,
        )
    };
    let (output_ordinal, output_subrecord) = coordinate(output);
    let mut result_request = ResultContentRequest {
        event_id: output.id,
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
        source_access: admit_source(output.id, source_snapshot.clone()),
        source_family: CompleteContentSourceFamily::Jsonl,
        content_profile: output_locator.content_profile().to_owned(),
        source_locator: output_locator.source_locator().unwrap(),
        source_record_ordinal: output_ordinal,
        source_record_subrecord_index: output_subrecord,
        expected_native_record_id: output_locator.native_record_id().to_owned(),
        expected_record_digest: output_locator.record_sha256().clone(),
        expected_content_ref: output_locator.content_ref().clone(),
    };
    assert_eq!(
        output_locator.content_ref(),
        &ContentRef::from_bytes(exact_output.as_bytes()).unwrap()
    );
    let persisted_result_ref =
        serde_json::from_value::<ContentRef>(output.payload["body"]["result_content_ref"].clone())
            .unwrap();
    assert_eq!(&persisted_result_ref, output_locator.content_ref());
    let (assistant_ordinal, assistant_subrecord) = coordinate(assistant);
    let message_request = CompleteMessageRequest {
        event_id: assistant.id,
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
        source_access: admit_source(assistant.id, source_snapshot.clone()),
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: assistant_locator.content_profile().to_owned(),
        source_locator: assistant_locator.source_locator(),
        provider_session_id: Some(session_id.to_owned()),
        source_record_ordinal: assistant_ordinal,
        source_record_subrecord_index: assistant_subrecord,
        expected_provider_event_hash: assistant.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(assistant_locator.native_record_id().to_owned()),
        expected_record_digest: Some(assistant_locator.record_sha256().clone()),
        expected_content_ref: Some(assistant_locator.content_ref().clone()),
        indexed_text: assistant.payload["body"]["text"]
            .as_str()
            .unwrap()
            .to_owned(),
        indexed_limit_chars: crate::PROVIDER_MAX_TEXT_CHARS,
    };
    let (user_ordinal, user_subrecord) = coordinate(user);
    let user_request = CompleteMessageRequest {
        event_id: user.id,
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
        source_access: admit_source(user.id, source_snapshot.clone()),
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: user_locator.content_profile().to_owned(),
        source_locator: user_locator.source_locator(),
        provider_session_id: Some(session_id.to_owned()),
        source_record_ordinal: user_ordinal,
        source_record_subrecord_index: user_subrecord,
        expected_provider_event_hash: user.sync.metadata["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(user_locator.native_record_id().to_owned()),
        expected_record_digest: Some(user_locator.record_sha256().clone()),
        expected_content_ref: Some(user_locator.content_ref().clone()),
        indexed_text: user.payload["body"]["text"].as_str().unwrap().to_owned(),
        indexed_limit_chars: crate::PROVIDER_MAX_TEXT_CHARS,
    };

    let resolver = JsonlCompleteContentResolver::new();
    let resolved_result =
        ResultContentResolver::resolve_results(&resolver, std::slice::from_ref(&result_request));
    assert_eq!(resolved_result[0].as_ref().unwrap().content, exact_output);
    let resolved_message =
        CompleteContentResolver::resolve(&resolver, std::slice::from_ref(&message_request))
            .unwrap();
    assert_eq!(resolved_message[0].text, long_assistant);
    let resolved_user = CompleteContentResolver::resolve(&resolver, &[user_request]).unwrap();
    assert_eq!(resolved_user[0].text, long_user);

    let mut appended = fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .unwrap();
    writeln!(appended, "{}", json!({"kind": "IgnoredAfterAddress"})).unwrap();
    appended.sync_all().unwrap();
    drop(appended);
    result_request.source_access = admit_source(result_request.event_id, source_snapshot.clone());
    assert_eq!(
        ResultContentResolver::resolve_results(&resolver, std::slice::from_ref(&result_request))[0]
            .as_ref()
            .unwrap()
            .content,
        exact_output
    );

    let mut wrong_id = result_request.clone();
    wrong_id.expected_native_record_id = "wrong-native-id".to_owned();
    assert_eq!(
        ResultContentResolver::resolve_results(&resolver, &[wrong_id])[0]
            .as_ref()
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
    let mut wrong_order = result_request.clone();
    let source_locator = wrong_order.source_locator.clone();
    let mut encoded = source_locator.value().to_vec();
    assert!(encoded.len() >= 7 + 48);
    let (first, rest) = encoded[7..].split_at_mut(24);
    first.swap_with_slice(&mut rest[..24]);
    wrong_order.source_locator =
        crate::complete_content::CompleteContentSourceLocator::new(source_locator.kind(), encoded)
            .unwrap();
    assert_eq!(
        ResultContentResolver::resolve_results(&resolver, &[wrong_order])[0]
            .as_ref()
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::HydrationUnsupported
    );

    let appended_source = fs::read_to_string(&events_path).unwrap();
    let rewritten = appended_source.replacen("saffron", "Saffron", 1);
    assert_eq!(rewritten.len(), appended_source.len());
    fs::write(&events_path, rewritten).unwrap();
    assert_eq!(
        ResultContentResolver::resolve_results(&resolver, std::slice::from_ref(&result_request))[0]
            .as_ref()
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceChanged
    );
    fs::write(&events_path, b"{}\n").unwrap();
    assert_eq!(
        CompleteContentResolver::resolve(&resolver, &[message_request])
            .unwrap_err()
            .kind,
        CompleteContentErrorKind::SourceChanged
    );
}

fn junie_partition_fixture(root: &Path) -> PathBuf {
    let session_id = "session-junie-safe-frontier";
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": session_id,
                "createdAt": 1_783_339_200_000_i64,
                "updatedAt": 1_783_339_800_000_i64,
                "taskName": "JUNIE_INDEX_TITLE_BEFORE_DYNAMIC_UPDATE"
            })
        ),
    )
    .unwrap();

    let mut contents = String::new();
    push_jsonl(
        &mut contents,
        json!({
            "kind": "UserPromptEvent",
            "prompt": "JUNIE_PARTITION_INITIAL_USER"
        }),
    );
    push_jsonl(
        &mut contents,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_260_000_i64,
            "event": {"agentEvent": {
                "kind": "AgentTaskNameUpdatedEvent",
                "name": "JUNIE_DYNAMIC_TITLE_AFTER_INDEX"
            }}
        }),
    );
    push_jsonl(
        &mut contents,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_260_001_i64,
            "event": {"agentEvent": {
                "kind": "CurrentDirectoryUpdatedEvent",
                "currentDirectory": "/workspace/JUNIE_DYNAMIC_CWD_FROM_EVENTS"
            }}
        }),
    );
    for update in 0..255 {
        push_jsonl(
            &mut contents,
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1_783_339_320_000_i64 + update,
                "event": {"agentEvent": {
                    "kind": "TerminalBlockUpdatedEvent",
                    "stepId": "partition-terminal-step",
                    "command": format!("printf JUNIE_PARTITION_COMMAND_{update}"),
                    "details": format!("JUNIE_PARTITION_DETAILS_{update}"),
                    "status": "COMPLETED"
                }}
            }),
        );
    }
    push_jsonl(
        &mut contents,
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_640_000_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "partition-result",
                "result": "JUNIE_PARTITION_INITIAL_RESULT"
            }}
        }),
    );
    push_jsonl(
        &mut contents,
        json!({
            "kind": "UserPromptEvent",
            "prompt": "JUNIE_PARTITION_CLOSING_USER"
        }),
    );
    let events = session_dir.join("events.jsonl");
    fs::write(&events, contents).unwrap();
    events
}

fn junie_partition_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "junie-safe-frontier-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: "2026-07-18T20:00:00Z".parse().unwrap(),
    }
}

fn certified_junie_cursor(
    store: &Store,
    events: &Path,
    machine_id: &str,
) -> CertifiedProviderCursor {
    let path_identity = provider_path_identity(events).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &path_identity,
    );
    let stored = store
        .get_sync_cursor(None, machine_id, &stream)
        .unwrap()
        .unwrap();
    CertifiedProviderCursor::decode(&stored.cursor).unwrap()
}

#[test]
fn junie_group_four_five_eof_noop_append_matches_one_shot_with_dynamic_metadata() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let events = junie_partition_fixture(&root);
    let context = junie_partition_context(&root);
    let options = NormalizedProviderImportOptions {
        fast_event_inserts: true,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
        ..NormalizedProviderImportOptions::default()
    };
    let mut resumed_store = Store::open(temp.path().join("resumed.sqlite")).unwrap();

    // The first 256 raw records end in the open assistant turn. Its final
    // update and close are in the fifth producer batch.
    let first = import_junie_session_events_batched(
        &root,
        &mut resumed_store,
        context.clone(),
        options.clone(),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.skipped_sessions, 0);
    assert_eq!(first.imported_events, 5);
    let initial_length = fs::metadata(&events).unwrap().len();
    let initial_cursor =
        certified_junie_cursor(&resumed_store, &events, "junie-safe-frontier-machine");
    assert_eq!(
        jsonl_position_offset(initial_cursor.native_position()).unwrap(),
        initial_length
    );
    let initial_checkpoint: JunieParserCheckpoint =
        initial_cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(initial_checkpoint.next_ordinal, 261);
    assert!(initial_checkpoint.source_ended);
    assert!(initial_checkpoint.title_anchor.is_some());
    assert!(initial_checkpoint.cwd_anchor.is_some());
    let checkpoint_text = String::from_utf8_lossy(initial_cursor.parser_checkpoint().as_bytes());
    for raw_metadata in [
        "JUNIE_DYNAMIC_TITLE_AFTER_INDEX",
        "/workspace/JUNIE_DYNAMIC_CWD_FROM_EVENTS",
        "JUNIE_PARTITION_COMMAND_254",
        "JUNIE_PARTITION_DETAILS_254",
        "JUNIE_PARTITION_INITIAL_RESULT",
    ] {
        assert!(!checkpoint_text.contains(raw_metadata));
    }

    let initial_source = resumed_store
        .capture_source_by_external_session(CaptureProvider::Junie, "session-junie-safe-frontier")
        .unwrap()
        .unwrap();
    assert_eq!(
        initial_source.descriptor.cwd.as_deref(),
        Some("/workspace/JUNIE_DYNAMIC_CWD_FROM_EVENTS")
    );
    assert_eq!(
        initial_source.sync.metadata["session_metadata"]["title"].as_str(),
        Some("JUNIE_DYNAMIC_TITLE_AFTER_INDEX")
    );
    let initial_session = resumed_store
        .session_by_external_session(CaptureProvider::Junie, "session-junie-safe-frontier")
        .unwrap()
        .unwrap();
    let initial_events = resumed_store
        .events_for_session(initial_session.id)
        .unwrap();
    let over_limit_output = initial_events
        .iter()
        .find(|event| event.event_type == EventType::CommandOutput)
        .unwrap();
    assert!(over_limit_output.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].is_null());
    assert!(over_limit_output.payload["body"]["result_content_ref"].is_null());

    let replay = import_junie_session_events_batched(
        &root,
        &mut resumed_store,
        context.clone(),
        options.clone(),
    )
    .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 5);
    let replay_cursor =
        certified_junie_cursor(&resumed_store, &events, "junie-safe-frontier-machine");
    assert_eq!(
        jsonl_position_offset(replay_cursor.native_position()).unwrap(),
        initial_length
    );

    let mut file = fs::OpenOptions::new().append(true).open(&events).unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_700_000_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "partition-appended-result",
                "result": "JUNIE_PARTITION_APPENDED_RESULT"
            }}
        })
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "kind": "UserPromptEvent",
            "prompt": "JUNIE_PARTITION_APPENDED_USER"
        })
    )
    .unwrap();
    file.sync_all().unwrap();
    drop(file);

    let appended = import_junie_session_events_batched(
        &root,
        &mut resumed_store,
        context.clone(),
        options.clone(),
    )
    .unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert_eq!(appended.imported_sessions, 0);
    assert_eq!(appended.skipped_sessions, 1);
    assert_eq!(appended.imported_events, 2);
    let appended_length = fs::metadata(&events).unwrap().len();
    let appended_cursor =
        certified_junie_cursor(&resumed_store, &events, "junie-safe-frontier-machine");
    assert_eq!(
        jsonl_position_offset(appended_cursor.native_position()).unwrap(),
        appended_length
    );
    let appended_checkpoint: JunieParserCheckpoint =
        appended_cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(appended_checkpoint.next_ordinal, 263);
    assert!(appended_checkpoint.source_ended);

    let resumed_session = resumed_store
        .session_by_external_session(CaptureProvider::Junie, "session-junie-safe-frontier")
        .unwrap()
        .unwrap();
    let resumed_source = resumed_store
        .capture_source_by_external_session(CaptureProvider::Junie, "session-junie-safe-frontier")
        .unwrap()
        .unwrap();
    let resumed_events = resumed_store
        .events_for_session(resumed_session.id)
        .unwrap();
    assert_eq!(resumed_events.len(), 7);

    let mut one_shot_store = Store::open(temp.path().join("one-shot.sqlite")).unwrap();
    let one_shot =
        import_junie_session_events_batched(&root, &mut one_shot_store, context, options).unwrap();
    assert_eq!(one_shot.failed, 0, "{:?}", one_shot.failures);
    assert_eq!(one_shot.imported_sessions, 1);
    assert_eq!(one_shot.skipped_sessions, 0);
    assert_eq!(one_shot.imported_events, 7);
    let one_shot_session = one_shot_store
        .session_by_external_session(CaptureProvider::Junie, "session-junie-safe-frontier")
        .unwrap()
        .unwrap();
    let one_shot_source = one_shot_store
        .capture_source_by_external_session(CaptureProvider::Junie, "session-junie-safe-frontier")
        .unwrap()
        .unwrap();
    let one_shot_events = one_shot_store
        .events_for_session(one_shot_session.id)
        .unwrap();
    let one_shot_cursor =
        certified_junie_cursor(&one_shot_store, &events, "junie-safe-frontier-machine");

    assert_eq!(resumed_session, one_shot_session);
    assert_eq!(resumed_source, one_shot_source);
    assert_eq!(resumed_events, one_shot_events);
    assert_eq!(
        appended_cursor.native_position(),
        one_shot_cursor.native_position()
    );
    let one_shot_checkpoint: JunieParserCheckpoint =
        one_shot_cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(one_shot_checkpoint.next_ordinal, 263);
    assert!(one_shot_checkpoint.source_ended);
}
