use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::{TimeZone, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;

use super::*;
use crate::{
    ImportProfile, ProOutputMaterializationPage, ProOutputPageResult, ProviderAdapterContext,
    ProviderImportOptions,
};

const FIXTURE_INDEX: &[u8] = include_bytes!(
    "../../../../../../../tests/fixtures/provider-history/junie/sessions/index.jsonl"
);
const FIXTURE_EVENTS: &[u8] = include_bytes!(
    "../../../../../../../tests/fixtures/provider-history/junie/sessions/session-260607-100000-acme/events.jsonl"
);

fn write_fixture(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("materialized fixture");
}

fn materialized_fixture_events() -> (tempfile::TempDir, PathBuf) {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let path = temp.path().join("events.jsonl");
    write_fixture(&path, FIXTURE_EVENTS);
    (temp, path)
}

fn initial_frontier() -> Frontier {
    let started = Utc
        .timestamp_millis_opt(1_783_339_200_000)
        .single()
        .expect("fixture timestamp");
    Frontier {
        offset: 0,
        next_ordinal: 0,
        next_event_index: 0,
        prefix_sha256: Sha256::digest([]).into(),
        state: RuntimeState {
            started_at_ms: started.timestamp_millis(),
            last_ts_ms: started.timestamp_millis(),
            ended_at_ms: None,
            title: Some("Junie fixture task".to_owned()),
            cwd: Some("/workspace/junie-fixture".to_owned()),
            saw_supported_event: false,
        },
        pending: None,
    }
}

#[test]
fn successful_output_is_transient_and_absent_from_core_rows() {
    let (_fixture, path) = materialized_fixture_events();
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    assert_eq!(first.rows.len(), 1);
    assert!(first.outputs.is_empty());
    assert!(!first.terminal);

    let second_frontier = Frontier {
        offset: first.end_offset,
        next_ordinal: first.end_ordinal,
        next_event_index: first.next_event_index,
        prefix_sha256: first.after_prefix_sha256,
        state: first.after_state,
        pending: None,
    };
    let second = parse_turn(&path, &second_frontier).expect("terminal safe turn");
    assert!(second.terminal);
    assert_eq!(second.outputs.len(), 1);
    assert_eq!(
        second.outputs[0].content,
        b"JUNIE_TERMINAL_OUTPUT saffron harbor"
    );
    assert!(second.rows.iter().all(|row| {
        !row.text.contains("JUNIE_TERMINAL_OUTPUT")
            && !row.body.to_string().contains("JUNIE_TERMINAL_OUTPUT")
    }));
}

#[test]
fn output_only_event_indexes_still_advance_the_core_frontier() {
    let (_fixture, path) = materialized_fixture_events();
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    let second = parse_turn(
        &path,
        &Frontier {
            offset: first.end_offset,
            next_ordinal: first.end_ordinal,
            next_event_index: first.next_event_index,
            prefix_sha256: first.after_prefix_sha256,
            state: first.after_state,
            pending: None,
        },
    )
    .expect("terminal safe turn");
    assert_eq!(
        second.next_event_index - second.base_event_index,
        second.rows.len() as u64 + second.outputs.len() as u64
    );
}

#[test]
fn pending_output_page_replay_is_bound_to_the_exact_turn() {
    let (_fixture, path) = materialized_fixture_events();
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    let frontier = Frontier {
        offset: first.end_offset,
        next_ordinal: first.end_ordinal,
        next_event_index: first.next_event_index,
        prefix_sha256: first.after_prefix_sha256,
        state: first.after_state,
        pending: None,
    };
    let parsed = parse_turn(&path, &frontier).expect("terminal safe turn");
    let mut pending_frontier = frontier;
    pending_frontier.pending = Some(PendingTurn {
        start_offset: parsed.start_offset,
        end_offset: parsed.end_offset,
        start_ordinal: parsed.start_ordinal,
        end_ordinal: parsed.end_ordinal,
        base_event_index: parsed.base_event_index,
        next_event_index: parsed.next_event_index,
        next_row: 0,
        row_count: parsed.outputs.len() as u32,
        turn_sha256: parsed.turn_sha256,
        terminal: parsed.terminal,
        after_state: parsed.after_state.clone(),
        after_prefix_sha256: parsed.after_prefix_sha256,
    });
    validate_output_pending_replay(&pending_frontier, &parsed).expect("exact replay");
    pending_frontier
        .pending
        .as_mut()
        .expect("pending")
        .turn_sha256[0] ^= 1;
    assert!(matches!(
        validate_output_pending_replay(&pending_frontier, &parsed),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn append_after_a_pending_terminal_turn_does_not_change_its_replay() {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let path = temp.path().join("events.jsonl");
    write_fixture(&path, FIXTURE_EVENTS);
    let first = parse_turn(&path, &initial_frontier()).expect("first safe turn");
    let turn_frontier = Frontier {
        offset: first.end_offset,
        next_ordinal: first.end_ordinal,
        next_event_index: first.next_event_index,
        prefix_sha256: first.after_prefix_sha256,
        state: first.after_state,
        pending: None,
    };
    let terminal = parse_turn(&path, &turn_frontier).expect("terminal turn");
    let mut pending_frontier = turn_frontier;
    pending_frontier.pending = Some(PendingTurn {
        start_offset: terminal.start_offset,
        end_offset: terminal.end_offset,
        start_ordinal: terminal.start_ordinal,
        end_ordinal: terminal.end_ordinal,
        base_event_index: terminal.base_event_index,
        next_event_index: terminal.next_event_index,
        next_row: 1,
        row_count: terminal.rows.len() as u32,
        turn_sha256: terminal.turn_sha256,
        terminal: true,
        after_state: terminal.after_state.clone(),
        after_prefix_sha256: terminal.after_prefix_sha256,
    });
    let mut append = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append source");
    writeln!(
        append,
        "{}",
        json!({"kind": "UserPromptEvent", "prompt": "appended after pending page"})
    )
    .expect("append prompt");
    drop(append);

    let replay = parse_turn(&path, &pending_frontier).expect("bounded pending replay");
    validate_pending_replay(&pending_frontier, &replay).expect("same pending turn");
    assert_eq!(replay.end_offset, terminal.end_offset);
    assert_eq!(replay.next_event_index, terminal.next_event_index);
}

#[test]
fn native_store_path_is_idempotent_and_handles_append_rewrite_and_deletion() {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let root = temp.path().join("sessions");
    let session_id = "session-260607-100000-acme";
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).expect("session directory");
    write_fixture(&root.join("index.jsonl"), FIXTURE_INDEX);
    let events_path = session_dir.join("events.jsonl");
    write_fixture(&events_path, FIXTURE_EVENTS);

    let context = ProviderAdapterContext {
        machine_id: "junie-nativepath-test-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: Utc
            .timestamp_millis_opt(1_783_339_500_000)
            .single()
            .expect("import timestamp"),
    };
    let options = ProviderImportOptions::default();
    let mut store = Store::open(temp.path().join("history.sqlite")).expect("store");

    let first = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("initial import");
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 4);
    let session = store
        .session_by_external_session(CaptureProvider::Junie, session_id)
        .expect("session query")
        .expect("Junie session");
    let events = store.events_for_session(session.id).expect("events");
    assert!(!events.iter().any(|event| {
        matches!(
            event.event_type,
            EventType::ToolOutput | EventType::CommandOutput
        )
    }));
    assert!(!serde_json::to_string(&events)
        .expect("events JSON")
        .contains("JUNIE_TERMINAL_OUTPUT"));

    let replay = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("idempotent replay");
    assert_eq!(replay.imported_events, 0);
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        4
    );

    let mut append = fs::OpenOptions::new()
        .append(true)
        .open(&events_path)
        .expect("append source");
    writeln!(
        append,
        "{}",
        json!({
            "kind": "SessionA2uxEvent",
            "timestampMs": 1_783_339_450_000_i64,
            "event": {"agentEvent": {
                "kind": "ResultBlockUpdatedEvent",
                "stepId": "appended-result",
                "result": "JUNIE_APPENDED_RESULT"
            }}
        })
    )
    .expect("append result");
    writeln!(
        append,
        "{}",
        json!({"kind": "UserPromptEvent", "prompt": "JUNIE_APPENDED_USER"})
    )
    .expect("append prompt");
    append.sync_all().expect("sync append");
    drop(append);
    let appended = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("append import");
    assert_eq!(appended.imported_events, 2);
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        6
    );

    fs::write(
        &events_path,
        b"{\"kind\":\"UserPromptEvent\",\"prompt\":\"JUNIE_REPLACEMENT_USER\"}\n",
    )
    .expect("rewrite source");
    let rewritten = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("rewrite import");
    assert_eq!(rewritten.imported_events, 1);
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        7
    );

    fs::remove_file(&events_path).expect("remove source");
    let retired = import_junie_nativepath(&root, &mut store, context.clone(), options.clone())
        .expect("route retirement");
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    let retired_again =
        import_junie_nativepath(&root, &mut store, context, options).expect("retirement replay");
    assert_eq!(retired_again.work_result(), ProviderImportWorkResult::NoOp);
}

