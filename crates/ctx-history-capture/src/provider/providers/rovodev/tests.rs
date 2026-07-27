use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched, Session,
    SessionStatus,
};
use ctx_history_store::Store;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    native_source::NativePosition,
    provider::{
        importer::{
            certified_provider_sync_cursor, provider_event_import_identity,
            provider_file_touch_import_id, provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, BoundedParserCheckpoint,
            CertifiedProviderCursor,
        },
        normalization::provider_message_id,
    },
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderImportSummary,
    ProviderImportWorkResult, ROVODEV_SOURCE_FORMAT,
};

use super::{
    import_rovodev_native_path, native_path::ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS,
    rovodev_result_content,
};
use crate::{CaptureWorkLimit, ProviderAdapterContext, ProviderImportOptions};

const MACHINE: &str = "rovodev-nativepath-test";
const OUTPUT_SENTINEL: &str = "rovodev-success-output-must-never-enter-core";

#[test]
fn result_profile_selects_only_explicit_tool_result_parts() {
    let message = json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "not a result"},
            {"type": "tool_result", "content": [
                {"text": "first"},
                {"output": "second"}
            ]}
        ]
    });
    assert_eq!(
        rovodev_result_content(&message).as_deref(),
        Some("first\nsecond")
    );
}

#[test]
fn nativepath_lifecycle_covers_append_rewrite_truncation_replacement_and_disappearance() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let session = write_session(
        &root,
        "lifecycle",
        &[
            message("user", "initial-user"),
            message("assistant", "initial-assistant"),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    let original_event = first_rovodev_event(&store);
    let original_source_id = store
        .authorized_source_route_for_event(original_event)
        .expect("initial source route")
        .capture_source_id();
    assert_eq!(
        original_source_id,
        provider_scoped_source_uuid(
            CaptureProvider::RovoDev,
            "lifecycle",
            ROVODEV_SOURCE_FORMAT,
            Some(&session.join("session_context.json").display().to_string()),
        ),
        "generation zero must preserve the released path-scoped source UUID"
    );
    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

    drop(store);
    let mut store = Store::open(&store_path).expect("restart store");
    let restart = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);

    let context_bytes =
        fs::read(session.join("session_context.json")).expect("same-content context");
    let metadata_bytes = fs::read(session.join("metadata.json")).expect("same-content metadata");
    fs::remove_dir_all(&session).expect("remove original physical source");
    fs::create_dir_all(&session).expect("recreate source directory");
    fs::write(session.join("session_context.json"), context_bytes).expect("restore context");
    fs::write(session.join("metadata.json"), metadata_bytes).expect("restore metadata");
    let same_content_replacement = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        same_content_replacement.work_result(),
        ProviderImportWorkResult::Changed
    );
    let replacement_source_id = store
        .authorized_source_route_for_event(first_authorized_rovodev_event(&store))
        .expect("replacement source route")
        .capture_source_id();
    assert_ne!(replacement_source_id, original_source_id);
    assert!(store
        .authorized_source_route_for_event(original_event)
        .is_err());
    let replacement_event = first_authorized_rovodev_event(&store);

    write_context(
        &session,
        "lifecycle",
        &[
            message("user", "initial-user"),
            message("assistant", "initial-assistant"),
            message("assistant", "append-suffix"),
        ],
    );
    let append = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "append-suffix");
    assert_eq!(
        store
            .authorized_source_route_for_event(replacement_event)
            .expect("append keeps the replacement route")
            .capture_source_id(),
        replacement_source_id
    );

    write_context(
        &session,
        "lifecycle",
        &[
            message("user", "rewrite-user"),
            message("assistant", "rewrite-assistant"),
        ],
    );
    let rewrite = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "rewrite-assistant");
    assert!(store
        .authorized_source_route_for_event(replacement_event)
        .is_err());
    let rewrite_event = first_authorized_rovodev_event(&store);
    let rewrite_source_id = store
        .authorized_source_route_for_event(rewrite_event)
        .expect("rewrite route")
        .capture_source_id();
    assert_ne!(rewrite_source_id, replacement_source_id);

    write_context(&session, "lifecycle", &[message("user", "truncated")]);
    let truncation = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(truncation.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "truncated");
    assert!(store
        .authorized_source_route_for_event(rewrite_event)
        .is_err());

    fs::remove_dir_all(&session).expect("remove old physical source");
    let replacement = write_session(
        &root,
        "lifecycle",
        &[
            message("user", "replacement-user"),
            message("assistant", "replacement-assistant"),
        ],
    );
    let replaced = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replaced.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "replacement-assistant");
    let final_event = first_authorized_rovodev_event(&store);

    fs::remove_dir_all(replacement).expect("remove source");
    let disappeared = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(final_event)
        .is_err());
    assert!(store
        .get_event(final_event)
        .expect("retired route preserves committed event")
        .sync
        .deleted_at
        .is_none());
    assert_search(&store, "replacement-assistant");
    let retired_noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(retired_noop.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn malformed_message_members_reject_locally_and_valid_siblings_continue() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    write_session(
        &root,
        "mixed",
        &[
            message("user", "valid-before-malformed"),
            Value::Null,
            json!(17),
            json!("not-a-message"),
            message("assistant", "valid-after-malformed"),
        ],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let first = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    assert_eq!(first.failed, 3);
    assert!(first
        .failures
        .iter()
        .all(|failure| failure.error.contains("member must be an object")));
    assert_search(&store, "valid-before-malformed");
    assert_search(&store, "valid-after-malformed");
    let session = store
        .session_by_external_session(CaptureProvider::RovoDev, "mixed")
        .expect("session lookup")
        .expect("mixed session");
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        2
    );

    let replay = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.failed, 3);
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        2
    );
}

