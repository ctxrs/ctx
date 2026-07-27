use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    num::NonZeroUsize,
};

use crate::test_support_paths::tempdir;
use tempfile::TempDir;

use super::super::import_codebuddy_history_batched;
use super::*;
use crate::provider::importer::import_captured_batches;

fn test_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "codebuddy-batch-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
    }
}

fn write_cli_file(message_count: usize) -> (TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    let project = root.join("projects/cli-project");
    let path = project.join("cli-session.jsonl");
    fs::create_dir_all(&project).unwrap();
    let mut jsonl = String::new();
    for index in 0..message_count {
        jsonl.push_str(
            &serde_json::to_string(&json!({
                "id": format!("cli-message-{index}"),
                "type": "message",
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": format!("bounded CodeBuddy CLI message {index}"),
                "timestamp": format!("2026-07-18T10:{:02}:00Z", index % 60),
                "sessionId": "cli-session",
                "cwd": "/workspace/codebuddy-cli",
            }))
            .unwrap(),
        );
        jsonl.push('\n');
    }
    fs::write(&path, jsonl).unwrap();
    (temp, root, path)
}

fn force_cli_restart_after_first_batch(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> CertifiedProviderCursor {
    let frozen = CodeBuddyFrozenFile::read(path).unwrap();
    let canonical_path = fs::canonicalize(path).unwrap();
    let path_identity = provider_path_identity(&canonical_path).unwrap();
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let source = SourceObservation::new(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        format!("codebuddy-cli-jsonl:{path_identity}"),
        frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION),
        provider_source_cursor_stream_for_path(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            &path_identity,
        ),
        CODEBUDDY_CAPTURE_REVISION,
        CODEBUDDY_CLI_POLICY_REVISION,
        None,
    )
    .unwrap();
    let stream = captured_batch_cursor_stream(&source);
    let initial_position = initial_jsonl_position().unwrap();
    let file = File::open(path).unwrap();
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        path_identity.clone().into_bytes(),
        ProviderRecordKind::new(CODEBUDDY_CLI_RECORD_KIND).unwrap(),
        frozen.length,
        0,
        0,
        true,
    )
    .unwrap();
    let admission =
        CapturedSourceAdmission::conversation_for_context(&source, &file_context).unwrap();
    let binding = CodeBuddyCliCompleteContentBinding::for_source(&source, &path_identity);
    let mut projector = CodeBuddyCliCapturedBatchProjector::fresh(
        file_context.clone(),
        path.to_path_buf(),
        1,
        binding.clone(),
    );
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
        || producer.next_batch().map_err(codebuddy_jsonl_error),
        || frozen.revalidate(path),
    )
    .unwrap();
    assert_eq!(first.batches_imported, 1);
    assert!(!first.source_exhausted);

    let stored_cursor = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&stored_cursor.cursor).unwrap();
    let mut resumed = CodeBuddyCliCapturedBatchProjector::resume(
        file_context,
        path.to_path_buf(),
        1,
        &certified,
        binding,
    )
    .unwrap()
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
        || producer.next_batch().map_err(codebuddy_jsonl_error),
        || frozen.revalidate(path),
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
fn cli_jsonl_replay_and_verified_append_are_exact() {
    let (temp, root, path) = write_cli_file(65);
    let mut store = Store::open(temp.path().join("cli.sqlite")).unwrap();
    let first = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 65);

    let replay = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 65);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&json!({
            "id": "cli-message-65",
            "type": "message",
            "role": "assistant",
            "content": "verified CodeBuddy append",
            "timestamp": "2026-07-18T11:05:00Z",
            "sessionId": "cli-session",
            "cwd": "/workspace/codebuddy-cli",
        }))
        .unwrap()
    )
    .unwrap();
    file.sync_all().unwrap();

    let append = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(append.failed, 0, "{:?}", append.failures);
    assert_eq!(append.imported_events, 1);
    assert_eq!(append.skipped_events, 0);

    let appended_session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, "cli-project/cli-session")
        .unwrap()
        .unwrap();
    let appended_session_index: Value = serde_json::from_str(
        appended_session.sync.metadata["metadata"]["session_index"]["json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(appended_session_index["rows"].as_u64(), Some(66));

    let mut one_shot = Store::open(temp.path().join("cli-one-shot-append.sqlite")).unwrap();
    let one_shot_summary = import_codebuddy_history_batched(
        &root,
        &mut one_shot,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        one_shot_summary.failed, 0,
        "{:?}",
        one_shot_summary.failures
    );
    assert_eq!(one_shot_summary.imported_events, 66);
    assert_codebuddy_store_parity(&one_shot, &store, "cli-project/cli-session");

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::to_string(&json!({
            "id": "cli-metadata-66",
            "type": "metadata",
            "timestamp": "2026-07-18T11:06:00Z",
            "sessionId": "cli-session",
            "cwd": "/workspace/codebuddy-cli",
        }))
        .unwrap()
    )
    .unwrap();
    file.sync_all().unwrap();

    let metadata_append = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(metadata_append.failed, 0, "{:?}", metadata_append.failures);
    assert_eq!(metadata_append.imported_events, 0);
    assert_eq!(metadata_append.accepted_content_records, 1);
    let appended_session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, "cli-project/cli-session")
        .unwrap()
        .unwrap();
    let appended_session_index: Value = serde_json::from_str(
        appended_session.sync.metadata["metadata"]["session_index"]["json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(appended_session_index["rows"].as_u64(), Some(67));

    let mut one_shot_with_metadata =
        Store::open(temp.path().join("cli-one-shot-metadata.sqlite")).unwrap();
    let one_shot_with_metadata_summary = import_codebuddy_history_batched(
        &root,
        &mut one_shot_with_metadata,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        one_shot_with_metadata_summary.failed, 0,
        "{:?}",
        one_shot_with_metadata_summary.failures
    );
    assert_eq!(one_shot_with_metadata_summary.imported_events, 66);
    assert_codebuddy_store_parity(&one_shot_with_metadata, &store, "cli-project/cli-session");
}

#[test]
fn cli_checkpoint_omits_source_text_and_forced_resume_matches_one_shot_store() {
    let (temp, root, path) = write_cli_file(65);
    let generated_title_secret = "cli-title-secret-must-not-enter-checkpoint";
    let unrelated_source_secret = "cli-unrelated-secret-must-not-enter-checkpoint";
    let mut lines = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut first: Value = serde_json::from_str(&lines[0]).unwrap();
    first["content"] = json!(generated_title_secret);
    first["providerSecret"] = json!(unrelated_source_secret);
    lines[0] = serde_json::to_string(&first).unwrap();
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let context = test_context(&root);
    let mut one_shot = Store::open(temp.path().join("cli-one-shot.sqlite")).unwrap();
    import_codebuddy_history_batched(
        &root,
        &mut one_shot,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    let mut resumed = Store::open(temp.path().join("cli-resumed.sqlite")).unwrap();
    let cursor = force_cli_restart_after_first_batch(&path, &mut resumed, &context);
    let checkpoint = String::from_utf8_lossy(cursor.parser_checkpoint().as_bytes());
    assert!(!checkpoint.contains(generated_title_secret));
    assert!(!checkpoint.contains(unrelated_source_secret));
    let checkpoint_value: Value =
        serde_json::from_slice(cursor.parser_checkpoint().as_bytes()).unwrap();
    assert!(checkpoint_value.get("title").is_none());
    assert!(checkpoint_value["generated_title_record"]["end"]
        .as_u64()
        .is_some());
    assert_codebuddy_store_parity(&one_shot, &resumed, "cli-project/cli-session");
}

#[test]
fn cli_changed_title_anchor_outside_append_proof_forces_full_reset() {
    let (temp, root, path) = write_cli_file(65);
    let original_title = "A".repeat(40);
    let rewritten_title = "B".repeat(40);
    let padding = "p".repeat(2 * 1024);
    let mut lines = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for (index, line) in lines.iter_mut().enumerate() {
        let mut value: Value = serde_json::from_str(line).unwrap();
        value["providerPadding"] = json!(padding.as_str());
        if index == 0 {
            value["content"] = json!(original_title.as_str());
        }
        *line = serde_json::to_string(&value).unwrap();
    }
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let mut store = Store::open(temp.path().join("cli-anchor-reset.sqlite")).unwrap();
    let first = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 65);
    let original_session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, "cli-project/cli-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        original_session.sync.metadata["metadata"]["title"].as_str(),
        Some(original_title.as_str())
    );

    let mut first_value: Value = serde_json::from_str(&lines[0]).unwrap();
    first_value["content"] = json!(rewritten_title.as_str());
    let rewritten_first = serde_json::to_string(&first_value).unwrap();
    assert_eq!(rewritten_first.len(), lines[0].len());
    lines[0] = rewritten_first;
    let mut changed = format!("{}\n", lines.join("\n"));
    changed.push_str(
        &serde_json::to_string(&json!({
            "id": "cli-message-65",
            "type": "message",
            "role": "assistant",
            "content": "append after old title rewrite",
            "timestamp": "2026-07-18T11:05:00Z",
            "sessionId": "cli-session",
            "cwd": "/workspace/codebuddy-cli",
        }))
        .unwrap(),
    );
    changed.push('\n');
    fs::write(&path, changed).unwrap();

    let replacement = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replacement.failed, 0, "{:?}", replacement.failures);
    assert_eq!(replacement.imported_events, 1);
    assert_eq!(replacement.skipped_events, 65);
    let replaced_session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, "cli-project/cli-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        replaced_session.sync.metadata["metadata"]["title"].as_str(),
        Some(rewritten_title.as_str())
    );
    assert_eq!(
        store.events_for_session(replaced_session.id).unwrap().len(),
        66
    );
}

#[test]
fn cli_jsonl_replay_retains_deterministic_failures() {
    let (temp, root, path) = write_cli_file(1);
    let valid = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{{not-json}}\n{valid}")).unwrap();
    let mut store = Store::open(temp.path().join("failures.sqlite")).unwrap();
    let first = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.imported_events, 1);

    let replay = import_codebuddy_history_batched(
        &root,
        &mut store,
        test_context(&root),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert_eq!(replay.failures, first.failures);
    assert_eq!(replay.skipped_events, 1);
}
