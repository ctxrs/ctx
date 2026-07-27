use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, ProviderCaptureEnvelope};
use ctx_history_store::Store;
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::captured_batch::jsonl::JsonlBatchProducer;
use crate::captured_batch::{CapturedBatch, ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchProjector, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::test_support_paths::tempdir;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderNormalizationResult, MAX_PROVIDER_JSONL_LINE_BYTES, MISTRAL_VIBE_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS,
};

use super::projector::{MistralVibeCapturedBatchProjector, MistralVibeParserCheckpoint};
use super::schema::mistral_vibe_bounded_metadata;
use super::source::{
    visit_mistral_vibe_session_sources, MistralVibeSessionObservation, MistralVibeSessionSource,
    MISTRAL_VIBE_MAX_DIRECTORY_DEPTH, MISTRAL_VIBE_MAX_TRAVERSAL_ENTRIES,
};
use super::{
    import_mistral_vibe_session_file_batched, MISTRAL_VIBE_CAPTURE_REVISION,
    MISTRAL_VIBE_POLICY_REVISION, MISTRAL_VIBE_RECORD_KIND,
};

fn write_session(message_count: usize) -> (TempDir, MistralVibeSessionSource, PathBuf) {
    let temp = tempdir().unwrap();
    let root = temp.path().join("vibe-root");
    let session_dir = root.join("session-1");
    fs::create_dir_all(&session_dir).unwrap();
    let metadata_path = session_dir.join("meta.json");
    let messages_path = session_dir.join("messages.jsonl");
    fs::write(
        &metadata_path,
        serde_json::to_vec(&json!({
            "session_id": "mistral-bounded-session",
            "start_time": "2026-07-17T12:00:00Z",
            "environment": { "working_directory": "/workspace/mistral" },
            "agent_profile": { "name": "vibe-agent" },
        }))
        .unwrap(),
    )
    .unwrap();
    let mut messages = String::new();
    for index in 0..message_count {
        messages.push_str(
            &serde_json::to_string(&json!({
                "message_id": format!("message-{index}"),
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": format!("bounded message {index}"),
                "timestamp": format!("2026-07-17T12:{:02}:00Z", index % 60),
            }))
            .unwrap(),
        );
        messages.push('\n');
    }
    fs::write(&messages_path, messages).unwrap();
    (
        temp,
        MistralVibeSessionSource {
            session_dir,
            metadata_path,
            messages_path,
        },
        root,
    )
}

fn context(source: &MistralVibeSessionSource, root: PathBuf) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "mistral-batch-machine".to_owned(),
        source_path: Some(source.messages_path.clone()),
        source_root: Some(root),
        imported_at: "2026-07-17T13:00:00Z".parse().unwrap(),
    }
}

fn write_traversal_session(session_dir: &Path) {
    fs::create_dir_all(session_dir).unwrap();
    fs::write(session_dir.join("meta.json"), b"{}").unwrap();
    fs::write(session_dir.join("messages.jsonl"), b"").unwrap();
}

fn nested_directory(root: &Path, depth: usize) -> PathBuf {
    let mut directory = root.to_path_buf();
    fs::create_dir_all(&directory).unwrap();
    for _ in 0..depth {
        directory.push("d");
        fs::create_dir(&directory).unwrap();
    }
    directory
}

fn write_child_directories(root: &Path, count: usize) {
    fs::create_dir_all(root).unwrap();
    for index in (0..count).rev() {
        fs::create_dir(root.join(format!("entry-{index:04}"))).unwrap();
    }
}

