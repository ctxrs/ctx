use crate::provider::importer::{
    provider_file_touch_import_id, provider_scoped_source_identity_key,
    provider_scoped_source_uuid, provider_source_event_import_identity, provider_source_event_uuid,
    provider_source_root_identity, provider_source_session_uuid, provider_sync_metadata,
    timestamps,
};
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::tests::support::source_snapshot::provider_source_snapshot;
use crate::{
    import_openhands_file_events, provider_source_for_path, OpenHandsImportOptions,
    ProviderSourceStatus, MAX_PROVIDER_JSONL_LINE_BYTES,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched, Session,
    SessionStatus,
};
use ctx_history_store::Store;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use uuid::Uuid;

use crate::provider::providers::openhands::{
    count_openhands_source_file_opens, seed_c213_openhands_terminal_cursor,
};

fn import_origin_main_openhands_event(
    store: &mut Store,
    root: &Path,
    event_path: &Path,
    provider_session_id: &str,
    provider_event: (u64, &str),
    payload_text: &str,
    file_touch_path: Option<&str>,
) -> Uuid {
    let (provider_event_index, provider_event_hash) = provider_event;
    let occurred_at = "2026-07-04T17:00:00Z".parse().unwrap();
    let canonical_event_path = fs::canonicalize(event_path).unwrap();
    let conversation_dir = canonical_event_path.parent().unwrap().display().to_string();
    let source_root = fs::canonicalize(root).unwrap().display().to_string();
    let source_format = "openhands_file_events";
    let legacy_source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenHands,
        provider_session_id,
        source_format,
        Some(&conversation_dir),
    );
    let source_identity =
        provider_source_root_identity(CaptureProvider::OpenHands, source_format, &source_root);
    let source_identity_key = provider_scoped_source_identity_key(
        CaptureProvider::OpenHands,
        provider_session_id,
        source_format,
        Some(&conversation_dir),
    );
    let source_idempotency_key =
        format!("provider-source:openhands:{source_format}:{provider_session_id}");
    let session_idempotency_key = format!("provider-session:openhands:{provider_session_id}");
    let session_metadata = json!({
        "source_format": source_format,
        "provider": "openhands",
        "conversation_id": provider_session_id,
    });
    let source_metadata = json!({
        "adapter": source_format,
        "storage": "filesystem_event_service",
        "conversation_dir": conversation_dir,
    });
    let legacy_session_id = provider_source_session_uuid(&source_identity, provider_session_id);
    let cursor = format!("{}:{provider_event_hash}", canonical_event_path.display());

    // Seed the exact Store rows emitted by the released 0.26 importer.
    store
        .upsert_capture_source(&CaptureSource {
            id: legacy_source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::OpenHands,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: None,
                raw_source_path: Some(conversation_dir.clone()),
                source_format: Some(source_format.to_owned()),
                source_root: Some(source_root.clone()),
                source_identity: Some(source_identity.clone()),
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at: occurred_at,
            ended_at: Some(occurred_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": provider_session_id,
                    "source_format": source_format,
                    "source_trust": "fixture",
                    "cursor": null,
                    "fixture_line": 1,
                    "imported_at": occurred_at,
                    "source_idempotency_key": source_idempotency_key,
                    "source_identity": source_identity,
                    "source_root": source_root,
                    "source_identity_key": source_identity_key,
                    "source_metadata": source_metadata,
                    "session_metadata": session_metadata,
                }),
            ),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: legacy_session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(legacy_source_id),
            provider: CaptureProvider::OpenHands,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: occurred_at,
            ended_at: Some(occurred_at),
            timestamps: timestamps(occurred_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": provider_session_id,
                    "parent_provider_session_id": null,
                    "root_provider_session_id": null,
                    "source_format": source_format,
                    "source_trust": "fixture",
                    "fixture_line": 1,
                    "imported_at": occurred_at,
                    "session_idempotency_key": session_idempotency_key,
                    "artifacts": [],
                    "metadata": session_metadata,
                }),
            ),
        })
        .unwrap();

    let event_identity = provider_source_event_import_identity(
        legacy_source_id,
        provider_event_index,
        provider_event_hash,
    );
    let legacy_event_id = event_identity.id;
    let event_type = if file_touch_path.is_some() {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    let role = if file_touch_path.is_some() {
        EventRole::Assistant
    } else {
        EventRole::User
    };
    let event_idempotency_key =
        format!("provider-event:openhands:{provider_session_id}:{provider_event_index}");
    assert_eq!(
        store
            .upsert_event(&Event {
                id: legacy_event_id,
                seq: event_identity.seq,
                history_record_id: None,
                session_id: Some(legacy_session_id),
                run_id: None,
                event_type,
                role: Some(role),
                occurred_at,
                capture_source_id: Some(legacy_source_id),
                payload: json!({
                    "provider": "openhands",
                    "provider_session_id": provider_session_id,
                    "provider_event_index": provider_event_index,
                    "provider_event_hash": provider_event_hash,
                    "cursor": cursor,
                    "artifacts": [],
                    "body": {
                        "text": payload_text,
                        "origin_main_fixture": true,
                    },
                }),
                payload_blob_id: None,
                dedupe_key: Some(event_identity.dedupe_key),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider_session_id": provider_session_id,
                        "provider_event_index": provider_event_index,
                        "provider_event_hash": provider_event_hash,
                        "provider_event_hash_authority": "provider_supplied",
                        "cursor": cursor,
                        "source_format": source_format,
                        "source_trust": "fixture",
                        "fixture_line": 1,
                        "imported_at": occurred_at,
                        "event_idempotency_key": event_idempotency_key,
                        "source_record_ordinal": null,
                        "source_record_subrecord_index": null,
                        "metadata": {
                            "event_path": canonical_event_path.display().to_string(),
                        },
                    }),
                ),
            })
            .unwrap(),
        legacy_event_id
    );

    if let Some(path) = file_touch_path {
        let touch_id = provider_file_touch_import_id(
            store,
            CaptureProvider::OpenHands,
            provider_session_id,
            legacy_source_id,
            Some(provider_event_index),
            0,
            false,
        )
        .unwrap();
        store
            .upsert_file_touched(&FileTouched {
                id: touch_id,
                history_record_id: None,
                run_id: None,
                event_id: Some(legacy_event_id),
                vcs_workspace_id: None,
                path: path.to_owned(),
                change_kind: Some(FileChangeKind::Modified),
                old_path: None,
                line_count_delta: Some(1),
                confidence: Confidence::Explicit,
                timestamps: timestamps(occurred_at),
                source_id: Some(legacy_source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": "openhands",
                        "provider_session_id": provider_session_id,
                        "provider_touch_index": 0,
                        "provider_event_index": provider_event_index,
                        "raw_source_path": conversation_dir,
                        "source_id": legacy_source_id,
                        "source_format": source_format,
                        "source_root": source_root,
                        "metadata": {"origin_main_fixture": true},
                        "session_id": legacy_session_id,
                    }),
                ),
            })
            .unwrap();
    }

    legacy_event_id
}