#[test]
fn verified_complete_content_locators_are_message_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let complete = "m".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 1);
    write_session(
        &root,
        "message-locators",
        &[
            message("assistant", &complete),
            json!({
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "call-read",
                        "name": "read_file",
                        "input": {"file_path": "src/locator.rs"}
                    },
                    {"type": "text", "text": complete}
                ]
            }),
        ],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let summary = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    let session = store
        .session_by_external_session(CaptureProvider::RovoDev, "message-locators")
        .expect("session lookup")
        .expect("locator session");
    let events = store.events_for_session(session.id).expect("events");
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .find(|event| event.event_type == EventType::Message)
        .expect("message event")
        .sync
        .metadata
        .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_some());
    assert!(events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .expect("tool-call event")
        .sync
        .metadata
        .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
}

#[test]
fn released_v025_unchanged_and_append_migrate_exact_event_and_touch_ids() {
    for append_before_upgrade in [false, true] {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("sessions");
        let baseline = vec![
            message(
                "user",
                "*** Begin Patch\n*** Update File: src/released.rs\n@@\n-old\n+new\n*** End Patch",
            ),
            message("assistant", "released-v025-assistant"),
        ];
        let session = write_session(&root, "released-v025", &baseline);
        let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
        let released = seed_released_v025_projection(&root, &session, &baseline, &store);

        if append_before_upgrade {
            let mut appended = baseline.clone();
            appended.push(message("assistant", "nativepath-append"));
            write_context(&session, "released-v025", &appended);
        }

        let migrated = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(migrated.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(migrated.imported_events, usize::from(append_before_upgrade));
        let events = store
            .events_for_session(released.session_id)
            .expect("migrated events");
        assert_eq!(
            events.len(),
            baseline.len() + usize::from(append_before_upgrade)
        );
        assert!(events.iter().any(|event| event.id == released.event_id));
        assert_eq!(
            store
                .get_event(released.event_id)
                .expect("released event")
                .capture_source_id,
            Some(released.source_id)
        );
        assert_eq!(
            store
                .authorized_source_route_for_event(released.event_id)
                .expect("migrated released route")
                .capture_source_id(),
            released.source_id
        );
        let archive = store.export_archive().expect("archive");
        let migrated_touch = archive
            .files_touched
            .iter()
            .find(|touch| touch.id == released.touch_id)
            .expect("released touch");
        assert_eq!(migrated_touch.event_id, Some(released.event_id));
        assert_eq!(migrated_touch.source_id, Some(released.source_id));
        assert_eq!(
            archive
                .capture_sources
                .iter()
                .filter(|source| source.descriptor.provider == CaptureProvider::RovoDev)
                .count(),
            1,
            "generation-zero migration must not create a replacement source"
        );

        let stable = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(stable.work_result(), ProviderImportWorkResult::NoOp);
        assert_eq!(
            store
                .events_for_session(released.session_id)
                .expect("stable events")
                .len(),
            baseline.len() + usize::from(append_before_upgrade)
        );
    }
}

#[test]
fn core_commit_output_failure_and_later_output_replay_are_independent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    write_session(
        &root,
        "outputs",
        &[
            message("user", "core-first-user"),
            successful_output(OUTPUT_SENTINEL),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let failing = Arc::new(RecordingSink::new(store_path.clone(), true));

    let core = import(
        &root,
        &mut store,
        ImportProfile::CoreAndPro(failing.clone()),
    );
    assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
    assert!(core.failed > 0);
    assert_core_excludes_output(&store);
    assert!(failing.saw_committed_core.load(Ordering::SeqCst));

    let replay_sink = Arc::new(RecordingSink::new(store_path, false));
    let replay = import(
        &root,
        &mut store,
        ImportProfile::ProReplayOnly(replay_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay_sink.outputs.load(Ordering::SeqCst), 1);
    assert_eq!(
        replay_sink.contents.lock().expect("contents").as_slice(),
        [OUTPUT_SENTINEL.as_bytes()]
    );
}

#[test]
fn output_progress_appends_without_replaying_committed_output_prefixes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let session = write_session(
        &root,
        "output-append",
        &[
            message("user", "first-core"),
            successful_output("first-output"),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let sink = Arc::new(RecordingSink::new(store_path, false));

    let first = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);

    write_context(
        &session,
        "output-append",
        &[
            message("user", "first-core"),
            successful_output("first-output"),
            message("assistant", "second-core"),
            successful_output("second-output"),
        ],
    );
    let append = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 2);
    assert_eq!(
        sink.contents.lock().expect("contents").as_slice(),
        [b"first-output".as_slice(), b"second-output".as_slice()]
    );
}

#[test]
fn corrupt_and_over_budget_sources_commit_only_diagnostics_then_recover() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let corrupt = root.join("corrupt");
    fs::create_dir_all(&corrupt).expect("session directory");
    fs::write(corrupt.join("session_context.json"), b"{not-json").expect("corrupt source");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let rejected = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(rejected.failed, 1);
    assert!(store.list_sessions().expect("sessions").is_empty());
    let replay = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(replay.failed, 1);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);

    write_context(&corrupt, "corrupt", &[message("user", "recovered-source")]);
    let recovered = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(recovered.work_result(), ProviderImportWorkResult::Changed);
    assert_search(&store, "recovered-source");
    let recovered_event = first_authorized_rovodev_event(&store);

    fs::write(corrupt.join("session_context.json"), b"{broken-again")
        .expect("break a formerly valid source");
    let invalidated = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(invalidated.work_result(), ProviderImportWorkResult::Changed);
    assert!(invalidated.failed > 0);
    assert!(store
        .authorized_source_route_for_event(recovered_event)
        .is_err());
    write_context(
        &corrupt,
        "corrupt",
        &[message("user", "recovered-source-again")],
    );
    let recovered_again = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(
        recovered_again.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_search(&store, "recovered-source-again");

    let over_budget = root.join("over-budget");
    fs::create_dir_all(&over_budget).expect("over-budget directory");
    fs::write(
        over_budget.join("session_context.json"),
        serde_json::to_vec(&json!({
            "session_id": "over-budget",
            "message_history": [message("user", "must-not-project")],
            "adversarial": vec![Value::Null; ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS]
        }))
        .expect("over-budget JSON"),
    )
    .expect("over-budget source");
    let bounded = import(&root, &mut store, ImportProfile::CoreOnly);
    assert!(bounded.failed > 0);
    assert!(store
        .search_event_hits("must-not-project", 10)
        .expect("search")
        .is_empty());
}

#[test]
fn one_safe_group_resumes_without_republishing_committed_prefixes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let messages = (0..140)
        .map(|index| message("assistant", &format!("bounded-message-{index}")))
        .collect::<Vec<_>>();
    write_session(&root, "bounded", &messages);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    let mut calls = 0_usize;
    loop {
        calls = calls.saturating_add(1);
        let summary = import_with_limit(
            &root,
            &mut store,
            ImportProfile::CoreOnly,
            CaptureWorkLimit::OneSafeGroup,
        );
        if !summary.work_remaining {
            break;
        }
        assert!(calls < 10, "bounded import did not converge");
    }
    assert!(calls >= 3);
    let session = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::RovoDev)
        .expect("RovoDev session");
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        messages.len()
    );
}