#[derive(Debug, Default, PartialEq)]
struct CollectingProjectionOutput {
    captures: Vec<(usize, ProviderCaptureEnvelope)>,
    files_touched: Vec<(usize, crate::ProviderFileTouchedEnvelope)>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CollectingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.captures.extend(normalization.captures);
        self.files_touched.extend(normalization.files_touched);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn captured_batches(source: &MistralVibeSessionSource) -> Vec<CapturedBatch> {
    let observation = MistralVibeSessionObservation::read(source).unwrap();
    let path_identity = provider_path_identity(&observation.canonical_messages_path).unwrap();
    let captured_source = SourceObservation::new(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        format!("mistral-vibe-session-file:{path_identity}"),
        observation.source_revision(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::MistralVibe,
            MISTRAL_VIBE_SOURCE_FORMAT,
            &path_identity,
        ),
        MISTRAL_VIBE_CAPTURE_REVISION,
        MISTRAL_VIBE_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(File::open(&source.messages_path).unwrap()),
        captured_source,
        path_identity.into_bytes(),
        ProviderRecordKind::new(MISTRAL_VIBE_RECORD_KIND).unwrap(),
        observation.messages_file.length,
        0,
        0,
        false,
    )
    .unwrap();
    let mut batches = Vec::new();
    while let Some(batch) = producer.next_batch().unwrap() {
        batches.push(batch);
    }
    batches
}

fn project_batch(
    projector: &mut MistralVibeCapturedBatchProjector,
    batch: &CapturedBatch,
    output: &mut CollectingProjectionOutput,
) {
    for record in batch.records() {
        projector.project_record(record, output).unwrap();
    }
}

#[test]
fn bounded_metadata_checkpoint_stays_small() {
    let (temp, source, _) = write_session(1);
    let huge = "x".repeat(PROVIDER_MAX_PREVIEW_CHARS * 8);
    fs::write(
        &source.metadata_path,
        serde_json::to_vec(&json!({
            "session_id": "mistral-bounded-session",
            "title": huge.clone(),
            "stats": { "huge": huge.clone() },
            "loops": [huge.clone()],
            "experiments": { "huge": huge },
        }))
        .unwrap(),
    )
    .unwrap();

    let (metadata, failure) =
        mistral_vibe_bounded_metadata(&source, "2026-07-17T13:00:00Z".parse().unwrap()).unwrap();
    assert!(metadata.get("title").is_some());
    assert!(failure.is_none());
    let checkpoint = MistralVibeParserCheckpoint {
        metadata_revision: "mistral-vibe-meta-v1:test".to_owned(),
        metadata_failure_reported: false,
        next_ordinal: 1,
        accepted_captures: 1,
        accepted_events: 1,
        accepted_file_touches: 0,
        rejected_records: 0,
    };
    BoundedParserCheckpoint::from_serializable(&checkpoint).unwrap();
    drop(temp);
}

#[test]
fn checkpoint_excludes_metadata_payload_and_resume_is_partition_exact() {
    const TITLE_SECRET: &str = "mistral-title-secret-7b2e";
    const SUBTREE_SECRET: &str = "mistral-subtree-secret-81ac";
    const PROFILE_SECRET: &str = "mistral-profile-secret-a90f";

    let (_temp, source, root) = write_session(65);
    fs::write(
        &source.metadata_path,
        serde_json::to_vec(&json!({
            "session_id": "mistral-bounded-session",
            "parent_session_id": "mistral-parent-session",
            "start_time": "2026-07-17T12:00:00Z",
            "end_time": "2026-07-17T14:00:00Z",
            "title": TITLE_SECRET,
            "environment": { "working_directory": "/workspace/mistral-secret" },
            "agent_profile": {
                "name": "vibe-agent",
                "private_note": PROFILE_SECRET,
            },
            "stats": { "private_note": SUBTREE_SECRET },
            "loops": [{ "private_note": SUBTREE_SECRET }],
            "experiments": { "private_note": SUBTREE_SECRET },
        }))
        .unwrap(),
    )
    .unwrap();

    let batches = captured_batches(&source);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].records().len(), 64);
    assert_eq!(batches[1].records().len(), 1);
    let observation = MistralVibeSessionObservation::read(&source).unwrap();
    let metadata_revision = observation.metadata_revision();
    let import_context = context(&source, root);
    let (metadata, metadata_failure) =
        mistral_vibe_bounded_metadata(&source, import_context.imported_at).unwrap();
    let binding = crate::complete_content::jsonl::ExactJsonlSourceBinding::new(
        &observation.source_revision(),
        "mistral-test-path",
    );

    let mut uninterrupted = MistralVibeCapturedBatchProjector::fresh(
        import_context.clone(),
        source.clone(),
        metadata.clone(),
        metadata_revision.clone(),
        metadata_failure.clone(),
        binding.clone(),
    );
    let mut uninterrupted_output = CollectingProjectionOutput::default();
    for batch in &batches {
        project_batch(&mut uninterrupted, batch, &mut uninterrupted_output);
    }

    let mut partitioned = MistralVibeCapturedBatchProjector::fresh(
        import_context.clone(),
        source.clone(),
        metadata,
        metadata_revision.clone(),
        metadata_failure,
        binding.clone(),
    );
    let mut partitioned_output = CollectingProjectionOutput::default();
    project_batch(&mut partitioned, &batches[0], &mut partitioned_output);
    let CapturedBatchCursorFinish::Advance(cursor) =
        partitioned.finish_cursor(&batches[0]).unwrap()
    else {
        panic!("Mistral Vibe completed batches must advance the certified cursor");
    };
    let checkpoint_bytes = cursor.parser_checkpoint().as_bytes();
    for secret in [TITLE_SECRET, SUBTREE_SECRET, PROFILE_SECRET] {
        assert!(!checkpoint_bytes
            .windows(secret.len())
            .any(|window| window == secret.as_bytes()));
    }
    let checkpoint_json = serde_json::from_slice::<Value>(checkpoint_bytes).unwrap();
    let checkpoint_fields = checkpoint_json.as_object().unwrap();
    assert_eq!(checkpoint_fields.len(), 7);
    assert!(checkpoint_fields.keys().all(|field| matches!(
        field.as_str(),
        "metadata_revision"
            | "metadata_failure_reported"
            | "next_ordinal"
            | "accepted_captures"
            | "accepted_events"
            | "accepted_file_touches"
            | "rejected_records"
    )));
    assert!(checkpoint_json.get("metadata").is_none());
    assert!(checkpoint_json.get("metadata_failure").is_none());
    assert_eq!(
        checkpoint_json
            .get("metadata_revision")
            .and_then(Value::as_str),
        Some(metadata_revision.as_str())
    );

    let (resumed_metadata, resumed_failure) =
        mistral_vibe_bounded_metadata(&source, import_context.imported_at).unwrap();
    let mut resumed = MistralVibeCapturedBatchProjector::resume(
        import_context,
        source,
        resumed_metadata,
        resumed_failure,
        &cursor,
        binding,
    )
    .unwrap();
    project_batch(&mut resumed, &batches[1], &mut partitioned_output);

    assert_eq!(partitioned_output, uninterrupted_output);
    assert_eq!(partitioned_output.captures.len(), 65);
    assert!(partitioned_output.rejections.is_empty());
}