#[test]
fn native_openhands_c213_cursor_upgrade_stabilizes_source_without_duplicate_event_or_touch() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("v1_conversations")
        .join("conversation-legacy-exact");
    fs::create_dir_all(&conversation).unwrap();
    let event_path = conversation.join("0001-action.json");
    fs::write(
        &event_path,
        json!({
            "id": "legacy-exact-native-id",
            "timestamp": "2026-07-04T17:00:00Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "path": "src/legacy_upgrade.rs",
                "thought": "exact legacy tool event"
            },
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let legacy_event_id = import_origin_main_openhands_event(
        &mut store,
        &root,
        &event_path,
        "conversation-legacy-exact",
        (0, "legacy-exact-native-id"),
        "origin/main exact legacy event",
        Some("src/legacy_upgrade.rs"),
    );
    assert_eq!(
        store.get_event(legacy_event_id).unwrap().id,
        legacy_event_id
    );
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenHands,
        "conversation-legacy-exact",
    );
    let original_session = store.get_session(session_id).unwrap();
    let imported_at = "2026-07-04T17:05:00Z".parse().unwrap();
    seed_c213_openhands_terminal_cursor(&store, &event_path, "test-machine", imported_at).unwrap();
    fs::write(
        &event_path,
        json!({
            "id": "legacy-exact-native-id",
            "timestamp": "2026-07-04T18:30:00Z",
            "source": "agent",
            "observation": {"metadata": {"working_dir": "/workspace/rewritten"}},
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "path": "src/should_not_replace_legacy_touch.rs",
                "thought": "rewritten legacy event"
            },
        })
        .to_string(),
    )
    .unwrap();

    let summary = import_openhands_file_events(
        &event_path,
        &mut store,
        OpenHandsImportOptions {
            machine_id: "test-machine".to_owned(),
            source_path: Some(root.clone()),
            imported_at,
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.imported_events, 0);
    assert_eq!(summary.skipped_events, 1);
    let upgraded_session = store.get_session(session_id).unwrap();
    assert_eq!(upgraded_session.started_at, original_session.started_at);
    assert_eq!(upgraded_session.ended_at, original_session.ended_at);
    assert_eq!(
        upgraded_session.sync.metadata["metadata"],
        original_session.sync.metadata["metadata"]
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, legacy_event_id);
    assert!(events[0].payload.to_string().contains("origin/main exact"));
    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 1);
    assert_eq!(archive.files_touched[0].event_id, Some(legacy_event_id));
    assert_eq!(archive.files_touched[0].path, "src/legacy_upgrade.rs");

    let canonical_event_path = fs::canonicalize(&event_path).unwrap().display().to_string();
    let expected_source_root = root.display().to_string();
    let file_source = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| {
            source.descriptor.provider == CaptureProvider::OpenHands
                && source.descriptor.raw_source_path.as_deref()
                    == Some(canonical_event_path.as_str())
        })
        .expect("file-scoped OpenHands source after c213 cursor upgrade");
    assert_eq!(
        file_source.descriptor.source_root.as_deref(),
        Some(expected_source_root.as_str())
    );
    assert_eq!(
        file_source.sync.metadata["source_metadata"]["event_path"].as_str(),
        Some(canonical_event_path.as_str())
    );
    assert_eq!(
        store.get_session(session_id).unwrap().sync.metadata["metadata"],
        original_session.sync.metadata["metadata"]
    );
}

