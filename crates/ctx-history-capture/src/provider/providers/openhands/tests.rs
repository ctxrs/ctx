use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::captured_batch::{ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CertifiedProviderCursor,
};
use crate::test_support_paths::tempdir;

use super::*;

#[test]
fn command_observation_retains_explicit_native_outcome() {
    let path = PathBuf::from("/tmp/openhands/session/events/1-observation.json");
    let value = json!({
        "id": "observation-1",
        "timestamp": "2026-07-21T00:00:00Z",
        "kind": "ObservationEvent",
        "source": "environment",
        "tool_call_id": "call-1",
        "observation": {
            "kind": "ExecuteBashObservation",
            "exit_code": 0,
            "content": "[main 0123456789abcdef0123456789abcdef01234567] private narrative"
        }
    });
    let decoded = decode_openhands_event(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
    let event = openhands_provider_event_with_identity(
        "session-1",
        &path,
        &decoded,
        decoded.timestamp(),
        OpenHandsEventIdentity::for_path(&path, "openhands-test-event"),
        None,
    );

    assert_eq!(event.event_type, EventType::CommandOutput);
    assert_eq!(event.payload["result_outcome"], "success");
    assert!(event
        .payload
        .to_string()
        .contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(!event.payload.to_string().contains("private narrative"));
}

fn openhands_checkpoint_failure(
    event_path: &Path,
    error: impl Into<String>,
) -> ProviderImportFailure {
    ProviderImportFailure {
        line: openhands_line_number(event_path),
        error: error.into(),
    }
}

fn resume_openhands_test_checkpoint(
    event_path: &Path,
    position: u64,
    checkpoint: OpenHandsParserCheckpoint,
) -> Result<OpenHandsCapturedBatchProjector> {
    let cursor = CertifiedProviderCursor::new(
        "openhands-test-revision",
        OPENHANDS_CAPTURE_REVISION,
        OPENHANDS_POLICY_REVISION,
        openhands_position(position)?,
        BoundedParserCheckpoint::from_serializable(&checkpoint)?,
    )?;
    OpenHandsCapturedBatchProjector::resume(
        ProviderAdapterContext {
            machine_id: "openhands-checkpoint-test".to_owned(),
            source_path: Some(event_path.to_path_buf()),
            source_root: event_path.parent().map(Path::to_path_buf),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        event_path.to_path_buf(),
        event_path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        "session".to_owned(),
        OpenHandsEventIdentity::for_path(event_path, "openhands-checkpoint-test"),
        OpenHandsProjectionMode::Full,
        &cursor,
    )
}

fn assert_openhands_checkpoint_rejected(
    event_path: &Path,
    position: u64,
    checkpoint: OpenHandsParserCheckpoint,
) {
    assert!(matches!(
        resume_openhands_test_checkpoint(event_path, position, checkpoint),
        Err(CaptureError::InvalidPayload(message))
            if message == "OpenHands parser checkpoint does not match its event-file position"
    ));
}

#[test]
fn certified_checkpoint_state_machine_accepts_only_reachable_states() {
    let event_path = Path::new("/tmp/openhands/v1_conversations/session/event.json");
    let touch_limit =
        u64::try_from(crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap();
    let checkpoints = [
        (
            0,
            OpenHandsParserCheckpoint {
                next_position: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejection: None,
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejection: Some(openhands_checkpoint_failure(event_path, "invalid JSON")),
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 1,
                accepted_file_touches: 0,
                rejection: None,
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 1,
                accepted_file_touches: touch_limit,
                rejection: None,
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 1,
                accepted_file_touches: touch_limit,
                rejection: Some(openhands_checkpoint_failure(
                    event_path,
                    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
                )),
            },
        ),
    ];

    for (position, checkpoint) in checkpoints {
        assert!(
            resume_openhands_test_checkpoint(event_path, position, checkpoint).is_ok(),
            "reachable checkpoint at position {position} was rejected"
        );
    }
}

#[test]
fn certified_checkpoint_state_machine_rejects_impossible_states() {
    let event_path = Path::new("/tmp/openhands/v1_conversations/session/event.json");
    let line = openhands_line_number(event_path);
    let touch_limit =
        u64::try_from(crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap();
    let rejection = || Some(openhands_checkpoint_failure(event_path, "invalid JSON"));
    let malformed = [
        (
            0,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejection: None,
            },
        ),
        (
            0,
            OpenHandsParserCheckpoint {
                next_position: 0,
                accepted_events: 1,
                accepted_file_touches: 0,
                rejection: None,
            },
        ),
        (
            0,
            OpenHandsParserCheckpoint {
                next_position: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejection: rejection(),
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejection: None,
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 0,
                accepted_file_touches: 1,
                rejection: rejection(),
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 1,
                accepted_file_touches: touch_limit + 1,
                rejection: None,
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 1,
                accepted_file_touches: touch_limit - 1,
                rejection: Some(openhands_checkpoint_failure(
                    event_path,
                    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
                )),
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 1,
                accepted_file_touches: touch_limit,
                rejection: rejection(),
            },
        ),
        (
            1,
            OpenHandsParserCheckpoint {
                next_position: 1,
                accepted_events: 2,
                accepted_file_touches: 0,
                rejection: None,
            },
        ),
    ];

    for (position, checkpoint) in malformed {
        assert_openhands_checkpoint_rejected(event_path, position, checkpoint);
    }

    assert_openhands_checkpoint_rejected(
        event_path,
        1,
        OpenHandsParserCheckpoint {
            next_position: 1,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejection: Some(ProviderImportFailure {
                line: line ^ 1,
                error: "invalid JSON".to_owned(),
            }),
        },
    );
    assert_openhands_checkpoint_rejected(
        event_path,
        1,
        OpenHandsParserCheckpoint {
            next_position: 1,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejection: Some(openhands_checkpoint_failure(event_path, "")),
        },
    );
    assert_openhands_checkpoint_rejected(
        event_path,
        1,
        OpenHandsParserCheckpoint {
            next_position: 1,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejection: Some(openhands_checkpoint_failure(
                event_path,
                "x".repeat(OPENHANDS_MAX_FAILURE_BYTES + 1),
            )),
        },
    );
}

#[test]
fn mixed_checkpoint_exception_is_limited_to_the_exact_touch_ceiling_rejection() {
    let event_path = Path::new("/tmp/openhands/v1_conversations/session/event.json");
    let line = openhands_line_number(event_path);
    let touch_limit =
        u64::try_from(crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap();
    let checkpoint = |accepted_file_touches, line, error: &str| OpenHandsParserCheckpoint {
        next_position: 1,
        accepted_events: 1,
        accepted_file_touches,
        rejection: Some(ProviderImportFailure {
            line,
            error: error.to_owned(),
        }),
    };

    assert!(openhands_checkpoint_matches_position(
        &checkpoint(touch_limit, line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION),
        1,
        event_path,
    ));
    assert!(!openhands_checkpoint_matches_position(
        &checkpoint(touch_limit - 1, line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION),
        1,
        event_path,
    ));
    assert!(!openhands_checkpoint_matches_position(
        &checkpoint(touch_limit, line ^ 1, PROVIDER_FILE_TOUCH_LIMIT_REJECTION),
        1,
        event_path,
    ));
    assert!(!openhands_checkpoint_matches_position(
        &checkpoint(touch_limit, line, "legacy mixed rejection"),
        1,
        event_path,
    ));
}

#[test]
fn accepted_event_with_touch_limit_rejection_replays_exactly_without_writes() {
    const OVERFLOWING_PATH_COUNT: usize =
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT + 1;

    let temp = tempdir().unwrap();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("local-user")
        .join("v1_conversations")
        .join("touch-limit-session");
    fs::create_dir_all(&conversation).unwrap();
    let event_path = conversation.join("0001-touch-limit.json");
    let paths = (0..OVERFLOWING_PATH_COUNT)
        .map(|index| json!({ "path": format!("src/generated/touch-{index:05}.rs") }))
        .collect::<Vec<_>>();
    fs::write(
        &event_path,
        serde_json::to_vec(&json!({
            "id": "openhands-touch-limit",
            "timestamp": "2026-07-18T12:00:00Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "files": paths,
            },
        }))
        .unwrap(),
    )
    .unwrap();
    let canonical_event_path = fs::canonicalize(&event_path).unwrap();

    let context = ProviderAdapterContext {
        machine_id: "openhands-touch-limit-import".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut store = Store::open(temp.path().join("store.sqlite")).unwrap();
    let first = import_openhands_file_events_batched(
        &event_path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(first.imported_sessions, 1, "{:?}", first.failures);
    assert_eq!(first.imported_events, 1, "{:?}", first.failures);
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].error, PROVIDER_FILE_TOUCH_LIMIT_REJECTION);
    assert_eq!(
        first.failures[0].line,
        openhands_line_number(&canonical_event_path)
    );
    assert_eq!(
        first.accepted_content_records,
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT + 1
    );
    let first_store_counts = {
        let archive = store.export_archive().unwrap();
        let final_accepted_tag = format!(
            "src/generated/touch-{:05}.rs",
            crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT - 1
        );
        let rejected_tag = format!(
            "src/generated/touch-{:05}.rs",
            crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT
        );
        assert!(archive
            .files_touched
            .iter()
            .any(|touch| touch.path == "src/generated/touch-00000.rs"));
        assert!(archive
            .files_touched
            .iter()
            .any(|touch| touch.path == final_accepted_tag));
        assert!(!archive
            .files_touched
            .iter()
            .any(|touch| touch.path == rejected_tag));
        (
            archive.capture_sources.len(),
            archive.sessions.len(),
            archive.events.len(),
            archive.files_touched.len(),
        )
    };
    assert_eq!(
        first_store_counts.3,
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT
    );

    let replay = import_openhands_file_events_batched(
        &event_path,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(replay.imported, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_sessions, 0, "{:?}", replay.failures);
    assert_eq!(replay.imported_events, 0, "{:?}", replay.failures);
    assert_eq!(replay.skipped_sessions, 1, "{:?}", replay.failures);
    assert_eq!(replay.skipped_events, 1, "{:?}", replay.failures);
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert_eq!(replay.failures, first.failures);
    assert_eq!(
        replay.accepted_content_records,
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT + 1
    );
    assert_eq!(
        replay.skipped,
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT + 2
    );
    let replay_store_counts = {
        let archive = store.export_archive().unwrap();
        (
            archive.capture_sources.len(),
            archive.sessions.len(),
            archive.events.len(),
            archive.files_touched.len(),
        )
    };
    assert_eq!(replay_store_counts, first_store_counts);
}

#[test]
fn interrupted_rotated_projection_repairs_all_touches_before_cursor_publication() {
    const TOUCH_COUNT: usize = 130;
    const FAIL_AFTER_TOUCHES: usize = 70;

    let temp = tempdir().unwrap();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("local-user")
        .join("v1_conversations")
        .join("rotated-recovery-session");
    fs::create_dir_all(&conversation).unwrap();
    let event_path = conversation.join("0001-rotated-recovery.json");
    let paths = (0..TOUCH_COUNT)
        .map(|index| json!({ "path": format!("src/recovery/touch-{index:03}.rs") }))
        .collect::<Vec<_>>();
    fs::write(
        &event_path,
        serde_json::to_vec(&json!({
            "id": "openhands-rotated-recovery",
            "timestamp": "2026-07-18T12:00:00Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "files": paths,
            },
        }))
        .unwrap(),
    )
    .unwrap();
    let canonical_event_path = fs::canonicalize(&event_path).unwrap();
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::OpenHands,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        &provider_path_identity(&canonical_event_path).unwrap(),
    );
    let context = ProviderAdapterContext {
        machine_id: "openhands-rotated-recovery-import".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut uninterrupted = Store::open(temp.path().join("uninterrupted.sqlite")).unwrap();
    let uninterrupted_summary = import_openhands_file_events_batched(
        &event_path,
        &mut uninterrupted,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(uninterrupted_summary.imported_events, 1);
    assert_eq!(
        uninterrupted.export_archive().unwrap().files_touched.len(),
        TOUCH_COUNT
    );

    let mut recovered = Store::open(temp.path().join("recovered.sqlite")).unwrap();
    let interrupted = with_openhands_post_touch_failure(FAIL_AFTER_TOUCHES, || {
        import_openhands_file_events_batched(
            &event_path,
            &mut recovered,
            context.clone(),
            NormalizedProviderImportOptions::default(),
        )
    });
    assert!(matches!(
        interrupted,
        Err(CaptureError::SystemInvariant(
            "injected OpenHands post-touch projection failure"
        ))
    ));
    let interrupted_archive = recovered.export_archive().unwrap();
    assert_eq!(interrupted_archive.events.len(), 1);
    assert!((1..FAIL_AFTER_TOUCHES).contains(&interrupted_archive.files_touched.len()));
    assert!(recovered
        .get_sync_cursor(None, &context.machine_id, &cursor_stream)
        .unwrap()
        .is_none());

    let repaired = import_openhands_file_events_batched(
        &event_path,
        &mut recovered,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(repaired.imported_events, 0, "{:?}", repaired.failures);
    assert_eq!(repaired.skipped_events, 1, "{:?}", repaired.failures);
    assert_eq!(
        recovered.export_archive().unwrap(),
        uninterrupted.export_archive().unwrap()
    );
    assert_eq!(
        recovered
            .get_sync_cursor(None, &context.machine_id, &cursor_stream)
            .unwrap(),
        uninterrupted
            .get_sync_cursor(None, &context.machine_id, &cursor_stream)
            .unwrap()
    );

    let before_noop = recovered.export_archive().unwrap();
    let before_noop_cursor = recovered
        .get_sync_cursor(None, &context.machine_id, &cursor_stream)
        .unwrap();
    let noop = import_openhands_file_events_batched(
        &event_path,
        &mut recovered,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(noop.imported, 0, "{:?}", noop.failures);
    assert_eq!(noop.skipped_events, 1, "{:?}", noop.failures);
    assert_eq!(recovered.export_archive().unwrap(), before_noop);
    assert_eq!(
        recovered
            .get_sync_cursor(None, "openhands-rotated-recovery-import", &cursor_stream,)
            .unwrap(),
        before_noop_cursor
    );
}

#[test]
fn one_shot_event_batch_marks_the_source_exhausted() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("1-event.json");
    let bytes = br#"{"id":"one-shot"}"#.to_vec();
    fs::write(&path, &bytes).unwrap();
    let frozen = OpenHandsFrozenFile::read(&path).unwrap();
    let source = SourceObservation::new(
        CaptureProvider::OpenHands,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        "openhands-one-shot-source",
        "openhands-one-shot-revision",
        "openhands-one-shot-stream",
        OPENHANDS_CAPTURE_REVISION,
        OPENHANDS_POLICY_REVISION,
        None,
    )
    .unwrap();

    let batch = capture_openhands_event_batch(
        &path,
        &path.display().to_string(),
        &frozen,
        Some(bytes),
        source,
        ProviderRecordKind::new(OPENHANDS_RECORD_KIND).unwrap(),
    )
    .unwrap();

    assert!(batch.source_exhausted());
}

#[test]
fn content_hash_changes_revision_when_file_stats_are_identical() {
    let frozen = OpenHandsFrozenFile {
        length: 4,
        modified: UNIX_EPOCH + Duration::from_secs(10),
        readonly: false,
        device: Some(1),
        inode: Some(2),
    };
    let first: [u8; 32] = Sha256::digest(b"aaaa").into();
    let second: [u8; 32] = Sha256::digest(b"bbbb").into();

    assert_ne!(
        frozen.source_revision(Some(&first)),
        frozen.source_revision(Some(&second))
    );
}

#[test]
fn event_identity_does_not_collapse_duplicate_filename_ordinals() {
    let first = OpenHandsEventIdentity::for_path(
        Path::new("0001-alpha.json"),
        "/root/v1_conversations/session/0001-alpha.json",
    );
    let second = OpenHandsEventIdentity::for_path(
        Path::new("0001-beta.json"),
        "/root/v1_conversations/session/0001-beta.json",
    );

    assert_ne!(first.provider_event_index, second.provider_event_index);
    assert_ne!(
        first.provider_event_identity_index,
        second.provider_event_identity_index
    );
    assert_eq!(first.legacy_provider_event_index_candidate, Some(0));
    assert_eq!(second.legacy_provider_event_index_candidate, Some(0));
}