#[test]
fn source_observation_freezes_metadata_and_messages() {
    let (_temp, source, _) = write_session(1);
    let observation = MistralVibeSessionObservation::read(&source).unwrap();
    assert!(observation.revalidate(&source).unwrap());

    let mut metadata = OpenOptions::new()
        .append(true)
        .open(&source.metadata_path)
        .unwrap();
    metadata.write_all(b" ").unwrap();
    metadata.sync_all().unwrap();

    assert!(!observation.revalidate(&source).unwrap());
    assert_ne!(
        observation.source_revision(),
        MistralVibeSessionObservation::read(&source)
            .unwrap()
            .source_revision()
    );
}

#[test]
fn session_source_traversal_accepts_a_session_at_the_depth_limit() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session_dir = nested_directory(&root, MISTRAL_VIBE_MAX_DIRECTORY_DEPTH);
    write_traversal_session(&session_dir);
    let mut visited = Vec::new();

    let count = visit_mistral_vibe_session_sources(&root, &mut |source| {
        visited.push(source.session_dir);
        Ok(())
    })
    .unwrap();

    assert_eq!(count, 1);
    assert_eq!(visited, vec![session_dir]);
}

#[test]
fn session_source_traversal_rejects_over_limit_nesting_without_a_positive_visit() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let over_limit = nested_directory(&root, MISTRAL_VIBE_MAX_DIRECTORY_DEPTH.saturating_add(1));
    write_traversal_session(&over_limit);
    let mut visited = Vec::new();

    let error = visit_mistral_vibe_session_sources(&root, &mut |source| {
        visited.push(source.session_dir);
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath {
            path,
            reason: "Mistral Vibe session directory nesting exceeds the supported limit",
        } if path == over_limit
    ));
    assert!(visited.is_empty());
}

