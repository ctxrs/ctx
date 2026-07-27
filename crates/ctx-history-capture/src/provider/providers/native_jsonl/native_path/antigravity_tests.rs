use std::{
    fs,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::EventType;
use serde_json::{json, Value};

use super::super::reader::DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD;
use super::*;
use crate::{
    test_support_paths::tempdir, AntigravityCliImportOptions, ProOutputMaterializationPage,
    ProOutputPageResult,
};

const MACHINE: &str = "antigravity-nativepath-test-machine";

#[test]
fn released_positional_identity_and_hash_survive_upgrade_and_reorder() {
    const STABLE_TEXT: &str = "ANTIGRAVITY_RELEASED_EVENT_MUST_STAY_STABLE";

    let temp = tempdir().unwrap();
    let root = temp.path().join("brain");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            record(0, "USER_INPUT", STABLE_TEXT),
            record(1, "PLANNER_RESPONSE", "released-assistant"),
        ],
    );

    let mut donor = Store::open(temp.path().join("released-donor.sqlite")).unwrap();
    import(&root, &mut donor, ImportProfile::CoreOnly);
    let source = donor
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.descriptor.provider == CaptureProvider::Antigravity)
        .unwrap();
    let donor_session = donor
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Antigravity)
        .unwrap();
    let donor_events = donor.events_for_session(donor_session.id).unwrap();
    drop(donor);

    let store_path = temp.path().join("released-upgrade.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    store.upsert_capture_source(&source).unwrap();
    let legacy_session_id =
        crate::provider::importer::provider_session_uuid(CaptureProvider::Antigravity, "agy-life");
    let mut released_session = donor_session;
    released_session.id = legacy_session_id;
    store.upsert_session(&released_session).unwrap();

    let mut released_ids = BTreeMap::new();
    for mut event in donor_events {
        let raw_ordinal = event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap();
        let released_hash = format!("step-{raw_ordinal}");
        let identity = crate::provider::importer::provider_source_event_import_identity(
            source.id,
            raw_ordinal,
            &released_hash,
        );
        event.id = identity.id;
        event.seq = identity.seq;
        event.session_id = Some(legacy_session_id);
        event.dedupe_key = Some(identity.dedupe_key);
        event.payload["provider_event_index"] = json!(raw_ordinal);
        event.payload["provider_event_hash"] = json!(released_hash);
        event.sync.metadata["provider_event_index"] = json!(raw_ordinal);
        event.sync.metadata["provider_event_hash"] = json!(released_hash);
        event.sync.metadata["provider_event_hash_authority"] = json!("provider_supplied");
        released_ids.insert(raw_ordinal, event.id);
        store.upsert_event(&event).unwrap();
    }

    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let upgraded = store.events_for_session(legacy_session_id).unwrap();
    assert_eq!(upgraded.len(), 2);
    let stable_id = released_ids[&0];
    let stable = store.get_event(stable_id).unwrap();
    assert!(stable.payload.to_string().contains(STABLE_TEXT));
    assert_eq!(
        stable.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_ne!(provider_event_hash(&stable), "step-0");

    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );
    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::NoOp
    );

    write_transcript(
        &transcript,
        &[
            record(2, "USER_INPUT", "inserted before released"),
            record(0, "USER_INPUT", STABLE_TEXT),
            record(1, "PLANNER_RESPONSE", "released-assistant"),
        ],
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let reordered = store.events_for_session(legacy_session_id).unwrap();
    assert_eq!(reordered.len(), 3);
    assert_eq!(
        reordered
            .iter()
            .find(|event| event.payload.to_string().contains(STABLE_TEXT))
            .unwrap()
            .id,
        stable_id
    );
    assert_eq!(
        reordered
            .iter()
            .filter(|event| event
                .payload
                .to_string()
                .contains("inserted before released"))
            .count(),
        1
    );
}