#[test]
fn one_safe_group_persists_disappearance_authority_with_the_first_core_page() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("sessions");
    let messages = (0..140)
        .map(|index| message("assistant", &format!("interrupted-message-{index}")))
        .collect::<Vec<_>>();
    write_session(&root, "interrupted", &messages);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let first = import_with_limit(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        CaptureWorkLimit::OneSafeGroup,
    );
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert!(first.work_remaining);
    let event = first_rovodev_event(&store);

    fs::remove_dir_all(&root).expect("remove root between bounded calls");
    let retired = import_with_limit(
        &root,
        &mut store,
        ImportProfile::CoreOnly,
        CaptureWorkLimit::OneSafeGroup,
    );
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(!retired.work_remaining);
    assert!(store.authorized_source_route_for_event(event).is_err());
    assert!(store
        .get_event(event)
        .expect("root retirement preserves committed bounded event")
        .sync
        .deleted_at
        .is_none());
    assert_search(&store, "interrupted-message-0");
    let stable = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(stable.work_result(), ProviderImportWorkResult::NoOp);
}

struct RecordingSink {
    store_path: PathBuf,
    fail: bool,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
    outputs: AtomicUsize,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail: bool) -> Self {
        Self {
            store_path,
            fail,
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
            outputs: AtomicUsize::new(0),
            saw_committed_core: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "rovodev-nativepath-test-v1"
    }

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().expect("progress").get(source).cloned())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_committed_core.store(true, Ordering::SeqCst);
        }
        if self.fail {
            return Err(ProOutputSinkError::new(
                "injected",
                "injected output failure",
            ));
        }
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        self.contents.lock().expect("contents").extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().expect("progress").insert(
            page.source.clone(),
            ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            },
        );
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).expect("bounded outputs"),
            materialized_facts: 0,
            replayed: false,
        })
    }
}