#[test]
fn session_source_traversal_is_deterministic_depth_first_by_native_filename() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    for relative in [
        "z-session",
        "m-session",
        "a-parent/z-session",
        "a-parent/a-session",
    ] {
        write_traversal_session(&root.join(relative));
    }
    let visit_order = || {
        let mut visited = Vec::new();
        let count = visit_mistral_vibe_session_sources(&root, &mut |source| {
            visited.push(
                source
                    .session_dir
                    .strip_prefix(&root)
                    .unwrap()
                    .to_path_buf(),
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 4);
        visited
    };
    let expected = [
        "a-parent/a-session",
        "a-parent/z-session",
        "m-session",
        "z-session",
    ]
    .map(PathBuf::from)
    .to_vec();

    assert_eq!(visit_order(), expected);
    assert_eq!(visit_order(), expected);
}

#[test]
fn session_source_traversal_accepts_the_exact_entry_collection_limit() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_child_directories(&root, MISTRAL_VIBE_MAX_TRAVERSAL_ENTRIES);
    let session_dir = root.join(format!(
        "entry-{:04}",
        MISTRAL_VIBE_MAX_TRAVERSAL_ENTRIES - 1
    ));
    write_traversal_session(&session_dir);
    let mut visited = Vec::new();

    let count = visit_mistral_vibe_session_sources(&root, &mut |source| {
        visited.push(source.session_dir);
        Ok(())
    })
    .unwrap();

    assert_eq!(count, 1);
    assert_eq!(visited, vec![session_dir]);
}

#[test]
fn session_source_traversal_rejects_over_entry_collection_limit_before_visiting() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_child_directories(&root, MISTRAL_VIBE_MAX_TRAVERSAL_ENTRIES.saturating_add(1));
    write_traversal_session(&root.join("entry-0000"));
    let mut visited = Vec::new();

    let error = visit_mistral_vibe_session_sources(&root, &mut |source| {
        visited.push(source.session_dir);
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath {
            path,
            reason: "Mistral Vibe session traversal exceeds the supported directory entry limit",
        } if path == root
    ));
    assert!(visited.is_empty());
}

#[test]
fn session_source_traversal_counts_irrelevant_files_toward_the_entry_limit() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    for index in 0..MISTRAL_VIBE_MAX_TRAVERSAL_ENTRIES {
        fs::write(root.join(format!("irrelevant-{index:04}.txt")), b"").unwrap();
    }
    write_traversal_session(&root.join("valid-session"));
    let mut visited = Vec::new();

    let error = visit_mistral_vibe_session_sources(&root, &mut |source| {
        visited.push(source.session_dir);
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath {
            path,
            reason: "Mistral Vibe session traversal exceeds the supported directory entry limit",
        } if path == root
    ));
    assert!(visited.is_empty());
}

#[test]
fn unchanged_replay_preserves_authoritative_structural_rejection_count() {
    let (temp, source, root) = write_session(0);
    let mut oversized_record = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1];
    oversized_record.push(b'\n');
    fs::write(&source.messages_path, oversized_record).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let import_context = context(&source, root);

    let first = import_mistral_vibe_session_file_batched(
        source.clone(),
        &mut store,
        &import_context,
        &NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.imported_events, 0);

    let unchanged = import_mistral_vibe_session_file_batched(
        source,
        &mut store,
        &import_context,
        &NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(unchanged.failed, 1);
    assert_eq!(unchanged.imported_events, 0);
}