#[test]
fn production_lifecycle_covers_all_source_changes_and_retires_disappearance() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("brain");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            record(0, "USER_INPUT", "fresh-user"),
            record(1, "PLANNER_RESPONSE", "fresh-assistant"),
            tool_call(2, "write README"),
        ],
    );
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 3);
    let session = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Antigravity)
        .unwrap();
    let original_events = store.events_for_session(session.id).unwrap();
    assert_eq!(original_events.len(), 3);
    assert!(original_events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    )));
    let original_user = original_events[0].clone();
    let original_assistant = original_events[1].clone();
    let original_user_hash = provider_event_hash(&original_user).to_owned();
    let original_assistant_hash = provider_event_hash(&original_assistant).to_owned();
    let routed_event = original_events[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let previous = checkpoint(&store, &transcript);
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Unchanged
    );
    let noop = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);

    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    let restart = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(restart.work_result(), ProviderImportWorkResult::NoOp);

    let previous = checkpoint(&store, &transcript);
    append_record(&transcript, &record(3, "PLANNER_RESPONSE", "append"));
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Append
    );
    let append = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(append.imported_events, 1);

    let previous = checkpoint(&store, &transcript);
    write_transcript(
        &transcript,
        &[
            record(0, "USER_INPUT", &"rewrite-user-content-".repeat(24)),
            record(
                1,
                "PLANNER_RESPONSE",
                &"rewrite-assistant-content-".repeat(24),
            ),
        ],
    );
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Rewrite
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );
    let rewritten_user = store.get_event(original_user.id).unwrap();
    let rewritten_assistant = store.get_event(original_assistant.id).unwrap();
    assert_eq!(rewritten_user.id, original_user.id);
    assert_eq!(rewritten_user.seq, original_user.seq);
    assert_eq!(rewritten_assistant.id, original_assistant.id);
    assert_eq!(rewritten_assistant.seq, original_assistant.seq);
    assert!(rewritten_user
        .payload
        .to_string()
        .contains("rewrite-user-content"));
    assert!(!rewritten_user.payload.to_string().contains("fresh-user"));
    assert!(rewritten_assistant
        .payload
        .to_string()
        .contains("rewrite-assistant-content"));
    assert!(!rewritten_assistant
        .payload
        .to_string()
        .contains("fresh-assistant"));
    assert_ne!(
        provider_event_hash(&rewritten_assistant),
        original_assistant_hash
    );
    assert_ne!(provider_event_hash(&rewritten_user), original_user_hash);
    assert_eq!(
        rewritten_user.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_eq!(
        rewritten_assistant.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert!(rewritten_user
        .dedupe_key
        .as_deref()
        .unwrap()
        .ends_with(provider_event_hash(&rewritten_user)));
    assert_eq!(
        store
            .search_event_hits("rewrite-user-content", 10)
            .unwrap()
            .iter()
            .map(|hit| hit.event_id)
            .collect::<Vec<_>>(),
        [original_user.id]
    );
    assert!(store
        .search_event_hits("fresh-user", 10)
        .unwrap()
        .is_empty());

    let previous = checkpoint(&store, &transcript);
    write_transcript(&transcript, &[record(0, "USER_INPUT", "short")]);
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Truncation
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    let previous = checkpoint(&store, &transcript);
    let replacement = transcript.with_extension("replacement");
    write_transcript(
        &replacement,
        &[record(0, "USER_INPUT", "replacement-generation")],
    );
    fs::rename(&replacement, &transcript).unwrap();
    assert_eq!(
        classify(&transcript, &root, &previous),
        DirectJsonlSourceChange::Replacement
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    let repeated = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn production_keeps_every_live_session_route_and_retires_only_missing_sessions() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("brain");
    let first_transcript = transcript_path_for(&root, "first-session");
    let second_transcript = transcript_path_for(&root, "second-session");
    write_transcript(
        &first_transcript,
        &[record(0, "USER_INPUT", "first-live-session")],
    );
    write_transcript(
        &second_transcript,
        &[record(0, "USER_INPUT", "second-live-session")],
    );
    let mut store = Store::open(temp.path().join("routes.sqlite")).unwrap();

    let imported = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(imported.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(imported.imported_sessions, 2);
    assert_eq!(imported.imported_events, 2);
    let sessions = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::Antigravity)
        .collect::<Vec<_>>();
    assert_eq!(sessions.len(), 2);
    let first = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("first-session"))
        .unwrap();
    let second = sessions
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("second-session"))
        .unwrap();
    let first_event = store.events_for_session(first.id).unwrap()[0].id;
    let second_event = store.events_for_session(second.id).unwrap()[0].id;
    assert!(store.authorized_source_route_for_event(first_event).is_ok());
    assert!(store
        .authorized_source_route_for_event(second_event)
        .is_ok());

    fs::remove_dir_all(root.join("first-session")).unwrap();
    let retired = import(&root, &mut store, ImportProfile::CoreOnly);
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(first_event)
        .is_err());
    assert!(store
        .authorized_source_route_for_event(second_event)
        .is_ok());
}