#[test]
fn native_openhands_stale_stable_cursor_rewrite_is_projection_free() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("v1_conversations")
        .join("conversation-stale-stable");
    fs::create_dir_all(&conversation).unwrap();
    let event_path = conversation.join("0001-action.json");
    fs::write(
        &event_path,
        json!({
            "id": "stable-native-id",
            "timestamp": "2026-07-04T17:00:00Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "path": "src/original.rs",
                "thought": "original stable event"
            },
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let imported_at = "2026-07-04T17:05:00Z".parse().unwrap();
    let initial = import_openhands_file_events(
        &event_path,
        &mut store,
        OpenHandsImportOptions {
            machine_id: "test-machine".to_owned(),
            source_path: Some(root.clone()),
            imported_at,
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(initial.imported_events, 1);

    let canonical_event_path = fs::canonicalize(&event_path).unwrap().display().to_string();
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenHands,
        "conversation-stale-stable",
    );
    let mut policy_one_event = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    policy_one_event.sync.metadata["metadata"]["legacy_source_event_candidate_v1"] = json!({
        "raw_source_path": conversation.display().to_string(),
        "provider_event_index": 0,
    });
    store.upsert_event(&policy_one_event).unwrap();
    let mut stale_source = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| {
            source.descriptor.provider == CaptureProvider::OpenHands
                && source.descriptor.raw_source_path.as_deref()
                    == Some(canonical_event_path.as_str())
        })
        .unwrap();
    stale_source.sync.metadata["source_metadata"]["event_path"] = json!("stale-policy-1");
    store.upsert_capture_source(&stale_source).unwrap();
    seed_c213_openhands_terminal_cursor(&store, &event_path, "test-machine", imported_at).unwrap();
    let unchanged_upgrade = import_openhands_file_events(
        &event_path,
        &mut store,
        OpenHandsImportOptions {
            machine_id: "test-machine".to_owned(),
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T17:10:00Z".parse().unwrap(),
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(unchanged_upgrade.skipped_events, 1);
    let upgraded_source = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| {
            source.descriptor.provider == CaptureProvider::OpenHands
                && source.descriptor.raw_source_path.as_deref()
                    == Some(canonical_event_path.as_str())
        })
        .unwrap();
    assert_eq!(
        upgraded_source.sync.metadata["source_metadata"]["event_path"].as_str(),
        Some(canonical_event_path.as_str())
    );

    seed_c213_openhands_terminal_cursor(&store, &event_path, "test-machine", imported_at).unwrap();
    let before_rewrite = store.export_archive().unwrap();

    fs::write(
        &event_path,
        json!({
            "id": "stable-native-id",
            "timestamp": "2026-07-04T18:30:00Z",
            "source": "agent",
            "observation": {"metadata": {"working_dir": "/workspace/rewritten"}},
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "path": "src/rewritten.rs",
                "thought": "rewritten stable event"
            },
        })
        .to_string(),
    )
    .unwrap();
    let upgraded = import_openhands_file_events(
        &event_path,
        &mut store,
        OpenHandsImportOptions {
            machine_id: "test-machine".to_owned(),
            source_path: Some(root),
            imported_at: "2026-07-04T18:35:00Z".parse().unwrap(),
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(upgraded.imported_events, 0);
    assert_eq!(upgraded.skipped_events, 1);
    assert_eq!(store.export_archive().unwrap(), before_rewrite);
}

#[test]
fn native_openhands_legacy_candidate_hash_mismatch_uses_stable_file_identity() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("v1_conversations")
        .join("conversation-legacy-mismatch");
    fs::create_dir_all(&conversation).unwrap();
    let event_path = conversation.join("0001-message.json");
    fs::write(
        &event_path,
        json!({
            "id": "new-file-native-id",
            "timestamp": "2026-07-04T17:00:00Z",
            "source": "user",
            "llm_message": {"role": "user", "content": "new stable file event"},
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let legacy_event_id = import_origin_main_openhands_event(
        &mut store,
        &root,
        &event_path,
        "conversation-legacy-mismatch",
        (0, "different-old-native-id"),
        "old ordinal occupant",
        None,
    );
    let original_legacy_event = store.get_event(legacy_event_id).unwrap();

    let summary = import_openhands_file_events(
        &event_path,
        &mut store,
        OpenHandsImportOptions {
            source_path: Some(root),
            imported_at: "2026-07-04T17:05:00Z".parse().unwrap(),
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.skipped_events, 0);
    assert_eq!(
        store.get_event(legacy_event_id).unwrap(),
        original_legacy_event
    );
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenHands,
        "conversation-legacy-mismatch",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    let new_event = events
        .iter()
        .find(|event| event.payload["provider_event_hash"].as_str() == Some("new-file-native-id"))
        .unwrap();
    assert_ne!(new_event.id, legacy_event_id);
    assert!(new_event
        .payload
        .to_string()
        .contains("new stable file event"));
}

#[test]
fn native_openhands_duplicate_prefix_reorder_requires_exact_legacy_event_path() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("v1_conversations")
        .join("conversation-legacy-reorder");
    fs::create_dir_all(&conversation).unwrap();
    let alpha = conversation.join("0001-alpha.json");
    let beta = conversation.join("0001-beta.json");
    for (path, timestamp, content) in [
        (&alpha, "2026-07-04T17:00:02Z", "new alpha file"),
        (&beta, "2026-07-04T17:00:01Z", "legacy beta file"),
    ] {
        fs::write(
            path,
            json!({
                "id": "duplicate-reordered-native-id",
                "timestamp": timestamp,
                "source": "user",
                "llm_message": {"role": "user", "content": content},
            })
            .to_string(),
        )
        .unwrap();
    }
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let legacy_beta_id = import_origin_main_openhands_event(
        &mut store,
        &root,
        &beta,
        "conversation-legacy-reorder",
        (0, "duplicate-reordered-native-id"),
        "origin/main beta event",
        None,
    );
    let original_beta = store.get_event(legacy_beta_id).unwrap();

    let alpha_summary = import_openhands_file_events(
        &alpha,
        &mut store,
        OpenHandsImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T17:05:00Z".parse().unwrap(),
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(alpha_summary.imported_events, 1);
    assert_eq!(alpha_summary.skipped_events, 0);
    assert_eq!(store.get_event(legacy_beta_id).unwrap(), original_beta);

    let beta_summary = import_openhands_file_events(
        &beta,
        &mut store,
        OpenHandsImportOptions {
            source_path: Some(root),
            imported_at: "2026-07-04T17:06:00Z".parse().unwrap(),
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(beta_summary.imported_events, 0);
    assert_eq!(beta_summary.skipped_events, 1);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenHands,
        "conversation-legacy-reorder",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| event.id == legacy_beta_id));
    let alpha_event = events
        .iter()
        .find(|event| event.payload.to_string().contains("new alpha file"))
        .unwrap();
    assert_ne!(alpha_event.id, legacy_beta_id);
    let canonical_alpha = fs::canonicalize(alpha).unwrap().display().to_string();
    assert_eq!(
        alpha_event.sync.metadata["metadata"]["event_path"].as_str(),
        Some(canonical_alpha.as_str())
    );
}

#[test]
fn native_openhands_file_events_redact_outputs_cite_source_and_leave_tree_readonly() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("user-a")
        .join("v1_conversations")
        .join("conversation-1");
    fs::create_dir_all(&conversation).unwrap();
    let raw_output = "OPENHANDS_RAW_COMMAND_OUTPUT_NEEDLE";
    let raw_old = "OPENHANDS_RAW_DIFF_OLD_NEEDLE";
    let raw_new = "OPENHANDS_RAW_DIFF_NEW_NEEDLE";
    let message_path = conversation.join("0001-message.json");
    let action_path = conversation.join("0002-action.json");
    let output_path = conversation.join("0003-output.json");
    fs::write(
        &message_path,
        json!({
            "id": "openhands-message-1",
            "timestamp": "2026-07-04T17:00:00Z",
            "source": "user",
            "llm_message": {
                "role": "user",
                "content": "openhands file event oracle prompt"
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        &action_path,
        json!({
            "id": "openhands-action-1",
            "timestamp": "2026-07-04T17:00:01Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "path": "src/openhands_policy.py",
                "diff": format!(
                    "diff --git a/src/openhands_policy.py b/src/openhands_policy.py\n@@\n- {raw_old}\n+ {raw_new}\n"
                )
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        &output_path,
        json!({
            "id": "openhands-output-1",
            "timestamp": "2026-07-04T17:00:02Z",
            "source": "environment",
            "observation": {
                "kind": "ExecuteBashObservation",
                "output": raw_output,
                "exit_code": 0
            }
        })
        .to_string(),
    )
    .unwrap();
    let before_tree = provider_source_snapshot(&root);
    let source = provider_source_for_path(CaptureProvider::OpenHands, root.clone());
    assert_eq!(source.source_format, "openhands_file_events");
    assert_eq!(source.status, ProviderSourceStatus::Available);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let (first, first_opens) = count_openhands_source_file_opens(|| {
        import_openhands_file_events(
            &root,
            &mut store,
            OpenHandsImportOptions {
                source_path: Some(root.clone()),
                imported_at: "2026-07-04T17:05:00Z".parse().unwrap(),
                ..OpenHandsImportOptions::default()
            },
        )
    });
    let first = first.unwrap();

    assert_eq!(provider_source_snapshot(&root), before_tree);
    assert_eq!(first_opens, 3);
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::OpenHands, "conversation-1");
    let session = store.get_session(session_id).unwrap();
    let original_session_metadata = session.sync.metadata["metadata"].clone();
    assert_eq!(
        session.started_at,
        "2026-07-04T17:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        session.ended_at,
        "2026-07-04T17:00:02Z".parse::<DateTime<Utc>>().ok()
    );
    let capture_source = store
        .get_capture_source(session.capture_source_id.unwrap())
        .unwrap();
    assert_eq!(capture_source.descriptor.cwd, None);
    assert_eq!(
        original_session_metadata,
        json!({
            "source_format": "openhands_file_events",
            "provider": "openhands",
            "conversation_id": "conversation-1",
            "user_id": "user-a",
        })
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    let imported_event_indices = events
        .iter()
        .map(|event| event.payload["provider_event_index"].as_u64().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(imported_event_indices.len(), 2);
    let action = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .expect("file editor action imported");
    let rendered_action = serde_json::to_string(action).unwrap();
    assert!(rendered_action.contains("src/openhands_policy.py"));
    assert!(!rendered_action.contains(raw_old));
    assert!(!rendered_action.contains(raw_new));
    assert!(events
        .iter()
        .all(|event| event.event_type != EventType::CommandOutput));
    assert!(!serde_json::to_string(&events).unwrap().contains(raw_output));
    assert!(store
        .search_event_hits("openhands file event oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::OpenHands)));
    assert!(store.search_event_hits(raw_output, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_old, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_new, 10).unwrap().is_empty());
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| {
            file.sync.metadata["provider"].as_str() == Some(CaptureProvider::OpenHands.as_str())
                && file.path == "src/openhands_policy.py"
                && file.confidence == Confidence::High
        }));
    let sources = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .filter(|source| source.descriptor.provider == CaptureProvider::OpenHands)
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 3);
    assert_eq!(
        sources
            .iter()
            .filter_map(|source| source.descriptor.raw_source_path.clone())
            .collect::<BTreeSet<_>>(),
        [&message_path, &action_path, &output_path]
            .into_iter()
            .map(|path| fs::canonicalize(path).unwrap().display().to_string())
            .collect()
    );
    let expected_source_root = root.display().to_string();
    for event in &events {
        let source_id = event.capture_source_id.unwrap();
        let source = store.get_capture_source(source_id).unwrap();
        let event_path = event.sync.metadata["metadata"]["event_path"]
            .as_str()
            .unwrap();
        let identity_index = event.sync.metadata["metadata"]["provider_event_identity_index"]
            .as_u64()
            .unwrap();
        assert_eq!(
            source.descriptor.raw_source_path.as_deref(),
            Some(event_path)
        );
        assert_eq!(
            source.descriptor.source_root.as_deref(),
            Some(expected_source_root.as_str())
        );
        assert!(event.payload["cursor"]
            .as_str()
            .unwrap()
            .starts_with(event_path));
        assert_eq!(
            event.id,
            provider_source_event_uuid(source.id, identity_index)
        );
    }
    let original_ids = events
        .iter()
        .map(|event| {
            (
                event.payload["provider_event_hash"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                event.id,
            )
        })
        .collect::<BTreeSet<_>>();

    let (second, second_opens) = count_openhands_source_file_opens(|| {
        import_openhands_file_events(
            &root,
            &mut store,
            OpenHandsImportOptions {
                source_path: Some(root.clone()),
                imported_at: "2026-07-04T17:06:00Z".parse().unwrap(),
                ..OpenHandsImportOptions::default()
            },
        )
    });
    let second = second.unwrap();
    assert_eq!(second_opens, 3, "direct wrapper hashes each selected file");
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 3);
    assert_eq!(second.skipped_events, 3);

    let before_same_native_rewrite = store.export_archive().unwrap();
    fs::write(
        &action_path,
        json!({
            "id": "openhands-action-1",
            "timestamp": "2026-07-04T18:30:00Z",
            "source": "agent",
            "observation": {
                "metadata": {
                    "working_dir": "/workspace/changed-metadata",
                    "revision": "rewritten"
                }
            },
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "path": "src/should_not_be_touched.rs",
                "thought": "changed file-local payload"
            }
        })
        .to_string(),
    )
    .unwrap();
    let (changed, changed_opens) = count_openhands_source_file_opens(|| {
        import_openhands_file_events(
            &action_path,
            &mut store,
            OpenHandsImportOptions {
                source_path: Some(root.clone()),
                imported_at: "2026-07-04T17:07:00Z".parse().unwrap(),
                ..OpenHandsImportOptions::default()
            },
        )
    });
    let changed = changed.unwrap();
    assert_eq!(changed_opens, 1);
    assert_eq!(changed.failed, 0, "{:?}", changed.failures);
    assert_eq!(changed.skipped_events, 1);
    assert_eq!(store.export_archive().unwrap(), before_same_native_rewrite);
    let after_touch = store.events_for_session(session_id).unwrap();
    assert_eq!(
        store.get_session(session_id).unwrap().sync.metadata["metadata"],
        original_session_metadata
    );
    assert_eq!(
        after_touch
            .iter()
            .filter_map(|event| {
                let hash = event.payload["provider_event_hash"].as_str()?;
                original_ids
                    .iter()
                    .any(|(original_hash, _)| original_hash == hash)
                    .then_some((hash.to_owned(), event.id))
            })
            .collect::<BTreeSet<_>>(),
        original_ids
    );

    let earlier_path = conversation.join("0000-earlier.json");
    fs::write(
        &earlier_path,
        json!({
            "id": "openhands-earlier-1",
            "timestamp": "2026-07-04T16:59:59Z",
            "source": "user",
            "llm_message": {"role": "user", "content": "earlier event"}
        })
        .to_string(),
    )
    .unwrap();
    let added = import_openhands_file_events(
        &earlier_path,
        &mut store,
        OpenHandsImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T17:08:00Z".parse().unwrap(),
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(added.imported_events, 1);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    let after_add = store.events_for_session(session_id).unwrap();
    assert_eq!(after_add.len(), 3);
    assert_eq!(
        store
            .list_capture_sources()
            .unwrap()
            .into_iter()
            .filter(|source| source.descriptor.provider == CaptureProvider::OpenHands)
            .count(),
        4
    );
    assert_eq!(
        store.get_session(session_id).unwrap().started_at,
        "2026-07-04T16:59:59Z".parse::<DateTime<Utc>>().unwrap()
    );
    for (hash, id) in original_ids {
        assert_eq!(
            after_add
                .iter()
                .find(|event| {
                    event.payload["provider_event_hash"].as_str() == Some(hash.as_str())
                })
                .map(|event| event.id),
            Some(id)
        );
    }
}

#[test]
fn native_openhands_identity_and_shared_state_ignore_order_and_changed_subsets() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("user-order")
        .join("v1_conversations")
        .join("conversation-order");
    fs::create_dir_all(&conversation).unwrap();
    let alpha = conversation.join("0001-alpha.json");
    let beta = conversation.join("0001-beta.json");
    let anchor = conversation.join("0042-anchor.json");
    let write_event = |path: &Path, id: &str, timestamp: &str, content: &str, cwd: &str| {
        fs::write(
            path,
            json!({
                "id": id,
                "timestamp": timestamp,
                "source": "user",
                "llm_message": {"role": "user", "content": content},
                "observation": {"metadata": {"working_dir": cwd}},
            })
            .to_string(),
        )
        .unwrap();
    };
    write_event(
        &alpha,
        "duplicate-native-id",
        "2026-07-04T17:00:03Z",
        "alpha original content",
        "/workspace/alpha",
    );
    write_event(
        &beta,
        "duplicate-native-id",
        "2026-07-04T17:00:01Z",
        "beta content",
        "/workspace/beta",
    );
    write_event(
        &anchor,
        "gapped-anchor",
        "2026-07-04T17:00:02Z",
        "anchor content",
        "/workspace/anchor",
    );
    let imported_at = "2026-07-04T17:05:00Z".parse().unwrap();
    let mut first_store = Store::open(temp.path().join("first.sqlite")).unwrap();
    let mut second_store = Store::open(temp.path().join("second.sqlite")).unwrap();
    let import = |store: &mut Store, path: &Path| {
        import_openhands_file_events(
            path,
            store,
            OpenHandsImportOptions {
                source_path: Some(root.clone()),
                imported_at,
                ..OpenHandsImportOptions::default()
            },
        )
        .unwrap()
    };

    for path in [&alpha, &beta, &anchor] {
        assert_eq!(import(&mut first_store, path).failed, 0);
    }
    for path in [&beta, &alpha, &anchor] {
        assert_eq!(import(&mut second_store, path).failed, 0);
    }

    let session_id = stored_provider_session_id(
        &first_store,
        CaptureProvider::OpenHands,
        "conversation-order",
    );
    assert_eq!(
        session_id,
        stored_provider_session_id(
            &second_store,
            CaptureProvider::OpenHands,
            "conversation-order",
        )
    );
    let first_events = first_store.events_for_session(session_id).unwrap();
    let second_events = second_store.events_for_session(session_id).unwrap();
    assert_eq!(first_events, second_events);
    assert_eq!(first_events.len(), 3);
    assert_eq!(
        first_events
            .iter()
            .map(|event| event.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    let duplicate_native_events = first_events
        .iter()
        .filter(|event| {
            event.payload["provider_event_hash"].as_str() == Some("duplicate-native-id")
        })
        .collect::<Vec<_>>();
    assert_eq!(duplicate_native_events.len(), 2);
    assert_ne!(
        duplicate_native_events[0].payload["provider_event_index"],
        duplicate_native_events[1].payload["provider_event_index"]
    );
    assert_ne!(duplicate_native_events[0].id, duplicate_native_events[1].id);
    assert_eq!(
        first_store.get_session(session_id).unwrap(),
        second_store.get_session(session_id).unwrap()
    );
    assert_eq!(
        first_store.list_capture_sources().unwrap(),
        second_store.list_capture_sources().unwrap()
    );

    write_event(
        &alpha,
        "duplicate-native-id",
        "2026-07-04T17:00:03Z",
        "alpha changed content",
        "/workspace/changed",
    );
    let changed = import(&mut first_store, &alpha);
    assert_eq!(changed.skipped_events, 1);
    let changed_session = first_store.get_session(session_id).unwrap();
    let untouched_session = second_store.get_session(session_id).unwrap();
    let changed_source = first_store
        .get_capture_source(changed_session.capture_source_id.unwrap())
        .unwrap();
    let untouched_source = second_store
        .get_capture_source(untouched_session.capture_source_id.unwrap())
        .unwrap();
    assert_eq!(
        changed_source.descriptor.cwd,
        untouched_source.descriptor.cwd
    );
    assert_eq!(
        changed_session.sync.metadata["metadata"],
        untouched_session.sync.metadata["metadata"]
    );
    assert_eq!(import(&mut first_store, &anchor).skipped_events, 1);
    assert_eq!(import(&mut second_store, &anchor).skipped_events, 1);

    assert_eq!(
        first_store.get_session(session_id).unwrap(),
        second_store.get_session(session_id).unwrap()
    );
    assert_eq!(
        first_store.list_capture_sources().unwrap(),
        second_store.list_capture_sources().unwrap()
    );
    assert_eq!(
        first_store.events_for_session(session_id).unwrap(),
        second_store.events_for_session(session_id).unwrap()
    );
    let rendered =
        serde_json::to_string(&first_store.events_for_session(session_id).unwrap()).unwrap();
    assert!(rendered.contains("alpha original content"));
    assert!(!rendered.contains("alpha changed content"));
    assert!(!rendered.contains("/workspace/changed"));
}

#[test]
fn native_openhands_directory_wrapper_keeps_sibling_rejections_record_local() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root.join("v1_conversations/conversation-bounded");
    fs::create_dir_all(&conversation).unwrap();
    fs::write(
        conversation.join("0001-message.json"),
        json!({
            "id": "openhands-valid",
            "timestamp": "2026-07-04T17:00:00Z",
            "source": "user",
            "llm_message": {"role": "user", "content": "valid bounded sibling"}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(conversation.join("0002-malformed.json"), b"{not-json").unwrap();
    fs::write(
        conversation.join("0003-oversize.json"),
        vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1],
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let (summary, opens) = count_openhands_source_file_opens(|| {
        import_openhands_file_events(
            &root,
            &mut store,
            OpenHandsImportOptions {
                source_path: Some(root.clone()),
                ..OpenHandsImportOptions::default()
            },
        )
    });
    let summary = summary.unwrap();

    assert_eq!(
        opens, 2,
        "oversize records must not be opened for raw reads"
    );
    assert_eq!(summary.failed, 2, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::OpenHands, "conversation-bounded");
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 1);
}