struct ReleasedV025Projection {
    source_id: Uuid,
    session_id: Uuid,
    event_id: Uuid,
    touch_id: Uuid,
}

fn seed_released_v025_projection(
    root: &Path,
    session_dir: &Path,
    messages: &[Value],
    store: &Store,
) -> ReleasedV025Projection {
    let imported_at = "2026-07-25T12:00:00Z"
        .parse()
        .expect("released import time");
    let started_at = "2026-07-25T11:00:00Z"
        .parse()
        .expect("released session time");
    let provider_session_id = "released-v025";
    let context_path = session_dir.join("session_context.json");
    let raw_source_path = context_path.display().to_string();
    let source_root = root.display().to_string();
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::RovoDev,
        provider_session_id,
        ROVODEV_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    let path_identity =
        provider_path_identity(&fs::canonicalize(&context_path).expect("canonical context"))
            .expect("path identity");
    let native_source_identity = format!("rovodev-session:{path_identity}");
    let canonical_source_identity = provider_source_identity(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        Some(&native_source_identity),
        &json!({"native_source_id": native_source_identity}),
    )
    .expect("canonical source identity");
    store
        .upsert_capture_source(&CaptureSource {
            id: source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::RovoDev,
                machine_id: MACHINE.to_owned(),
                process_id: None,
                cwd: Some("/workspace/rovodev".to_owned()),
                raw_source_path: Some(raw_source_path.clone()),
                source_format: Some(ROVODEV_SOURCE_FORMAT.to_owned()),
                source_root: Some(source_root),
                source_identity: Some(canonical_source_identity.clone()),
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at,
            ended_at: None,
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": provider_session_id,
                    "source_format": ROVODEV_SOURCE_FORMAT,
                    "source_identity": canonical_source_identity,
                }),
            ),
        })
        .expect("released source");
    let session_id = provider_import_session_uuid(
        store,
        CaptureProvider::RovoDev,
        provider_session_id,
        source_id,
        Some(&canonical_source_identity),
    )
    .expect("released session id");
    store
        .upsert_session(&Session {
            id: session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::RovoDev,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at,
            ended_at: None,
            timestamps: timestamps(imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": provider_session_id,
                    "source_format": ROVODEV_SOURCE_FORMAT,
                }),
            ),
        })
        .expect("released session");

    let mut released_event_id = None;
    for (index, message) in messages.iter().enumerate() {
        let provider_event_index = u64::try_from(index).expect("bounded test event index");
        let event_hash = provider_message_id(message, provider_event_index);
        let identity = provider_event_import_identity(
            store,
            CaptureProvider::RovoDev,
            provider_session_id,
            source_id,
            provider_event_index,
            provider_event_index,
            &event_hash,
            None,
            false,
        )
        .expect("released event identity");
        let role = match message.get("role").and_then(Value::as_str) {
            Some("assistant") => EventRole::Assistant,
            Some("system") => EventRole::System,
            _ => EventRole::User,
        };
        store
            .upsert_event(&Event {
                id: identity.id,
                seq: identity.seq,
                history_record_id: None,
                session_id: Some(session_id),
                run_id: None,
                event_type: EventType::Message,
                role: Some(role),
                occurred_at: started_at,
                capture_source_id: Some(source_id),
                payload: json!({
                    "provider": CaptureProvider::RovoDev.as_str(),
                    "provider_session_id": provider_session_id,
                    "provider_event_index": provider_event_index,
                    "provider_event_hash": event_hash,
                    "body": message,
                }),
                payload_blob_id: None,
                dedupe_key: Some(identity.dedupe_key),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider_session_id": provider_session_id,
                        "provider_event_index": provider_event_index,
                        "provider_event_hash": event_hash,
                        "source_format": ROVODEV_SOURCE_FORMAT,
                    }),
                ),
            })
            .expect("released event");
        released_event_id.get_or_insert(identity.id);
    }
    let event_id = released_event_id.expect("released fixture event");
    let touch_id = provider_file_touch_import_id(
        store,
        CaptureProvider::RovoDev,
        provider_session_id,
        source_id,
        Some(0),
        0,
        false,
    )
    .expect("released touch identity");
    store
        .upsert_file_touched(&FileTouched {
            id: touch_id,
            history_record_id: None,
            run_id: None,
            event_id: Some(event_id),
            vcs_workspace_id: None,
            path: "src/released.rs".to_owned(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(started_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::RovoDev.as_str(),
                    "provider_session_id": provider_session_id,
                    "provider_event_index": 0,
                    "provider_touch_index": 0,
                    "source_id": source_id,
                    "source_format": ROVODEV_SOURCE_FORMAT,
                }),
            ),
        })
        .expect("released touch");

    let released_cursor = CertifiedProviderCursor::new(
        "sha256:released-v025-rovodev",
        3,
        6,
        NativePosition::new("whole-json-item-v1", 1_u64.to_be_bytes().to_vec())
            .expect("released whole-JSON position"),
        BoundedParserCheckpoint::from_serializable(&json!({
            "next_ordinal": 1,
            "accepted_sessions": 1,
            "accepted_events": messages.len(),
            "accepted_file_touches": 1,
            "rejected_records": 0,
            "failures": [],
        }))
        .expect("released parser checkpoint"),
    )
    .expect("released certified cursor");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        &path_identity,
    );
    store
        .upsert_sync_cursor(
            &certified_provider_sync_cursor(
                CaptureProvider::RovoDev,
                MACHINE,
                stream,
                &released_cursor,
                imported_at,
            )
            .expect("released sync cursor"),
        )
        .expect("store released cursor");
    ReleasedV025Projection {
        source_id,
        session_id,
        event_id,
        touch_id,
    }
}