struct RecordingSink {
    store_path: PathBuf,
    fail: AtomicBool,
    behind: AtomicUsize,
    progress: Mutex<Option<ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail: bool) -> Self {
        Self {
            store_path,
            fail: AtomicBool::new(fail),
            behind: AtomicUsize::new(0),
            progress: Mutex::new(None),
            contents: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "junie-nativepath-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().expect("progress").clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "intentional_test_failure",
                "Junie Pro output test failure",
            ));
        }
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            return Err(ProOutputSinkError::new(
                "core_not_committed",
                "Junie output page arrived before Core committed",
            ));
        }
        self.contents.lock().expect("contents").extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().expect("progress") = Some(ProOutputProgress {
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
            accepted_outputs: u32::try_from(page.observations.len()).expect("bounded outputs"),
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn pro_failure_does_not_roll_back_core_and_later_activation_replays_output() {
    let temp = crate::test_support_paths::tempdir().expect("temporary directory");
    let root = temp.path().join("sessions");
    let session_dir = root.join("session-260607-100000-acme");
    fs::create_dir_all(&session_dir).expect("session directory");
    write_fixture(&root.join("index.jsonl"), FIXTURE_INDEX);
    write_fixture(&session_dir.join("events.jsonl"), FIXTURE_EVENTS);
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).expect("store");
    let context = ProviderAdapterContext {
        machine_id: "junie-nativepath-pro-test-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: Utc
            .timestamp_millis_opt(1_783_339_500_000)
            .single()
            .expect("import timestamp"),
    };
    let sink = Arc::new(RecordingSink::new(store_path, true));
    let core = import_junie_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions {
            import_profile: ImportProfile::CoreAndPro(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .expect("Core survives Pro failure");
    assert_eq!(core.imported_events, 4);
    assert!(sink.behind.load(Ordering::SeqCst) > 0);
    let session = store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.provider == CaptureProvider::Junie)
        .expect("Junie session");
    assert_eq!(
        store.events_for_session(session.id).expect("events").len(),
        4
    );

    sink.fail.store(false, Ordering::SeqCst);
    let replay = import_junie_nativepath(
        &root,
        &mut store,
        context,
        ProviderImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .expect("later Pro activation");
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(
        sink.contents.lock().expect("contents").as_slice(),
        [b"JUNIE_TERMINAL_OUTPUT saffron harbor".as_slice()]
    );
}
