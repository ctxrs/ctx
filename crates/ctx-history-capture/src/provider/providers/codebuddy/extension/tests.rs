use std::{fs, num::NonZeroUsize, path::PathBuf};

use crate::test_support_paths::tempdir;
use serde_json::json;
use tempfile::TempDir;

use super::super::import_codebuddy_history_batched;
use super::*;
use crate::captured_batch::{CAPTURE_BATCH_MAX_PAYLOAD_BYTES, CAPTURE_BATCH_MAX_RECORDS};
use crate::provider::importer::import_captured_batches;

fn test_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "codebuddy-batch-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
    }
}

fn write_extension_session(message_count: usize) -> (TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let project = temp.path().join("history/project-hash");
    let session = project.join("session-bounded");
    fs::create_dir_all(session.join("messages")).unwrap();
    let messages = (0..message_count)
        .map(|index| {
            json!({
                "id": format!("message-{index}"),
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "type": "message",
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        session.join("index.json"),
        serde_json::to_vec(&json!({ "messages": messages })).unwrap(),
    )
    .unwrap();
    fs::write(
        project.join("index.json"),
        serde_json::to_vec(&json!({
            "conversations": [{
                "id": "session-bounded",
                "name": "Bounded CodeBuddy session",
                "createdAt": "2026-07-18T10:00:00Z",
                "updatedAt": "2026-07-18T11:59:00Z",
                "projectPath": "/workspace/codebuddy",
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    for index in 0..message_count {
        fs::write(
            session.join(format!("messages/message-{index}.json")),
            serde_json::to_vec(&json!({
                "id": format!("message-{index}"),
                "content": format!("bounded CodeBuddy extension message {index}"),
                "createdAt": format!("2026-07-18T10:{:02}:00Z", index % 60),
            }))
            .unwrap(),
        )
        .unwrap();
    }
    (temp, project, session)
}

fn force_extension_restart_after_first_batch(
    session_dir: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> CertifiedProviderCursor {
    let (Some(metadata), mut preparation) = codebuddy_extension_metadata(session_dir, 1).unwrap()
    else {
        panic!("extension metadata should be readable");
    };
    let observation = CodeBuddyExtensionObservation::read(&metadata, 1, &mut preparation).unwrap();
    assert_eq!(preparation.failed, 0, "{:?}", preparation.failures);
    let path_identity = provider_path_identity(&observation.canonical_session_dir).unwrap();
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(session_dir.to_path_buf()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let source = SourceObservation::new(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        format!("codebuddy-extension-session:{path_identity}"),
        observation.source_revision.clone(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            &path_identity,
        ),
        CODEBUDDY_CAPTURE_REVISION,
        CODEBUDDY_POLICY_REVISION,
        None,
    )
    .unwrap();
    let stream = captured_batch_cursor_stream(&source);
    let initial_position = codebuddy_whole_json_position(0).unwrap();
    let mut producer = codebuddy_extension_batch_producer(
        source.clone(),
        ProviderRecordKind::new(CODEBUDDY_EXTENSION_RECORD_KIND).unwrap(),
        &metadata,
        0,
    )
    .unwrap();
    let admission =
        CapturedSourceAdmission::conversation_for_context(&source, &file_context).unwrap();
    let mut projector =
        CodeBuddyExtensionCapturedBatchProjector::fresh(file_context.clone(), &metadata, 1);
    let first = import_captured_batches(
        store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        None,
        &initial_position,
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(1).unwrap(),
        &mut projector,
        || producer.next_batch().map_err(codebuddy_whole_json_error),
        || observation.revalidate(session_dir),
    )
    .unwrap();
    assert_eq!(first.batches_imported, 1);
    assert!(!first.source_exhausted);

    let stored_cursor = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&stored_cursor.cursor).unwrap();
    let mut resumed =
        CodeBuddyExtensionCapturedBatchProjector::resume(file_context, &metadata, 1, &certified)
            .unwrap();
    drain_captured_batches(
        store,
        &admission,
        NormalizedProviderImportOptions::default(),
        &context.machine_id,
        context.imported_at,
        Some(stored_cursor),
        &initial_position,
        CapturedBatchCursorMode::Resume,
        &stream,
        &mut resumed,
        || producer.next_batch().map_err(codebuddy_whole_json_error),
        || observation.revalidate(session_dir),
    )
    .unwrap();
    certified
}

fn assert_codebuddy_store_parity(one_shot: &Store, resumed: &Store, provider_session_id: &str) {
    let one_shot_session = one_shot
        .session_by_external_session(CaptureProvider::CodeBuddy, provider_session_id)
        .unwrap()
        .unwrap();
    let resumed_session = resumed
        .session_by_external_session(CaptureProvider::CodeBuddy, provider_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(resumed_session, one_shot_session);
    assert_eq!(
        resumed.events_for_session(resumed_session.id).unwrap(),
        one_shot.events_for_session(one_shot_session.id).unwrap()
    );
    assert_eq!(
        resumed
            .get_capture_source(resumed_session.capture_source_id.unwrap())
            .unwrap(),
        one_shot
            .get_capture_source(one_shot_session.capture_source_id.unwrap())
            .unwrap()
    );
}

#[test]
fn extension_producer_bounds_batches_and_replay_preserves_behavior() {
    let (temp, project, session) = write_extension_session(65);
    let (Some(metadata), mut preparation) = codebuddy_extension_metadata(&session, 1).unwrap()
    else {
        panic!("extension metadata should be readable");
    };
    let observation = CodeBuddyExtensionObservation::read(&metadata, 1, &mut preparation).unwrap();
    assert_eq!(preparation.failed, 0, "{:?}", preparation.failures);
    let source = SourceObservation::new(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        "codebuddy-extension-test",
        observation.source_revision.clone(),
        "provider:codebuddy:test:extension",
        CODEBUDDY_CAPTURE_REVISION,
        CODEBUDDY_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut producer = codebuddy_extension_batch_producer(
        source,
        ProviderRecordKind::new(CODEBUDDY_EXTENSION_RECORD_KIND).unwrap(),
        &metadata,
        0,
    )
    .unwrap();
    let first = producer.next_batch().unwrap().unwrap();
    let second = producer.next_batch().unwrap().unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(second.records().len(), 1);
    assert!(first.retained_payload_bytes() <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
    assert!(second.retained_payload_bytes() <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
    assert_eq!(first.range_end(), second.range_before());
    assert!(producer.next_batch().unwrap().is_none());
    drop(producer);

    let mut store = Store::open(temp.path().join("extension.sqlite")).unwrap();
    let first = import_codebuddy_history_batched(
        &project,
        &mut store,
        test_context(&project),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 65);
    let session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, "project-hash/session-bounded")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 65);
    let stored = store.get_session(session.id).unwrap();
    assert_eq!(
        stored.sync.metadata["metadata"]["title"].as_str(),
        Some("Bounded CodeBuddy session")
    );
    let capture_source = store
        .capture_source_by_external_session(
            CaptureProvider::CodeBuddy,
            "project-hash/session-bounded",
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        capture_source.descriptor.cwd.as_deref(),
        Some("/workspace/codebuddy")
    );

    let replay = import_codebuddy_history_batched(
        &project,
        &mut store,
        test_context(&project),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 65);
}

#[test]
fn extension_checkpoint_omits_source_text_and_forced_resume_matches_one_shot_store() {
    let (temp, project, session_dir) = write_extension_session(65);
    let generated_title_secret = "extension-title-secret-must-not-enter-checkpoint";
    let unrelated_source_secret = "extension-unrelated-secret-must-not-enter-checkpoint";
    let mut project_index: Value =
        serde_json::from_slice(&fs::read(project.join("index.json")).unwrap()).unwrap();
    project_index["conversations"][0]
        .as_object_mut()
        .unwrap()
        .remove("name");
    fs::write(
        project.join("index.json"),
        serde_json::to_vec(&project_index).unwrap(),
    )
    .unwrap();
    let first_message_path = session_dir.join("messages/message-0.json");
    let mut first_message: Value =
        serde_json::from_slice(&fs::read(&first_message_path).unwrap()).unwrap();
    first_message["content"] = json!(generated_title_secret);
    first_message["providerSecret"] = json!(unrelated_source_secret);
    fs::write(
        &first_message_path,
        serde_json::to_vec(&first_message).unwrap(),
    )
    .unwrap();

    let context = test_context(&project);
    let mut one_shot = Store::open(temp.path().join("extension-one-shot.sqlite")).unwrap();
    import_codebuddy_history_batched(
        &project,
        &mut one_shot,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    let mut resumed = Store::open(temp.path().join("extension-resumed.sqlite")).unwrap();
    let cursor = force_extension_restart_after_first_batch(&session_dir, &mut resumed, &context);
    let checkpoint = String::from_utf8_lossy(cursor.parser_checkpoint().as_bytes());
    assert!(!checkpoint.contains(generated_title_secret));
    assert!(!checkpoint.contains(unrelated_source_secret));
    let checkpoint_value: Value =
        serde_json::from_slice(cursor.parser_checkpoint().as_bytes()).unwrap();
    assert!(checkpoint_value.get("generated_title").is_none());
    assert_eq!(checkpoint_value["generated_title_message_index"], json!(0));
    assert_codebuddy_store_parity(&one_shot, &resumed, "project-hash/session-bounded");
}

#[test]
fn extension_replay_retains_deterministic_failures() {
    let (temp, project, session_dir) = write_extension_session(2);
    fs::write(
        session_dir.join("messages/message-0.json"),
        b"{malformed-extension-message}",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("extension-failures.sqlite")).unwrap();
    let first = import_codebuddy_history_batched(
        &project,
        &mut store,
        test_context(&project),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.imported_events, 1);

    let replay = import_codebuddy_history_batched(
        &project,
        &mut store,
        test_context(&project),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert_eq!(replay.failures, first.failures);
    assert_eq!(replay.skipped_events, 1);
}