fn import(root: &Path, store: &mut Store, profile: ImportProfile) -> ProviderImportSummary {
    import_with_limit(root, store, profile, CaptureWorkLimit::Drain)
}

fn import_with_limit(
    root: &Path,
    store: &mut Store,
    profile: ImportProfile,
    capture_work_limit: CaptureWorkLimit,
) -> ProviderImportSummary {
    import_rovodev_native_path(
        root,
        store,
        ProviderAdapterContext {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            source_root: None,
            imported_at: "2026-07-25T12:00:00Z".parse().expect("timestamp"),
        },
        ProviderImportOptions {
            capture_work_limit,
            import_profile: profile,
            ..ProviderImportOptions::default()
        },
    )
    .expect("RovoDev NativePath import")
}

fn write_session(root: &Path, id: &str, messages: &[Value]) -> PathBuf {
    let session = root.join(id);
    fs::create_dir_all(&session).expect("session directory");
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "session_id": id,
            "created_at": "2026-07-25T11:00:00Z",
            "workspace_path": "/workspace/rovodev"
        }))
        .expect("metadata"),
    )
    .expect("metadata file");
    write_context(&session, id, messages);
    session
}

fn write_context(session: &Path, id: &str, messages: &[Value]) {
    fs::write(
        session.join("session_context.json"),
        serde_json::to_vec(&json!({
            "session_id": id,
            "message_history": messages
        }))
        .expect("context"),
    )
    .expect("context file");
}