#[test]
fn production_dedupes_bounded_touches_and_rejects_oversized_record_before_store() {
    let temp = tempdir().unwrap();
    let bounded_root = temp.path().join("bounded/brain");
    let bounded_transcript = transcript_path(&bounded_root);
    let mut bounded_paths = (0..DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD)
        .map(|index| format!("src/bounded-{index}.rs"))
        .collect::<Vec<_>>();
    bounded_paths.extend(["src/bounded-0.rs".to_owned(), "src/bounded-0.rs".to_owned()]);
    write_transcript(
        &bounded_transcript,
        &[
            record(0, "USER_INPUT", "bounded-header"),
            tool_call_with_paths(1, &bounded_paths),
        ],
    );
    let mut bounded_store = Store::open(temp.path().join("bounded-store.sqlite")).unwrap();

    let bounded = import(&bounded_root, &mut bounded_store, ImportProfile::CoreOnly);
    assert_eq!(bounded.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(bounded.failed, 0);
    assert_eq!(bounded.imported_events, 2);
    assert_eq!(
        bounded_store
            .file_touch_scope("src/bounded-0.rs")
            .unwrap()
            .event_ids
            .len(),
        1
    );
    assert_eq!(
        bounded_store
            .file_touch_scope(&format!(
                "src/bounded-{}.rs",
                DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD - 1
            ))
            .unwrap()
            .event_ids
            .len(),
        1
    );

    let oversized_root = temp.path().join("oversized/brain");
    let oversized_transcript = transcript_path(&oversized_root);
    let oversized_paths = (0..=DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD)
        .map(|index| format!("src/oversized-{index}.rs"))
        .collect::<Vec<_>>();
    write_transcript(
        &oversized_transcript,
        &[
            record(0, "USER_INPUT", "oversized-header"),
            tool_call_with_paths(1, &oversized_paths),
        ],
    );
    let mut oversized_store = Store::open(temp.path().join("oversized-store.sqlite")).unwrap();

    let rejected = import(
        &oversized_root,
        &mut oversized_store,
        ImportProfile::CoreOnly,
    );
    assert_eq!(rejected.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(rejected.failed, 1);
    assert_eq!(rejected.imported_events, 1);
    assert!(rejected.failures[0].error.contains(&format!(
        "{} unique file-touch transaction bound",
        DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD
    )));
    assert!(oversized_store
        .file_touch_scope("src/oversized-0.rs")
        .unwrap()
        .event_ids
        .is_empty());
}

#[test]
fn production_is_core_first_with_independent_pro_replay() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("brain");
    let transcript = transcript_path(&root);
    write_transcript(
        &transcript,
        &[
            record(0, "USER_INPUT", "core-first"),
            tool_call(1, "call-only-no-output-body"),
        ],
    );
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let sink = Arc::new(RecordingSink::new(store_path.clone()));

    let fresh = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert!(sink.saw_core_before_page.load(Ordering::SeqCst));
    assert!(sink.pages.load(Ordering::SeqCst) > 0);
    assert_eq!(sink.outputs.load(Ordering::SeqCst), 0);
    let pages_after_fresh = sink.pages.load(Ordering::SeqCst);

    let noop = import(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), pages_after_fresh);

    let pro_only_path = temp.path().join("pro-only.sqlite");
    let mut pro_only_store = Store::open(&pro_only_path).unwrap();
    let pro_only_sink = Arc::new(RecordingSink::new(pro_only_path));
    let replay = import(
        &root,
        &mut pro_only_store,
        ImportProfile::ProReplayOnly(pro_only_sink.clone()),
    );
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert!(pro_only_store.list_sessions().unwrap().is_empty());
    assert!(!pro_only_sink.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(pro_only_sink.pages.load(Ordering::SeqCst), 0);
    assert_eq!(pro_only_sink.outputs.load(Ordering::SeqCst), 0);
}

struct RecordingSink {
    store_path: PathBuf,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    outputs: AtomicUsize,
    saw_core_before_page: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            outputs: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "antigravity-nativepath-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
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
            self.saw_core_before_page.store(true, Ordering::SeqCst);
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.outputs
            .fetch_add(page.observations.len(), Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }
}

fn import(root: &Path, store: &mut Store, import_profile: ImportProfile) -> ProviderImportSummary {
    crate::import_antigravity_cli_history(
        root,
        store,
        AntigravityCliImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile,
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap()
}

fn transcript_path(root: &Path) -> PathBuf {
    transcript_path_for(root, "agy-life")
}

fn transcript_path_for(root: &Path, session_id: &str) -> PathBuf {
    root.join(session_id)
        .join(".system_generated/logs/transcript_full.jsonl")
}

fn record(step: u64, kind: &str, content: &str) -> Value {
    json!({
        "step_index": step,
        "source": if kind == "USER_INPUT" { "user" } else { "planner" },
        "type": kind,
        "status": "ok",
        "created_at": format!("2026-07-25T12:00:{step:02}Z"),
        "content": content,
    })
}

fn tool_call(step: u64, content: &str) -> Value {
    json!({
        "step_index": step,
        "source": "planner",
        "type": "CODE_ACTION",
        "status": "ok",
        "created_at": format!("2026-07-25T12:00:{step:02}Z"),
        "content": content,
        "tool_calls": [{"name": "write_to_file", "args": {"TargetFile": "README.md"}}],
    })
}

fn tool_call_with_paths(step: u64, paths: &[String]) -> Value {
    let tool_calls = paths
        .iter()
        .map(|path| {
            json!({
                "name": "write_to_file",
                "args": {"TargetFile": path},
            })
        })
        .collect::<Vec<_>>();
    json!({
        "step_index": step,
        "source": "planner",
        "type": "CODE_ACTION",
        "status": "ok",
        "created_at": format!("2026-07-25T12:00:{step:02}Z"),
        "content": "bounded file writes",
        "tool_calls": tool_calls,
    })
}

fn provider_event_hash(event: &ctx_history_core::Event) -> &str {
    event.payload["provider_event_hash"].as_str().unwrap()
}

fn write_transcript(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_record(path: &Path, record: &Value) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}

fn checkpoint(store: &Store, path: &Path) -> DirectJsonlCheckpoint {
    let canonical = fs::canonicalize(path).unwrap();
    let locator = provider_path_identity(&canonical).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        &locator,
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    decode_direct_jsonl_native_cursor(
        &cursor.cursor,
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
    )
    .unwrap()
}

fn classify(path: &Path, root: &Path, previous: &DirectJsonlCheckpoint) -> DirectJsonlSourceChange {
    open_direct_jsonl_pages(
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        path,
        Some(root.to_path_buf()),
        "2026-07-25T12:01:00Z".parse().unwrap(),
        false,
        Some(previous),
    )
    .unwrap()
    .source_change()
}