#[test]
fn batched_import_streams_messages_and_resumes_verified_append() {
    let (temp, source, root) = write_session(65);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let import_context = context(&source, root.clone());

    let first = import_mistral_vibe_session_file_batched(
        source.clone(),
        &mut store,
        &import_context,
        &NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 65);
    assert_eq!(first.failed, 0);

    let session = store
        .session_by_external_session(CaptureProvider::MistralVibe, "mistral-bounded-session")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 65);
    let capture_source = store
        .capture_source_by_external_session(CaptureProvider::MistralVibe, "mistral-bounded-session")
        .unwrap()
        .unwrap();
    let messages_path = source.messages_path.display().to_string();
    let source_root = root.display().to_string();
    assert_eq!(
        capture_source.descriptor.raw_source_path.as_deref(),
        Some(messages_path.as_str())
    );
    assert_eq!(
        capture_source.descriptor.source_root.as_deref(),
        Some(source_root.as_str())
    );

    let mut messages = OpenOptions::new()
        .append(true)
        .open(&source.messages_path)
        .unwrap();
    writeln!(
        messages,
        "{}",
        serde_json::to_string(&json!({
            "message_id": "message-65",
            "role": "assistant",
            "content": "verified append",
            "timestamp": "2026-07-17T13:01:00Z",
        }))
        .unwrap()
    )
    .unwrap();
    messages.sync_all().unwrap();

    let second = import_mistral_vibe_session_file_batched(
        source,
        &mut store,
        &ProviderAdapterContext {
            imported_at: "2026-07-17T13:02:00Z".parse().unwrap(),
            ..import_context
        },
        &NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(second.imported_events, 1);
    assert_eq!(second.skipped_events, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 66);
}

#[test]
fn append_resume_partition_matches_one_shot_store_exactly() {
    let (temp, source, root) = write_session(65);
    let import_context = context(&source, root);
    let options = NormalizedProviderImportOptions::default();
    let mut resumed_store = Store::open(temp.path().join("resumed.sqlite")).unwrap();

    let initial = import_mistral_vibe_session_file_batched(
        source.clone(),
        &mut resumed_store,
        &import_context,
        &options,
    )
    .unwrap();
    assert_eq!(initial.failed, 0, "{:?}", initial.failures);
    assert_eq!(initial.imported_events, 65);

    let mut messages = OpenOptions::new()
        .append(true)
        .open(&source.messages_path)
        .unwrap();
    writeln!(
        messages,
        "{}",
        serde_json::to_string(&json!({
            "message_id": "message-65",
            "role": "assistant",
            "content": "partition parity append",
            "timestamp": "2026-07-17T13:01:00Z",
        }))
        .unwrap()
    )
    .unwrap();
    messages.sync_all().unwrap();
    drop(messages);

    let resumed = import_mistral_vibe_session_file_batched(
        source.clone(),
        &mut resumed_store,
        &import_context,
        &options,
    )
    .unwrap();
    assert_eq!(resumed.failed, 0, "{:?}", resumed.failures);
    assert_eq!(resumed.imported_events, 1);

    let mut one_shot_store = Store::open(temp.path().join("one-shot.sqlite")).unwrap();
    let one_shot = import_mistral_vibe_session_file_batched(
        source.clone(),
        &mut one_shot_store,
        &import_context,
        &options,
    )
    .unwrap();
    assert_eq!(one_shot.failed, 0, "{:?}", one_shot.failures);
    assert_eq!(one_shot.imported_events, 66);

    let resumed_session = resumed_store
        .session_by_external_session(CaptureProvider::MistralVibe, "mistral-bounded-session")
        .unwrap()
        .unwrap();
    let one_shot_session = one_shot_store
        .session_by_external_session(CaptureProvider::MistralVibe, "mistral-bounded-session")
        .unwrap()
        .unwrap();
    let resumed_source = resumed_store
        .capture_source_by_external_session(CaptureProvider::MistralVibe, "mistral-bounded-session")
        .unwrap()
        .unwrap();
    let one_shot_source = one_shot_store
        .capture_source_by_external_session(CaptureProvider::MistralVibe, "mistral-bounded-session")
        .unwrap()
        .unwrap();
    let path_identity = provider_path_identity(&source.messages_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        &path_identity,
    );
    let resumed_cursor = resumed_store
        .get_sync_cursor(None, &import_context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let one_shot_cursor = one_shot_store
        .get_sync_cursor(None, &import_context.machine_id, &stream)
        .unwrap()
        .unwrap();

    assert_eq!(resumed_session, one_shot_session);
    assert_eq!(resumed_source, one_shot_source);
    assert_eq!(
        resumed_store
            .events_for_session(resumed_session.id)
            .unwrap(),
        one_shot_store
            .events_for_session(one_shot_session.id)
            .unwrap()
    );
    assert_eq!(resumed_cursor.cursor, one_shot_cursor.cursor);
}