fn message(role: &str, content: &str) -> Value {
    json!({"role": role, "content": content})
}

fn successful_output(content: &str) -> Value {
    json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": "rovodev-call",
            "content": content,
            "status": "success"
        }]
    })
}

fn first_rovodev_event(store: &Store) -> uuid::Uuid {
    store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::RovoDev)
        .and_then(|session| {
            store
                .events_for_session(session.id)
                .expect("events")
                .into_iter()
                .next()
        })
        .map(|event| event.id)
        .expect("RovoDev event")
}

fn first_authorized_rovodev_event(store: &Store) -> uuid::Uuid {
    store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::RovoDev)
        .flat_map(|session| store.events_for_session(session.id).expect("events"))
        .find(|event| store.authorized_source_route_for_event(event.id).is_ok())
        .map(|event| event.id)
        .expect("authorized RovoDev event")
}

fn assert_search(store: &Store, needle: &str) {
    assert!(store
        .search_event_hits(needle, 10)
        .expect("search")
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::RovoDev)));
}

fn assert_core_excludes_output(store: &Store) {
    let events = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::RovoDev)
        .flat_map(|session| store.events_for_session(session.id).expect("events"))
        .collect::<Vec<_>>();
    assert!(!serde_json::to_string(&events)
        .expect("event JSON")
        .contains(OUTPUT_SENTINEL));
    assert!(store
        .search_event_hits(OUTPUT_SENTINEL, 10)
        .expect("output search")
        .is_empty());
}
