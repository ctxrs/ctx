use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection};

use super::publication::{
    WarpNativeEvent, WarpNativePage, WarpNativeProOutputPage, WarpNativeProOutputPageReceipt,
    WarpNativeScanOutcome, WarpNativeSession, WarpNativeSink, WARP_NATIVE_PAGE_MAX_BYTES,
    WARP_NATIVE_PAGE_MAX_ROWS,
};
use super::*;
use crate::provider::sqlite::ProviderSqliteSourceSnapshot;
use crate::test_support_paths::tempdir;
use crate::MAX_PROVIDER_SQLITE_VALUE_BYTES;

#[derive(Default)]
struct CollectingSink {
    pages: Vec<WarpNativePage>,
    pro_pages: Vec<WarpNativeProOutputPage>,
    pro_receipts: Vec<WarpNativeProOutputPageReceipt>,
}

#[derive(Clone)]
struct AttemptedPage {
    identity: publication::WarpNativePageIdentity,
    expected_frontier: publication::WarpNativeFrontier,
    next_frontier: publication::WarpNativeFrontier,
}

struct CrashSink {
    fail_on_call: usize,
    calls: usize,
    committed: Vec<WarpNativePage>,
    attempted: Option<AttemptedPage>,
}

impl CrashSink {
    fn new(fail_on_call: usize) -> Self {
        Self {
            fail_on_call,
            calls: 0,
            committed: Vec::new(),
            attempted: None,
        }
    }
}

impl WarpNativeSink for CrashSink {
    fn push_page(&mut self, page: WarpNativePage) -> Result<()> {
        self.calls = self.calls.saturating_add(1);
        if self.calls == self.fail_on_call {
            self.attempted = Some(AttemptedPage {
                identity: page.identity,
                expected_frontier: page.expected_frontier.clone(),
                next_frontier: page.next_safe_frontier.clone(),
            });
            return Err(CaptureError::SystemInvariant(
                "injected Warp page commit crash",
            ));
        }
        self.committed.push(page);
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        page.receipt()
    }
}

impl WarpNativeSink for CollectingSink {
    fn push_page(&mut self, page: WarpNativePage) -> Result<()> {
        self.pages.push(page);
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        let receipt = page.receipt();
        self.pro_pages.push(page);
        self.pro_receipts.push(receipt.clone());
        receipt
    }
}

impl CollectingSink {
    fn sessions(&self) -> Vec<&WarpNativeSession> {
        self.pages
            .iter()
            .flat_map(|page| page.sessions.iter())
            .collect()
    }

    fn events(&self) -> Vec<&WarpNativeEvent> {
        self.pages
            .iter()
            .flat_map(|page| page.events.iter())
            .collect()
    }

    fn outputs(&self) -> Vec<&crate::ProOutputObservation> {
        self.pro_pages
            .iter()
            .flat_map(|page| page.outputs.iter())
            .collect()
    }

    fn output_rejections(&self) -> Vec<&publication::WarpNativeOutputRejection> {
        self.pro_pages
            .iter()
            .flat_map(|page| page.rejections.iter())
            .collect()
    }

    fn rejections(&self) -> Vec<&publication::WarpNativeRejection> {
        self.pages
            .iter()
            .flat_map(|page| page.rejections.iter())
            .collect()
    }

    fn rejection_count(&self) -> usize {
        self.rejections().len()
    }
}

#[derive(Default)]
struct DiscardingSink {
    pages: usize,
    sessions: usize,
    hierarchy_edges: usize,
    events: usize,
    rejections: usize,
    max_page_rows: usize,
    max_page_bytes: usize,
    pro_pages: usize,
    pro_outputs: usize,
    max_pro_page_rows: usize,
    max_pro_page_bytes: usize,
}

impl WarpNativeSink for DiscardingSink {
    fn push_page(&mut self, page: WarpNativePage) -> Result<()> {
        self.pages = self.pages.saturating_add(1);
        self.sessions = self.sessions.saturating_add(page.sessions.len());
        self.hierarchy_edges = self
            .hierarchy_edges
            .saturating_add(page.hierarchy_edges.len());
        self.events = self.events.saturating_add(page.events.len());
        self.rejections = self.rejections.saturating_add(page.rejections.len());
        self.max_page_rows = self.max_page_rows.max(page.row_count());
        self.max_page_bytes = self.max_page_bytes.max(page.estimated_bytes);
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        let receipt = page.receipt();
        self.pro_pages = self.pro_pages.saturating_add(1);
        self.pro_outputs = self.pro_outputs.saturating_add(page.outputs.len());
        self.max_pro_page_rows = self.max_pro_page_rows.max(page.logical_unit_count());
        self.max_pro_page_bytes = self.max_pro_page_bytes.max(page.estimated_bytes);
        receipt
    }
}

fn complete(outcome: WarpNativeScanOutcome) -> publication::WarpNativeSourceAuthority {
    match outcome {
        WarpNativeScanOutcome::Complete(authority) => authority,
        WarpNativeScanOutcome::Incomplete(incomplete) => {
            panic!("expected complete Warp source, got {incomplete:?}")
        }
    }
}

fn create_schema(conn: &Connection) {
    conn.execute_batch(
        "pragma user_version = 1;
         create table agent_conversations (
             id integer primary key,
             conversation_id text not null unique,
             conversation_data text not null,
             last_modified_at text not null
         );
         create table agent_tasks (
             id integer primary key,
             conversation_id text not null,
             task_id text not null unique,
             task blob not null,
             last_modified_at text not null
         );
         create table ai_queries (
             id integer primary key,
             exchange_id text not null unique,
             conversation_id text not null,
             start_ts text not null,
             input text not null,
             working_directory text,
             output_status text not null,
             model_id text not null,
             planning_model_id text not null default '',
             coding_model_id text not null default ''
         );",
    )
    .unwrap();
}

fn insert_conversation(
    conn: &Connection,
    conversation_id: &str,
    parent: Option<&str>,
    agent_name: &str,
) {
    let parent = parent.map_or_else(String::new, |value| {
        format!(r#","parent_conversation_id":"{value}""#)
    });
    let data =
        format!(r#"{{"agent_name":"{agent_name}","run_id":"run-{conversation_id}"{parent}}}"#);
    conn.execute(
        "insert into agent_conversations
         (conversation_id, conversation_data, last_modified_at)
         values (?1, ?2, '2026-07-24 12:00:00')",
        params![conversation_id, data],
    )
    .unwrap();
}

fn insert_task(conn: &Connection, conversation_id: &str, task_id: &str, messages: &[Vec<u8>]) {
    let task = task(task_id, messages);
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values (?1, ?2, ?3, '2026-07-24 12:00:01')",
        params![conversation_id, task_id, task],
    )
    .unwrap();
}

fn scan(path: &Path) -> (publication::WarpNativeSourceAuthority, CollectingSink) {
    scan_profile(path, WarpNativeProfile::CoreOnly)
}

fn scan_profile(
    path: &Path,
    profile: WarpNativeProfile,
) -> (publication::WarpNativeSourceAuthority, CollectingSink) {
    let mut sink = CollectingSink::default();
    let outcome = scan_warp_nativepath_with_profile(path, profile, &mut sink).unwrap();
    (complete(outcome), sink)
}

fn field(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3 | 2);
    value.extend(varint(payload.len() as u64));
    value.extend_from_slice(payload);
    value
}

fn integer_field(number: u32, integer: u64) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3);
    value.extend(varint(integer));
    value
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut output = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            return output;
        }
    }
}

fn timestamp(sequence: u64) -> Vec<u8> {
    let mut value = integer_field(1, 1_782_259_200 + sequence);
    value.extend(integer_field(2, sequence % 1_000));
    value
}

fn message(
    message_id: &str,
    task_id: &str,
    request_id: &str,
    sequence: u64,
    arms: &[Vec<u8>],
) -> Vec<u8> {
    let mut value = field(1, message_id.as_bytes());
    for arm in arms {
        value.extend_from_slice(arm);
    }
    value.extend(field(11, task_id.as_bytes()));
    value.extend(field(13, request_id.as_bytes()));
    value.extend(field(14, &timestamp(sequence)));
    value
}

fn text_arm(field_number: u32, text: &str) -> Vec<u8> {
    field(field_number, &field(1, text.as_bytes()))
}

fn tool_call_arm(tool_field: u32) -> Vec<u8> {
    field(4, &field(tool_field, &[]))
}

fn tool_result_arm(call_id: &str, output: &[u8]) -> Vec<u8> {
    let finished = field(1, output);
    let run_shell = field(5, &finished);
    let mut result = field(1, call_id.as_bytes());
    result.extend(field(2, &run_shell));
    field(5, &result)
}

fn tool_result_last_wins_arm(call_id: &str, stale: &[u8], selected: &[u8]) -> Vec<u8> {
    let mut result = field(1, call_id.as_bytes());
    result.extend(field(4, &field(1, stale)));
    let finished = field(1, selected);
    result.extend(field(2, &field(5, &finished)));
    field(5, &result)
}

fn tool_result_variant_arm(call_id: &str, variant: u32, result_payload: &[u8]) -> Vec<u8> {
    let mut result = field(1, call_id.as_bytes());
    result.extend(field(variant, result_payload));
    field(5, &result)
}

fn task(task_id: &str, messages: &[Vec<u8>]) -> Vec<u8> {
    let mut value = field(1, task_id.as_bytes());
    value.extend(field(2, format!("Task {task_id}").as_bytes()));
    for message in messages {
        value.extend(field(5, message));
    }
    value.extend(field(6, format!("Summary {task_id}").as_bytes()));
    value
}

#[test]
fn immutable_snapshot_observes_committed_wal_without_mutating_provider_files() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("pragma journal_mode = wal; pragma wal_autocheckpoint = 0;")
        .unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-wal", None, "Before WAL");
    insert_task(
        &conn,
        "conversation-wal",
        "task-wal",
        &[message(
            "message-wal",
            "task-wal",
            "request-wal",
            1,
            &[text_arm(2, "WAL message")],
        )],
    );
    conn.execute_batch("pragma wal_checkpoint(truncate);")
        .unwrap();
    conn.execute(
        "update agent_conversations
         set conversation_data =
             '{\"agent_name\":\"Committed only in WAL\",\"run_id\":\"run-wal\"}'
         where conversation_id = 'conversation-wal'",
        [],
    )
    .unwrap();
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    assert!(fs::metadata(&wal_path).unwrap().len() > 32);

    let observed = ProviderSqliteSourceSnapshot::read(
        &path,
        WARP_SOURCE_INVALID_REASON,
        WARP_SIDECAR_INVALID_REASON,
    )
    .unwrap();
    let (authority, sink) = scan(&path);
    assert!(authority.source_complete);
    assert_eq!(sink.sessions().len(), 1);
    assert_eq!(sink.sessions()[0].title, "Committed only in WAL");
    assert!(observed.revalidate(&path).unwrap());
    assert!(fs::metadata(&wal_path).unwrap().len() > 32);
    drop(conn);
}

#[test]
fn wal_snapshot_sidecars_are_ephemeral_and_cleaned_after_scan() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("pragma journal_mode = wal; pragma wal_autocheckpoint = 0;")
        .unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-wal", None, "WAL cleanup");
    insert_task(&conn, "conversation-wal", "task-wal", &[]);
    conn.execute_batch("pragma wal_checkpoint(truncate);")
        .unwrap();
    conn.execute(
        "update agent_conversations
         set conversation_data =
             '{\"agent_name\":\"WAL cleanup pending\",\"run_id\":\"run-wal\"}'",
        [],
    )
    .unwrap();

    let prepared = match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("WAL source did not produce a certified snapshot"),
    };
    let snapshot_directory = prepared.snapshot_directory().to_path_buf();
    assert!(snapshot_directory.is_dir());
    assert!(fs::read_dir(&snapshot_directory).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with("-wal")
    }));

    let mut sink = CollectingSink::default();
    complete(
        scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut sink).unwrap(),
    );
    assert!(!snapshot_directory.exists());
    drop(conn);
}

#[test]
fn malformed_protobuf_is_record_local_and_valid_sibling_survives() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Warp");
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values ('conversation-1', 'task-bad', x'0A2078', '2026-07-24 12:00:01')",
        [],
    )
    .unwrap();
    insert_task(
        &conn,
        "conversation-1",
        "task-good",
        &[message(
            "message-good",
            "task-good",
            "request-good",
            1,
            &[text_arm(3, "valid sibling")],
        )],
    );
    drop(conn);

    let (authority, sink) = scan(&path);
    assert!(authority.source_complete);
    assert!(authority.has_useful_content);
    assert_eq!(authority.counters.task_rows, 2);
    assert_eq!(authority.counters.malformed_task_cells, 1);
    assert_eq!(sink.rejection_count(), 1);
    assert_eq!(sink.events().len(), 1);
    assert_eq!(sink.events()[0].body, "valid sibling");
}

#[test]
fn oversized_task_blob_is_rejected_without_hydration_between_valid_siblings() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-oversize", None, "Oversize");
    insert_task(
        &conn,
        "conversation-oversize",
        "task-a-valid",
        &[message(
            "message-before",
            "task-a-valid",
            "request-before",
            1,
            &[text_arm(2, "valid before")],
        )],
    );
    let before_rowid = conn.last_insert_rowid();
    let oversize_blob_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .unwrap()
        .saturating_add(1);
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values ('conversation-oversize', 'task-m-oversized', zeroblob(?1),
                 '2026-07-24 12:00:01')",
        [oversize_blob_bytes],
    )
    .unwrap();
    let oversize_rowid = conn.last_insert_rowid();
    insert_task(
        &conn,
        "conversation-oversize",
        "task-z-valid",
        &[message(
            "message-after",
            "task-z-valid",
            "request-after",
            2,
            &[text_arm(3, "valid after")],
        )],
    );
    let after_rowid = conn.last_insert_rowid();
    drop(conn);

    query::start_native_task_hydration_trace();
    let (authority, sink) = scan(&path);
    let hydrated_rowids = query::take_native_task_hydration_trace();

    assert_eq!(authority.counters.task_rows, 3);
    assert_eq!(authority.counters.oversized_task_rows, 1);
    assert_eq!(authority.counters.retained_events, 2);
    assert_eq!(sink.rejection_count(), 1);
    assert_eq!(
        sink.events()
            .iter()
            .map(|event| event.native_order.task_key.as_str())
            .collect::<Vec<_>>(),
        ["task-a-valid", "task-z-valid"]
    );
    let rejection = sink
        .pages
        .iter()
        .flat_map(|page| page.rejections.iter())
        .next()
        .unwrap();
    assert_eq!(
        rejection.kind,
        publication::WarpNativeRejectionKind::OversizedTask
    );
    assert_eq!(rejection.native_key, format!("rowid:{oversize_rowid}"));
    assert_eq!(hydrated_rowids, [before_rowid, after_rowid]);
    assert!(!hydrated_rowids.contains(&oversize_rowid));
}

#[test]
fn oversized_conversation_row_is_local_and_valid_siblings_survive() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-a-valid", None, "Before");
    let oversized_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .unwrap()
        .saturating_add(1);
    conn.execute(
        "insert into agent_conversations
         (conversation_id, conversation_data, last_modified_at)
         values ('conversation-m-oversized', zeroblob(?1), '2026-07-24 12:00:00')",
        [oversized_bytes],
    )
    .unwrap();
    insert_conversation(&conn, "conversation-z-valid", None, "After");
    drop(conn);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.conversation_rows, 3);
    assert_eq!(authority.counters.conversation_rows_hydrated, 2);
    assert_eq!(authority.counters.conversation_json_objects_parsed, 2);
    assert_eq!(authority.counters.sessions_retained, 2);
    assert_eq!(sink.rejection_count(), 1);
    assert_eq!(
        sink.sessions()
            .iter()
            .map(|session| session.conversation_id.as_str())
            .collect::<Vec<_>>(),
        ["conversation-a-valid", "conversation-z-valid"]
    );
    assert_safe_page_chain(&sink.pages);
}

#[test]
fn hierarchy_and_native_order_survive_late_lower_sorting_task_key() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "child", Some("parent"), "Child");
    insert_conversation(&conn, "parent", None, "Parent");
    insert_task(
        &conn,
        "child",
        "task-m",
        &[message(
            "message-m",
            "task-m",
            "request-m",
            1,
            &[text_arm(2, "middle")],
        )],
    );
    insert_task(
        &conn,
        "parent",
        "task-z",
        &[message(
            "message-z",
            "task-z",
            "request-z",
            2,
            &[text_arm(3, "last")],
        )],
    );
    drop(conn);

    let (first_authority, first) = scan(&path);
    assert_eq!(first_authority.counters.conversation_rows, 2);
    assert_eq!(first_authority.counters.conversation_rows_hydrated, 2);
    assert_eq!(first_authority.counters.conversation_json_objects_parsed, 2);
    let stable_identities = first
        .events()
        .into_iter()
        .map(|event| event.identity.clone())
        .collect::<Vec<_>>();
    let (replay_authority, replay) = scan(&path);
    assert_eq!(
        first_authority.source_integrity_digest,
        replay_authority.source_integrity_digest
    );
    assert_eq!(
        first_authority.core_generation_digest,
        replay_authority.core_generation_digest
    );
    assert_eq!(
        stable_identities,
        replay
            .events()
            .into_iter()
            .map(|event| event.identity.clone())
            .collect::<Vec<_>>()
    );

    let conn = Connection::open(&path).unwrap();
    insert_task(
        &conn,
        "parent",
        "task-a",
        &[message(
            "message-a",
            "task-a",
            "request-a",
            3,
            &[text_arm(2, "late but first")],
        )],
    );
    drop(conn);
    let (authority, second) = scan(&path);

    assert_eq!(
        second
            .events()
            .iter()
            .map(|event| event.native_order.task_key.as_str())
            .collect::<Vec<_>>(),
        vec!["task-a", "task-m", "task-z"]
    );
    for identity in stable_identities {
        assert!(second
            .events()
            .iter()
            .any(|event| event.identity == identity));
    }
    let child = second
        .sessions()
        .into_iter()
        .find(|session| session.conversation_id == "child")
        .unwrap();
    assert_eq!(child.parent_conversation_id.as_deref(), Some("parent"));
    assert_eq!(child.root_conversation_id, "parent");
    assert!(child.parent_present);
    assert_eq!(authority.counters.hierarchy_edges, 1);

    let middle_before = second
        .events()
        .into_iter()
        .find(|event| event.native_order.task_key == "task-m")
        .cloned()
        .unwrap();
    let conn = Connection::open(&path).unwrap();
    let rewritten_task = task(
        "task-m",
        &[message(
            "message-m",
            "task-m",
            "request-m",
            1,
            &[text_arm(2, "rewritten middle")],
        )],
    );
    conn.execute(
        "update agent_tasks set task = ?1 where task_id = 'task-m'",
        [rewritten_task],
    )
    .unwrap();
    drop(conn);
    let (rewritten_authority, rewritten) = scan(&path);
    let middle_after = rewritten
        .events()
        .into_iter()
        .find(|event| event.native_order.task_key == "task-m")
        .unwrap();
    assert_eq!(middle_before.identity, middle_after.identity);
    assert_ne!(middle_before.content_hash, middle_after.content_hash);
    assert_ne!(
        authority.core_generation_digest,
        rewritten_authority.core_generation_digest
    );
}

#[test]
fn cyclic_hierarchy_fails_before_emitting_pages() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "cycle-a", Some("cycle-b"), "A");
    insert_conversation(&conn, "cycle-b", Some("cycle-a"), "B");
    drop(conn);

    let mut sink = CollectingSink::default();
    let error = scan_warp_nativepath(&path, &mut sink).unwrap_err();
    assert!(error.to_string().contains("hierarchy contains a cycle"));
    assert!(sink.pages.is_empty());
}

#[test]
fn duplicate_identity_including_elided_output_rejects_whole_task_before_message_pages() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-duplicate", None, "Duplicate");
    insert_task(
        &conn,
        "conversation-duplicate",
        "task-duplicate",
        &[
            message(
                "same-message-id",
                "task-duplicate",
                "request-1",
                1,
                &[tool_result_arm("call-elided", b"successful output")],
            ),
            message(
                "same-message-id",
                "task-duplicate",
                "request-2",
                2,
                &[text_arm(3, "second")],
            ),
        ],
    );
    drop(conn);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.duplicate_message_identity_tasks, 1);
    assert_eq!(authority.counters.native_result_records, 1);
    assert!(sink.events().is_empty());
    assert!(sink.outputs().is_empty());
    assert_eq!(sink.rejection_count(), 1);
    let rejection = sink.rejections()[0];
    assert_eq!(
        rejection.kind,
        publication::WarpNativeRejectionKind::DuplicateMessageIdentity
    );
    assert_eq!(rejection.native_key, "task-duplicate");
    assert!(rejection.native_key.len() <= 512);
    assert!(rejection.reason.len() <= 1_024);
    assert!(sink.pages.iter().all(|page| {
        page.next_safe_frontier.next_message_ordinal == 0
            && page.expected_frontier.next_message_ordinal == 0
    }));
    let final_frontier = &sink.pages.last().unwrap().next_safe_frontier;
    assert_eq!(final_frontier.completed_task_rows, 1);
    assert_eq!(final_frontier.next_message_ordinal, 0);
    assert_safe_page_chain(&sink.pages);
}

#[test]
fn outputs_are_excluded_before_body_hash_and_preview_construction() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-output", None, "Output");
    let output = "CTX-NATIVEPATH-OUTPUT-MUST-NOT-SURVIVE"
        .repeat(900)
        .into_bytes();
    let output_messages = |result_body: &[u8]| {
        vec![
            message(
                "message-user",
                "task-output",
                "request-user",
                1,
                &[text_arm(2, "retained user")],
            ),
            message(
                "message-call",
                "task-output",
                "request-call",
                2,
                &[tool_call_arm(2)],
            ),
            message(
                "message-result",
                "task-output",
                "request-result",
                3,
                &[tool_result_arm("request-call", result_body)],
            ),
            message(
                "message-assistant",
                "task-output",
                "request-assistant",
                4,
                &[text_arm(3, "retained assistant")],
            ),
        ]
    };
    insert_task(
        &conn,
        "conversation-output",
        "task-output",
        &output_messages(&output),
    );
    drop(conn);

    let (authority, sink) = scan(&path);
    let counters = authority.counters;
    assert_eq!(counters.native_result_records, 1);
    assert_eq!(
        counters.native_result_body_bytes_observed,
        output.len() as u64
    );
    assert_eq!(counters.native_results_success, 1);
    assert_eq!(counters.tool_calls_retained, 1);
    assert_eq!(counters.retained_events, 3);
    assert_eq!(counters.retained_content_hashes, 3);
    assert_eq!(counters.retained_previews, 3);
    assert_eq!(counters.result_body_bytes_decoded, 0);
    assert_eq!(counters.result_body_strings_allocated, 0);
    assert_eq!(counters.result_events_created, 0);
    assert_eq!(counters.result_hashes_created, 0);
    assert_eq!(counters.result_previews_created, 0);
    assert_eq!(counters.result_file_touches_created, 0);
    assert_eq!(counters.result_fts_documents_created, 0);
    assert_eq!(counters.result_handoffs_created, 0);
    assert_eq!(counters.generic_envelope_rows, 0);
    assert_eq!(counters.durable_transaction_rotations, 0);
    let retained = sink
        .events()
        .iter()
        .map(|event| format!("{}\n{}", event.body, event.preview))
        .collect::<String>();
    assert!(!retained.contains("CTX-NATIVEPATH-OUTPUT-MUST-NOT-SURVIVE"));
    assert!(sink
        .events()
        .iter()
        .all(|event| event.event_type != ctx_history_core::EventType::ToolOutput));

    let first_hashes = sink
        .events()
        .iter()
        .map(|event| event.content_hash.clone())
        .collect::<Vec<_>>();
    let replacement_output = vec![0xff; output.len()];
    let replacement_task = task("task-output", &output_messages(&replacement_output));
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update agent_tasks set task = ?1 where task_id = 'task-output'",
        [replacement_task],
    )
    .unwrap();
    drop(conn);
    let (replacement_authority, replacement_sink) = scan(&path);
    assert_ne!(
        authority.source_integrity_digest,
        replacement_authority.source_integrity_digest
    );
    assert_eq!(
        authority.core_generation_digest,
        replacement_authority.core_generation_digest
    );
    assert_eq!(
        first_hashes,
        replacement_sink
            .events()
            .iter()
            .map(|event| event.content_hash.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn core_and_pro_match_output_outcome_oracle_with_one_classification_per_result() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-output-routing", None, "Output Routing");
    let unknown_payload = field(1, b"opaque future payload");
    let failure_payload = field(2, &field(1, b"failure body"));
    insert_task(
        &conn,
        "conversation-output-routing",
        "task-output-routing",
        &[
            message(
                "message-success",
                "task-output-routing",
                "request-success",
                1,
                &[tool_result_arm("call-success", b"success body")],
            ),
            message(
                "message-unknown",
                "task-output-routing",
                "request-unknown",
                2,
                &[tool_result_variant_arm(
                    "call-unknown",
                    99,
                    &unknown_payload,
                )],
            ),
            message(
                "message-failure",
                "task-output-routing",
                "request-failure",
                3,
                &[tool_result_variant_arm(
                    "call-failure",
                    39,
                    &failure_payload,
                )],
            ),
        ],
    );
    drop(conn);

    let (core_authority, core) = scan_profile(&path, WarpNativeProfile::CoreOnly);
    let (pro_authority, pro) = scan_profile(&path, WarpNativeProfile::CoreAndPro);

    assert_eq!(
        core_authority.core_generation_digest,
        pro_authority.core_generation_digest
    );
    assert_eq!(core.sessions(), pro.sessions());
    assert_eq!(core.events(), pro.events());
    assert_eq!(core.rejections(), pro.rejections());
    assert_core_pages_identical(&core.pages, &pro.pages);
    assert_eq!(
        core.pages
            .iter()
            .map(|page| page.identity)
            .collect::<Vec<_>>(),
        pro.pages
            .iter()
            .map(|page| page.identity)
            .collect::<Vec<_>>()
    );
    assert_eq!(core_authority.counters.native_result_records, 3);
    assert_eq!(pro_authority.counters.native_result_records, 3);
    assert_eq!(core_authority.counters.native_results_success, 1);
    assert_eq!(core_authority.counters.native_results_unknown, 1);
    assert_eq!(core_authority.counters.native_results_failure, 1);
    assert_eq!(core.events().len(), 1);
    let failure = core.events()[0];
    assert_eq!(failure.event_type, ctx_history_core::EventType::ToolOutput);
    assert_eq!(failure.result_outcome, Some(crate::OutputOutcome::Failure));
    assert_eq!(failure.call_id.as_deref(), Some("call-failure"));
    assert_eq!(failure.body, "tool result: run_agents");
    assert!(!failure.body.contains("failure body"));
    assert!(core.outputs().is_empty());
    assert!(core.output_rejections().is_empty());

    let outputs = pro.outputs();
    assert_eq!(outputs.len(), 3);
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.content.as_slice())
            .collect::<Vec<_>>(),
        [
            b"success body".as_slice(),
            b"".as_slice(),
            b"failure body".as_slice()
        ]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.outcome.outcome)
            .collect::<Vec<_>>(),
        [
            crate::OutputOutcome::Success,
            crate::OutputOutcome::Unknown,
            crate::OutputOutcome::Failure
        ]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.call_id.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("call-success"),
            Some("call-unknown"),
            Some("call-failure")
        ]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.coordinate.source_record_subrecord_index)
            .collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
    assert_eq!(pro_authority.counters.result_handoffs_created, 3);
    assert_eq!(pro_authority.counters.result_body_strings_allocated, 0);
    assert!(pro.output_rejections().is_empty());
    assert_safe_page_chain(&core.pages);
    assert_safe_page_chain(&pro.pages);
}

#[test]
fn core_and_pro_pages_fan_out_successful_output_without_changing_core() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-fanout", None, "Fanout");
    insert_task(
        &conn,
        "conversation-fanout",
        "task-fanout",
        &[
            message(
                "message-user",
                "task-fanout",
                "request-user",
                1,
                &[text_arm(2, "retained user")],
            ),
            message(
                "message-call",
                "task-fanout",
                "request-call",
                2,
                &[tool_call_arm(2)],
            ),
            message(
                "message-output",
                "task-fanout",
                "request-output",
                3,
                &[tool_result_last_wins_arm(
                    "request-call",
                    b"STALE-WARP-OUTPUT",
                    b"CTX-WARP-TRANSIENT-SUCCESS",
                )],
            ),
            message(
                "message-assistant",
                "task-fanout",
                "request-assistant",
                4,
                &[text_arm(3, "retained assistant")],
            ),
        ],
    );
    drop(conn);

    let (core_authority, core) = scan_profile(&path, WarpNativeProfile::CoreOnly);
    let (pro_authority, pro) = scan_profile(&path, WarpNativeProfile::CoreAndPro);

    assert_eq!(
        core_authority.core_generation_digest,
        pro_authority.core_generation_digest
    );
    assert_eq!(
        core.events()
            .iter()
            .map(|event| (&event.identity, &event.content_hash, &event.body))
            .collect::<Vec<_>>(),
        pro.events()
            .iter()
            .map(|event| (&event.identity, &event.content_hash, &event.body))
            .collect::<Vec<_>>()
    );
    assert_core_pages_identical(&core.pages, &pro.pages);
    assert!(core.outputs().is_empty());
    assert_eq!(core_authority.counters.result_body_bytes_decoded, 0);
    assert_eq!(core_authority.counters.result_body_strings_allocated, 0);
    assert_eq!(core_authority.counters.result_handoffs_created, 0);

    let outputs = pro.outputs();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, b"CTX-WARP-TRANSIENT-SUCCESS");
    assert_ne!(outputs[0].content, b"STALE-WARP-OUTPUT");
    assert_eq!(outputs[0].call_id.as_deref(), Some("request-call"));
    assert_eq!(outputs[0].outcome.outcome, crate::OutputOutcome::Success);
    assert_eq!(
        outputs[0].associations.direct_session_id,
        "conversation-fanout"
    );
    assert_eq!(
        outputs[0].associations.root_session_id,
        "conversation-fanout"
    );
    assert_eq!(
        pro_authority.counters.result_body_bytes_decoded,
        b"CTX-WARP-TRANSIENT-SUCCESS".len() as u64
    );
    assert_eq!(pro_authority.counters.result_body_strings_allocated, 0);
    assert_eq!(pro_authority.counters.result_handoffs_created, 1);
    assert!(pro.events().iter().all(|event| {
        !event.body.contains("CTX-WARP-TRANSIENT-SUCCESS")
            && !event.preview.contains("CTX-WARP-TRANSIENT-SUCCESS")
    }));

    assert_safe_page_chain(&pro.pages);
    let (_, replay) = scan_profile(&path, WarpNativeProfile::CoreAndPro);
    assert_eq!(
        pro.pages
            .iter()
            .map(|page| (
                page.identity,
                page.expected_frontier.clone(),
                page.next_safe_frontier.clone(),
            ))
            .collect::<Vec<_>>(),
        replay
            .pages
            .iter()
            .map(|page| (
                page.identity,
                page.expected_frontier.clone(),
                page.next_safe_frontier.clone(),
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn pro_observation_bytes_never_change_core_pages_and_use_independent_receipts() {
    const OUTPUTS: usize = 32;
    const OUTPUT_BYTES: usize = 256 * 1024;

    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(
        &conn,
        "conversation-independent-pro-pages",
        None,
        "Independent Pro Pages",
    );
    let output = vec![b'x'; OUTPUT_BYTES];
    let mut messages = (0..OUTPUTS)
        .map(|index| {
            message(
                &format!("message-output-{index:03}"),
                "task-independent-pro-pages",
                &format!("request-output-{index:03}"),
                index as u64,
                &[tool_result_arm(&format!("call-output-{index:03}"), &output)],
            )
        })
        .collect::<Vec<_>>();
    messages.push(message(
        "message-after-outputs",
        "task-independent-pro-pages",
        "request-after-outputs",
        OUTPUTS as u64,
        &[text_arm(3, "retained after independently paged outputs")],
    ));
    insert_task(
        &conn,
        "conversation-independent-pro-pages",
        "task-independent-pro-pages",
        &messages,
    );
    drop(conn);

    let (core_authority, core) = scan_profile(&path, WarpNativeProfile::CoreOnly);
    let (pro_authority, pro) = scan_profile(&path, WarpNativeProfile::CoreAndPro);

    assert_core_pages_identical(&core.pages, &pro.pages);
    assert_eq!(core.pages.len(), 1);
    assert_eq!(core_authority.pages_emitted, 1);
    assert_eq!(core_authority.pro_output_pages_emitted, 0);
    assert_eq!(pro_authority.pages_emitted, 1);
    assert!(pro_authority.pro_output_pages_emitted >= 2);
    assert_eq!(
        pro_authority.pro_output_pages_emitted as usize,
        pro.pro_pages.len()
    );
    assert_eq!(pro.outputs().len(), OUTPUTS);
    assert_eq!(
        pro.outputs()
            .iter()
            .map(|observation| observation.content.len())
            .sum::<usize>(),
        OUTPUTS * OUTPUT_BYTES
    );
    assert!(pro.output_rejections().is_empty());
    assert_eq!(pro.pro_receipts.len(), pro.pro_pages.len());
    for (page, receipt) in pro.pro_pages.iter().zip(&pro.pro_receipts) {
        assert_eq!(&page.receipt(), receipt);
    }
    assert_safe_page_chain(&core.pages);
    assert_safe_page_chain(&pro.pages);
    assert_pro_page_chain(&pro.pro_pages, &pro.pages);
}

#[test]
fn pro_output_decode_and_malformed_failures_do_not_change_core_rows_or_digest() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-output-errors", None, "Output Errors");

    let malformed_result_envelope = vec![0x12, 0x05, b'x'];
    let malformed_nested_body = vec![0x0a, 0x05, b'x'];
    let mut malformed_nested_result = field(1, b"call-malformed-body");
    malformed_nested_result.extend(field(39, &field(2, &malformed_nested_body)));
    let mut malformed_output_metadata = field(1, b"message-malformed-metadata");
    malformed_output_metadata.extend(tool_result_arm(
        "call-malformed-metadata",
        b"otherwise valid output",
    ));
    malformed_output_metadata.extend(field(11, b"task-output-errors"));
    malformed_output_metadata.extend(field(13, &[0xff]));
    malformed_output_metadata.extend(field(14, &timestamp(4)));
    insert_task(
        &conn,
        "conversation-output-errors",
        "task-output-errors",
        &[
            message(
                "message-decode-error",
                "task-output-errors",
                "request-decode-error",
                1,
                &[field(5, &malformed_result_envelope)],
            ),
            message(
                "message-malformed-body",
                "task-output-errors",
                "request-malformed-body",
                2,
                &[field(5, &malformed_nested_result)],
            ),
            message(
                "message-invalid-utf8",
                "task-output-errors",
                "request-invalid-utf8",
                3,
                &[tool_result_arm("call-invalid-utf8", &[0xff])],
            ),
            malformed_output_metadata,
            message(
                "message-after-errors",
                "task-output-errors",
                "request-after-errors",
                5,
                &[text_arm(3, "retained after output-local failures")],
            ),
        ],
    );
    drop(conn);

    let (core_authority, core) = scan_profile(&path, WarpNativeProfile::CoreOnly);
    let (pro_authority, pro) = scan_profile(&path, WarpNativeProfile::CoreAndPro);

    assert_eq!(
        core_authority.core_generation_digest,
        pro_authority.core_generation_digest
    );
    assert_eq!(core.sessions(), pro.sessions());
    assert_eq!(core.events(), pro.events());
    assert_eq!(core.rejections(), pro.rejections());
    assert_core_pages_identical(&core.pages, &pro.pages);
    assert_eq!(core.events().len(), 2);
    assert_eq!(core.events()[0].body, "tool result: run_agents");
    assert_eq!(
        core.events()[0].result_outcome,
        Some(crate::OutputOutcome::Failure)
    );
    assert_eq!(
        core.events()[0].call_id.as_deref(),
        Some("call-malformed-body")
    );
    assert_eq!(
        core.events()[1].body,
        "retained after output-local failures"
    );
    assert!(core.output_rejections().is_empty());
    assert!(core.outputs().is_empty());
    assert_eq!(core_authority.counters.native_result_records, 4);
    assert_eq!(pro_authority.counters.native_result_records, 4);
    assert_eq!(core_authority.counters.malformed_output_records, 1);
    assert_eq!(pro_authority.counters.malformed_output_records, 4);
    assert!(pro.outputs().is_empty());
    let rejections = pro.output_rejections();
    assert_eq!(rejections.len(), 4);
    assert!(rejections.iter().all(|rejection| {
        rejection.kind == publication::WarpNativeOutputRejectionKind::Malformed
            && rejection.native_key.len() <= 512
            && rejection.reason.len() <= 1_024
    }));
    assert_eq!(core.rejection_count(), 0);
    assert_eq!(pro.rejection_count(), 0);
    assert_safe_page_chain(&core.pages);
    assert_safe_page_chain(&pro.pages);
}

#[test]
fn task_is_hydrated_once_while_complete_messages_advance_bounded_safe_pages() {
    const MESSAGES: usize = 130;

    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-pages", None, "Pages");
    let messages = (0..MESSAGES)
        .map(|index| {
            message(
                &format!("message-{index:03}"),
                "task-pages",
                &format!("request-{index:03}"),
                index as u64,
                &[text_arm(
                    if index % 2 == 0 { 2 } else { 3 },
                    &format!("message body {index:03}"),
                )],
            )
        })
        .collect::<Vec<_>>();
    insert_task(&conn, "conversation-pages", "task-pages", &messages);
    let task_rowid = conn.last_insert_rowid();
    drop(conn);

    query::start_native_task_hydration_trace();
    let (authority, sink) = scan(&path);
    let hydrated = query::take_native_task_hydration_trace();

    assert_eq!(hydrated, [task_rowid]);
    assert_eq!(authority.counters.retained_events, MESSAGES as u64);
    assert!(sink.pages.len() >= 3);
    assert!(sink.pages.iter().any(|page| {
        page.next_safe_frontier.last_task_rowid == Some(task_rowid)
            && page.next_safe_frontier.next_message_ordinal > 0
    }));
    assert_safe_page_chain(&sink.pages);
    let last = sink.pages.last().unwrap();
    assert_eq!(last.next_safe_frontier.completed_task_rows, 1);
    assert_eq!(last.next_safe_frontier.last_task_rowid, Some(task_rowid));
    assert_eq!(last.next_safe_frontier.next_message_ordinal, 0);
}

#[test]
fn persisted_terminal_forgery_replays_exact_suffix_and_retries_failed_page_idempotently() {
    const TASKS: usize = 5;
    const MESSAGES_PER_TASK: usize = 70;

    let directory = tempdir().unwrap();
    let path = directory.path().join("restart.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let mut task_rowids = Vec::new();
    for task_index in 0..TASKS {
        let conversation_id = format!("conversation-restart-{task_index:02}");
        insert_conversation(&conn, &conversation_id, None, "Restart");
        let task_id = format!("task-restart-{task_index:02}");
        let messages = (0..MESSAGES_PER_TASK)
            .map(|message_index| {
                let sequence = task_index * MESSAGES_PER_TASK + message_index;
                message(
                    &format!("message-{sequence:03}"),
                    &task_id,
                    &format!("request-{sequence:03}"),
                    sequence as u64,
                    &[text_arm(2, &format!("restart body {sequence:03}"))],
                )
            })
            .collect::<Vec<_>>();
        insert_task(&conn, &conversation_id, &task_id, &messages);
        task_rowids.push(conn.last_insert_rowid());
    }
    drop(conn);

    let (full_authority, full_sink) = scan(&path);
    assert!(full_sink.pages.len() > 3);

    let prepared = match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("fresh source did not produce a certified snapshot"),
    };
    let preparation_inputs = prepared.inputs.clone();
    let mut crashing = CrashSink::new(3);
    let error =
        scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut crashing)
            .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected Warp page commit crash"));
    assert_eq!(crashing.committed.len(), 2);
    let attempted = crashing.attempted.clone().unwrap();
    let committed_frontier = crashing
        .committed
        .last()
        .unwrap()
        .next_safe_frontier
        .clone();
    assert_eq!(attempted.expected_frontier, committed_frontier);
    assert!(committed_frontier.next_message_ordinal > 0);
    assert_eq!(committed_frontier.last_task_rowid, Some(task_rowids[1]));
    let partial_state = preparation_inputs
        .persisted_state_at(committed_frontier.clone())
        .unwrap();
    assert!(!partial_state.checkpoint_is_terminal());
    assert_eq!(partial_state.checkpoint_frontier(), &committed_frontier);
    let mut forged_wire = serde_json::to_value(&partial_state).unwrap();
    assert_eq!(forged_wire["checkpoint"]["terminal"], false);
    forged_wire["checkpoint"]["terminal"] = serde_json::Value::Bool(true);
    let persisted_partial: lifecycle::WarpNativePersistedState =
        serde_json::from_value(forged_wire).unwrap();
    assert!(!persisted_partial.checkpoint_is_terminal());
    assert_eq!(persisted_partial.checkpoint_frontier(), &committed_frontier);

    let resumed = match prepare_warp_nativepath_lifecycle(&path, &[persisted_partial]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            panic!("forged persisted terminal authority produced an exact no-op")
        }
        _ => panic!("exact partial snapshot did not prepare for resume"),
    };
    assert_eq!(
        resumed.inputs.action,
        lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
    );
    query::start_native_task_hydration_trace();
    let mut suffix_sink = CollectingSink::default();
    let resumed_authority = complete(
        scan_prepared_warp_nativepath(*resumed, WarpNativeProfile::CoreOnly, &mut suffix_sink)
            .unwrap(),
    );
    let hydrated = query::take_native_task_hydration_trace();

    assert_eq!(hydrated, task_rowids[1..].to_vec());
    assert_eq!(
        suffix_sink.pages.len(),
        full_sink.pages.len() - crashing.committed.len()
    );
    assert_eq!(
        suffix_sink
            .pages
            .iter()
            .map(|page| page.identity)
            .collect::<Vec<_>>(),
        full_sink
            .pages
            .iter()
            .skip(crashing.committed.len())
            .map(|page| page.identity)
            .collect::<Vec<_>>()
    );
    let committed_events = crashing
        .committed
        .iter()
        .map(|page| page.events.len())
        .sum::<usize>();
    assert_eq!(
        suffix_sink.events().len(),
        full_sink.events().len() - committed_events
    );
    assert_eq!(
        suffix_sink.pages.first().unwrap().identity,
        attempted.identity
    );
    assert_eq!(
        suffix_sink.pages.first().unwrap().next_safe_frontier,
        attempted.next_frontier
    );
    assert_eq!(
        resumed_authority.counters.retained_events,
        suffix_sink.events().len() as u64
    );
    assert_eq!(resumed_authority.counters.task_rows, (TASKS - 1) as u64);
    assert!(resumed_authority.counters.task_rows < full_authority.counters.task_rows);
    assert_eq!(
        resumed_authority.counters.conversation_json_objects_parsed,
        (TASKS - 1) as u64
    );
    assert!(
        resumed_authority.counters.conversation_json_objects_parsed
            < full_authority.counters.conversation_json_objects_parsed
    );
    assert_eq!(
        resumed_authority.source_integrity_digest,
        full_authority.source_integrity_digest
    );
    assert_eq!(
        resumed_authority.core_generation_digest,
        full_authority.core_generation_digest
    );
    assert!(resumed_authority.persisted_state.checkpoint_is_terminal());

    assert!(matches!(
        prepare_warp_nativepath_lifecycle(
            &path,
            std::slice::from_ref(resumed_authority.persisted_state.as_ref())
        ),
        WarpNativePreparationOutcome::ExactNoOp { .. }
    ));

    let encoded_terminal = serde_json::to_vec(&resumed_authority.persisted_state).unwrap();
    let persisted_terminal: lifecycle::WarpNativePersistedState =
        serde_json::from_slice(&encoded_terminal).unwrap();
    assert!(!persisted_terminal.checkpoint_is_terminal());
    let restarted = match prepare_warp_nativepath_lifecycle(&path, &[persisted_terminal]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            panic!("persisted terminal observation crossed the runtime authority boundary")
        }
        _ => panic!("legitimate terminal restart did not prepare for EOF recertification"),
    };
    assert_eq!(
        restarted.inputs.action,
        lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
    );
    query::start_native_task_hydration_trace();
    let mut restart_sink = CollectingSink::default();
    let restarted_authority = complete(
        scan_prepared_warp_nativepath(*restarted, WarpNativeProfile::CoreOnly, &mut restart_sink)
            .unwrap(),
    );
    assert!(query::take_native_task_hydration_trace().is_empty());
    assert!(restart_sink.pages.is_empty());
    assert!(restarted_authority.persisted_state.checkpoint_is_terminal());
    assert_eq!(
        restarted_authority.source_integrity_digest,
        resumed_authority.source_integrity_digest
    );
    assert_eq!(
        restarted_authority.core_generation_digest,
        resumed_authority.core_generation_digest
    );
}

#[test]
fn exact_snapshot_conversation_restart_seeks_after_the_committed_rowid() {
    const CONVERSATIONS: usize = 130;

    let directory = tempdir().unwrap();
    let path = directory.path().join("conversation-restart.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    for index in 0..CONVERSATIONS {
        insert_conversation(
            &conn,
            &format!("conversation-restart-{index:03}"),
            None,
            "Conversation restart",
        );
    }
    drop(conn);

    let (full_authority, full_sink) = scan(&path);
    assert_eq!(full_sink.pages.len(), 3);

    let prepared = match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("fresh conversation source did not produce a certified snapshot"),
    };
    let preparation_inputs = prepared.inputs.clone();
    let mut crashing = CrashSink::new(2);
    scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut crashing)
        .unwrap_err();
    assert_eq!(crashing.committed.len(), 1);
    let attempted = crashing.attempted.clone().unwrap();
    let committed_frontier = crashing.committed[0].next_safe_frontier.clone();
    assert_eq!(committed_frontier.completed_conversation_rows, 64);
    assert!(committed_frontier.last_conversation_rowid.is_some());
    assert_eq!(attempted.expected_frontier, committed_frontier);
    let partial_state = preparation_inputs
        .persisted_state_at(committed_frontier)
        .unwrap();
    assert!(!partial_state.checkpoint_is_terminal());

    let resumed = match prepare_warp_nativepath_lifecycle(&path, &[partial_state]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("exact partial conversation snapshot did not resume"),
    };
    let mut suffix_sink = CollectingSink::default();
    let resumed_authority = complete(
        scan_prepared_warp_nativepath(*resumed, WarpNativeProfile::CoreOnly, &mut suffix_sink)
            .unwrap(),
    );

    assert_eq!(suffix_sink.pages.len(), 2);
    assert_eq!(
        suffix_sink.pages[0].expected_frontier,
        attempted.expected_frontier
    );
    assert_eq!(
        suffix_sink.pages[0].next_safe_frontier,
        attempted.next_frontier
    );
    assert_eq!(suffix_sink.pages[0].identity, attempted.identity);
    assert_eq!(
        resumed_authority.counters.conversation_rows,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.counters.conversation_rows_hydrated,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.counters.conversation_json_objects_parsed,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.counters.sessions_retained,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.source_integrity_digest,
        full_authority.source_integrity_digest
    );
    assert_eq!(
        resumed_authority.core_generation_digest,
        full_authority.core_generation_digest
    );
}

#[test]
fn oversized_successful_output_is_local_and_later_output_survives() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-output-bound", None, "Output Bound");
    let oversized = vec![b'x'; publication::WARP_NATIVE_PRO_OUTPUT_MAX_BODY_BYTES + 1];
    insert_task(
        &conn,
        "conversation-output-bound",
        "task-output-bound",
        &[
            message(
                "message-too-large",
                "task-output-bound",
                "request-too-large",
                1,
                &[tool_result_arm("call-too-large", &oversized)],
            ),
            message(
                "message-small",
                "task-output-bound",
                "request-small",
                2,
                &[tool_result_arm("call-small", b"small successful output")],
            ),
            message(
                "message-after",
                "task-output-bound",
                "request-after",
                3,
                &[text_arm(3, "valid retained sibling after outputs")],
            ),
        ],
    );
    drop(conn);

    let (core_authority, core) = scan_profile(&path, WarpNativeProfile::CoreOnly);
    let (pro_authority, pro) = scan_profile(&path, WarpNativeProfile::CoreAndPro);

    assert_eq!(
        core_authority.core_generation_digest,
        pro_authority.core_generation_digest
    );
    assert_eq!(core.sessions(), pro.sessions());
    assert_eq!(core.events(), pro.events());
    assert_eq!(core.rejections(), pro.rejections());
    assert_core_pages_identical(&core.pages, &pro.pages);
    assert_eq!(core_authority.counters.oversized_output_records, 0);
    assert_eq!(pro_authority.counters.oversized_output_records, 1);
    assert_eq!(pro.outputs().len(), 1);
    assert_eq!(pro.outputs()[0].content, b"small successful output");
    assert_eq!(pro_authority.counters.result_body_strings_allocated, 0);
    assert_eq!(
        pro_authority.counters.result_body_bytes_decoded,
        b"small successful output".len() as u64
    );
    assert_eq!(core.rejection_count(), 0);
    assert_eq!(pro.rejection_count(), 0);
    assert!(core.output_rejections().is_empty());
    assert_eq!(pro.events().len(), 1);
    assert_eq!(pro.events()[0].body, "valid retained sibling after outputs");
    let output_rejections = pro.output_rejections();
    assert_eq!(output_rejections.len(), 1);
    let rejection = output_rejections[0];
    assert_eq!(
        rejection.kind,
        publication::WarpNativeOutputRejectionKind::Oversized
    );
    assert!(rejection.reason.contains(&oversized.len().to_string()));
    assert!(rejection.native_key.len() <= 512);
    assert!(rejection.reason.len() <= 1_024);
    assert!(oversized.len() > 8 * 1024 * 1024);
    assert!(oversized.len() < 16 * 1024 * 1024);
    assert_safe_page_chain(&core.pages);
    assert_safe_page_chain(&pro.pages);
}

#[test]
fn normalized_message_between_eight_and_sixteen_mib_is_locally_rejected() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(
        &conn,
        "conversation-normalized-bound",
        None,
        "Normalized Bound",
    );
    let oversized_message_id = "m".repeat(WARP_NATIVE_PAGE_MAX_BYTES + 1);
    assert!(oversized_message_id.len() > 8 * 1024 * 1024);
    assert!(oversized_message_id.len() < 16 * 1024 * 1024);
    insert_task(
        &conn,
        "conversation-normalized-bound",
        "task-normalized-bound",
        &[
            message(
                &oversized_message_id,
                "task-normalized-bound",
                "request-too-large",
                1,
                &[text_arm(2, "unit must be rejected")],
            ),
            message(
                "message-valid-after",
                "task-normalized-bound",
                "request-valid-after",
                2,
                &[text_arm(3, "valid after oversized normalized message")],
            ),
        ],
    );
    drop(conn);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.oversized_normalized_units, 1);
    assert_eq!(sink.events().len(), 1);
    assert_eq!(
        sink.events()[0].body,
        "valid after oversized normalized message"
    );
    assert_eq!(sink.rejection_count(), 1);
    let rejection = sink.rejections()[0];
    assert_eq!(
        rejection.kind,
        publication::WarpNativeRejectionKind::OversizedNormalizedUnit
    );
    assert_eq!(rejection.native_key, "task-normalized-bound:message:0");
    assert!(rejection.native_key.len() <= 512);
    assert!(rejection.reason.len() <= 1_024);
    assert_eq!(
        sink.pages
            .last()
            .unwrap()
            .next_safe_frontier
            .completed_task_rows,
        1
    );
    assert_safe_page_chain(&sink.pages);
}

fn assert_safe_page_chain(pages: &[WarpNativePage]) {
    assert!(!pages.is_empty());
    let initial = &pages.first().unwrap().expected_frontier;
    assert_eq!(initial.phase, publication::WarpNativeFrontierPhase::Start);
    assert_eq!(initial.completed_conversation_rows, 0);
    assert_eq!(initial.completed_hierarchy_edges, 0);
    assert_eq!(initial.completed_task_rows, 0);
    assert_eq!(initial.last_task_rowid, None);
    assert_eq!(initial.next_message_ordinal, 0);
    assert_ne!(initial.source_digest, [0; 32]);
    assert_ne!(initial.core_digest, [0; 32]);
    for pair in pages.windows(2) {
        assert_eq!(pair[0].next_safe_frontier, pair[1].expected_frontier);
    }
    assert!(pages.iter().all(|page| {
        page.logical_unit_count() <= WARP_NATIVE_PAGE_MAX_ROWS
            && page.estimated_bytes <= WARP_NATIVE_PAGE_MAX_BYTES
    }));
}

fn assert_core_pages_identical(core_only: &[WarpNativePage], core_and_pro: &[WarpNativePage]) {
    let coordinates = |pages: &[WarpNativePage]| {
        pages
            .iter()
            .map(|page| {
                (
                    page.identity,
                    page.expected_frontier.clone(),
                    page.next_safe_frontier.clone(),
                    page.logical_units,
                    page.estimated_bytes,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(coordinates(core_only), coordinates(core_and_pro));
}

fn assert_pro_page_chain(pro_pages: &[WarpNativeProOutputPage], core_pages: &[WarpNativePage]) {
    assert!(!pro_pages.is_empty());
    assert_eq!(
        pro_pages.first().unwrap().expected_frontier,
        core_pages.first().unwrap().expected_frontier
    );
    for pair in pro_pages.windows(2) {
        assert_eq!(pair[0].next_safe_frontier, pair[1].expected_frontier);
    }
    assert_eq!(
        pro_pages.last().unwrap().next_safe_frontier,
        core_pages.last().unwrap().next_safe_frontier
    );
    assert!(pro_pages.iter().all(|page| {
        page.logical_unit_count() <= WARP_NATIVE_PAGE_MAX_ROWS
            && page.estimated_bytes <= WARP_NATIVE_PAGE_MAX_BYTES
            && page.row_count() <= page.logical_unit_count()
    }));
}

#[test]
fn protobuf_oneofs_are_last_wins_at_message_and_result_boundaries() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-oneof", None, "Oneof");

    let unknown_last = message(
        "message-unknown",
        "task-oneof",
        "request-unknown",
        1,
        &[
            text_arm(3, "must be suppressed"),
            field(17, &field(1, b"future arm")),
        ],
    );
    let result_then_assistant = message(
        "message-retained",
        "task-oneof",
        "request-retained",
        2,
        &[
            tool_result_arm("request-retained", b"stale output"),
            text_arm(3, "last assistant wins"),
        ],
    );
    let assistant_then_result = message(
        "message-output",
        "task-oneof",
        "request-output",
        3,
        &[
            text_arm(3, "stale assistant"),
            tool_result_arm("request-output", b"selected output"),
        ],
    );
    let mut run_shell = field(5, &field(1, b"stale success output"));
    run_shell.extend(field(6, &[]));
    let mut failure_result = field(1, b"request-failure");
    failure_result.extend(field(2, &run_shell));
    let nested_failure = message(
        "message-failure",
        "task-oneof",
        "request-failure",
        4,
        &[field(5, &failure_result)],
    );
    insert_task(
        &conn,
        "conversation-oneof",
        "task-oneof",
        &[
            unknown_last,
            result_then_assistant,
            assistant_then_result,
            nested_failure,
        ],
    );
    drop(conn);

    let (authority, sink) = scan(&path);
    assert_eq!(sink.events().len(), 2);
    assert_eq!(sink.events()[0].body, "last assistant wins");
    assert_eq!(sink.events()[1].body, "tool result: run_shell_command");
    assert_eq!(
        sink.events()[1].result_outcome,
        Some(crate::OutputOutcome::Failure)
    );
    assert_eq!(sink.events()[1].call_id.as_deref(), Some("request-failure"));
    assert_eq!(authority.counters.unknown_oneofs, 1);
    assert_eq!(authority.counters.native_result_records, 2);
    assert_eq!(authority.counters.native_results_success, 1);
    assert_eq!(authority.counters.native_results_failure, 1);
    assert_eq!(
        authority.counters.native_result_body_bytes_observed,
        b"selected output".len() as u64
    );
}

#[test]
fn empty_source_and_pre_certification_mutation_publish_zero_pages() {
    let directory = tempdir().unwrap();
    let empty_path = directory.path().join("empty.sqlite");
    let conn = Connection::open(&empty_path).unwrap();
    create_schema(&conn);
    drop(conn);
    let (empty, sink) = scan(&empty_path);
    assert!(empty.source_complete);
    assert!(empty.zero_authoritative_rows);
    assert!(!empty.has_useful_content);
    assert!(sink.pages.is_empty());
    assert!(empty.persisted_state.checkpoint_is_terminal());
    let empty_frontier = empty.persisted_state.checkpoint_frontier();
    assert_eq!(empty_frontier.phase, WarpNativeFrontierPhase::Start);
    assert_eq!(empty_frontier.completed_conversation_rows, 0);
    assert_eq!(empty_frontier.completed_task_rows, 0);
    assert_eq!(empty_frontier.retained_events, 0);
    assert_ne!(empty_frontier.source_digest, [0; 32]);
    assert_ne!(empty_frontier.core_digest, [0; 32]);
    assert!(matches!(
        prepare_warp_nativepath_lifecycle(
            &empty_path,
            std::slice::from_ref(empty.persisted_state.as_ref())
        ),
        WarpNativePreparationOutcome::ExactNoOp { .. }
    ));
    let encoded_empty = serde_json::to_vec(&empty.persisted_state).unwrap();
    let persisted_empty: lifecycle::WarpNativePersistedState =
        serde_json::from_slice(&encoded_empty).unwrap();
    assert!(!persisted_empty.checkpoint_is_terminal());
    let restarted_empty = match prepare_warp_nativepath_lifecycle(&empty_path, &[persisted_empty]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            panic!("persisted empty EOF crossed the runtime authority boundary")
        }
        _ => panic!("persisted empty EOF did not prepare for recertification"),
    };
    assert_eq!(
        restarted_empty.inputs.action,
        lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
    );
    let mut restarted_empty_sink = CollectingSink::default();
    let recertified_empty = complete(
        scan_prepared_warp_nativepath(
            *restarted_empty,
            WarpNativeProfile::CoreOnly,
            &mut restarted_empty_sink,
        )
        .unwrap(),
    );
    assert!(restarted_empty_sink.pages.is_empty());
    assert!(recertified_empty.persisted_state.checkpoint_is_terminal());
    assert_eq!(
        recertified_empty.persisted_state.checkpoint_frontier(),
        empty_frontier
    );

    let changed_path = directory.path().join("changed.sqlite");
    let conn = Connection::open(&changed_path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-change", None, "Before");
    insert_task(
        &conn,
        "conversation-change",
        "task-change",
        &[message(
            "message-change",
            "task-change",
            "request-change",
            1,
            &[text_arm(2, "must not publish")],
        )],
    );
    drop(conn);
    let mut sink = CollectingSink::default();
    let hook_path = changed_path.clone();
    let outcome = scan_warp_nativepath_with_certification_hook(
        &changed_path,
        WarpNativeProfile::CoreOnly,
        &mut sink,
        || {
            let conn = Connection::open(&hook_path)?;
            conn.execute(
                "update agent_conversations
             set conversation_data = '{\"agent_name\":\"After\"}'
             where conversation_id = 'conversation-change'",
                [],
            )?;
            Ok(())
        },
    )
    .unwrap();
    let WarpNativeScanOutcome::Incomplete(incomplete) = outcome else {
        panic!("mutated source was incorrectly marked complete");
    };
    assert!(!incomplete.source_complete);
    assert_eq!(
        incomplete.reason,
        publication::WarpNativeIncompleteReason::SnapshotCertificationRace
    );
    assert_eq!(incomplete.pages_emitted, 0);
    assert_eq!(incomplete.pro_output_pages_emitted, 0);
    assert!(sink.pages.is_empty());
    assert!(sink.pro_pages.is_empty());
}

#[test]
fn preparation_exact_noop_and_nonterminal_resume_are_store_ready() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Baseline");
    insert_task(
        &conn,
        "conversation-1",
        "task-1",
        &[message(
            "message-1",
            "task-1",
            "request-1",
            1,
            &[text_arm(2, "baseline")],
        )],
    );
    drop(conn);

    let (baseline, _) = scan(&path);
    let partial = match prepare_warp_nativepath_lifecycle(
        &path,
        std::slice::from_ref(baseline.persisted_state.as_ref()),
    ) {
        WarpNativePreparationOutcome::ExactNoOp {
            inputs,
            persisted_state,
        } => {
            assert_eq!(
                inputs.action,
                lifecycle::WarpNativePreparationAction::ExactNoOp
            );
            assert_eq!(persisted_state, baseline.persisted_state);
            let partial = inputs
                .persisted_state_at(persisted_state.checkpoint_frontier().clone())
                .unwrap();
            assert!(!partial.checkpoint_is_terminal());
            partial
        }
        _ => panic!("terminal exact generation did not prepare as an exact no-op"),
    };

    match prepare_warp_nativepath_lifecycle(&path, std::slice::from_ref(&partial)) {
        WarpNativePreparationOutcome::Ready(prepared) => {
            assert_eq!(
                prepared.inputs.action,
                lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
            );
            assert_eq!(
                prepared.inputs.resume_frontier.as_ref(),
                Some(partial.checkpoint_frontier())
            );
        }
        _ => panic!("non-terminal exact generation did not prepare for bounded resume"),
    }
}

#[test]
fn lifecycle_certified_snapshot_remains_publishable_after_live_source_changes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Before");
    insert_task(&conn, "conversation-1", "task-1", &[]);
    drop(conn);

    let prepared = match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("fresh Warp source did not produce a frozen preparation"),
    };
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update agent_conversations
         set conversation_data = '{\"agent_name\":\"After\"}'
         where conversation_id = 'conversation-1'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut sink = CollectingSink::default();
    let authority = complete(
        scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut sink).unwrap(),
    );
    assert!(authority.source_complete);
    assert!(!sink.pages.is_empty());
    assert_eq!(sink.sessions()[0].title, "Before");
}

#[test]
fn preparation_schema_and_index_drift_are_typed_and_never_resume_stale_authority() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Schema");
    insert_task(&conn, "conversation-1", "task-1", &[]);
    drop(conn);
    let (baseline, _) = scan(&path);

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let prepared = match prepare_warp_nativepath_lifecycle(
        &path,
        std::slice::from_ref(baseline.persisted_state.as_ref()),
    ) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("compatible schema drift did not produce an authoritative snapshot"),
    };
    assert_eq!(
        prepared.inputs.action,
        lifecycle::WarpNativePreparationAction::AuthoritativeScan
    );
    assert_ne!(
        prepared.inputs.capability_digest,
        baseline.persisted_state.capability_digest
    );

    let incompatible_path = directory.path().join("incompatible.sqlite");
    let conn = Connection::open(&incompatible_path).unwrap();
    conn.execute_batch(
        "create table agent_conversations (
             conversation_id text not null,
             conversation_data text not null,
             last_modified_at text not null
         );
         create table agent_tasks (
             conversation_id text not null,
             task_id text not null,
             task blob not null,
             last_modified_at text not null
         );",
    )
    .unwrap();
    drop(conn);
    match prepare_warp_nativepath_lifecycle(&incompatible_path, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => assert_eq!(
            failure.kind,
            lifecycle::WarpNativeSourceFailureKind::SchemaIncompatible
        ),
        _ => panic!("missing Warp keyset index was not a typed schema failure"),
    }
}

#[test]
fn preparation_and_scan_failures_remain_narrowly_typed() {
    let directory = tempdir().unwrap();
    let missing = directory.path().join("missing.sqlite");
    match prepare_warp_nativepath_lifecycle(&missing, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => assert_eq!(
            failure.kind,
            lifecycle::WarpNativeSourceFailureKind::NotFound
        ),
        _ => panic!("missing Warp source was not typed"),
    }

    let corrupt = directory.path().join("corrupt.sqlite");
    fs::write(&corrupt, b"not a sqlite database").unwrap();
    match prepare_warp_nativepath_lifecycle(&corrupt, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => {
            assert_eq!(
                failure.kind,
                lifecycle::WarpNativeSourceFailureKind::Corrupt
            );
        }
        _ => panic!("corrupt Warp source was not typed"),
    }

    let locked = lifecycle::WarpNativeSourceFailure::from_capture(
        &corrupt,
        CaptureError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_owned()),
        )),
        false,
    );
    assert_eq!(locked.kind, lifecycle::WarpNativeSourceFailureKind::Locked);

    let path = directory.path().join("rows.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Rows");
    insert_task(&conn, "conversation-1", "task-1", &[]);
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values ('conversation-1', 'task-bad', x'0A2078', '2026-07-24 12:00:01')",
        [],
    )
    .unwrap();
    drop(conn);
    let (_, sink) = scan(&path);
    assert_eq!(sink.rejection_count(), 1);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values ('conversation-1', 'task-incomplete', 7, '2026-07-24 12:00:01')",
        [],
    )
    .unwrap();
    drop(conn);
    let (_, sink) = scan(&path);
    assert_eq!(sink.rejection_count(), 2);
}

#[test]
fn persisted_checkpoint_is_bounded_and_has_no_artifact_authority() {
    const TASKS: usize = 512;

    let directory = tempdir().unwrap();
    let path = directory.path().join("bounded.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let transaction = conn.transaction().unwrap();
    insert_conversation(&transaction, "conversation-1", None, "Bounded");
    for index in 0..TASKS {
        insert_task(
            &transaction,
            "conversation-1",
            &format!("task-{index:04}"),
            &[],
        );
    }
    transaction.commit().unwrap();
    drop(conn);

    let (authority, _) = scan(&path);
    let encoded = serde_json::to_vec(&authority.persisted_state).unwrap();
    assert!(encoded.len() < lifecycle::WARP_NATIVE_PERSISTED_STATE_MAX_BYTES);
    let decoded: lifecycle::WarpNativePersistedState = serde_json::from_slice(&encoded).unwrap();
    assert!(!decoded.checkpoint_is_terminal());
    assert_eq!(
        decoded.checkpoint_frontier(),
        authority.persisted_state.checkpoint_frontier()
    );
    assert!(decoded.is_supported());
    assert!(authority
        .persisted_state
        .checkpoint_frontier()
        .last_task_rowid
        .is_some());

    let json: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        json["checkpoint"]["inventory"]["task_rows"].as_u64(),
        Some(TASKS as u64)
    );
    let checkpoint = json["checkpoint"].as_object().unwrap();
    assert!(!checkpoint.contains_key("keyset"));
    assert!(!checkpoint.contains_key("exact_evidence_sha256"));
    assert!(!json.as_object().unwrap().contains_key("artifact_path"));
    assert!(!json.as_object().unwrap().contains_key("evidence_path"));

    for digest_field in ["source_integrity_digest", "core_generation_digest"] {
        let mut mismatched = json.clone();
        let digest = mismatched[digest_field].as_str().unwrap();
        let first = if digest.starts_with('0') { "1" } else { "0" };
        mismatched[digest_field] = serde_json::Value::String(format!("{first}{}", &digest[1..]));
        let mismatched: lifecycle::WarpNativePersistedState =
            serde_json::from_value(mismatched).unwrap();
        assert!(
            !mismatched.is_supported(),
            "{digest_field} was not tied to its checkpoint frontier bytes"
        );
    }
}

#[test]
fn giant_provider_key_never_enters_bounded_durable_state() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("giant-key.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Giant key");
    let giant_task_id = "k".repeat(200_000);
    insert_task(&conn, "conversation-1", &giant_task_id, &[]);
    drop(conn);

    let (authority, sink) = scan(&path);
    assert!(authority.source_complete);
    assert!(!sink.pages.is_empty());
    let encoded = serde_json::to_vec(&authority.persisted_state).unwrap();
    assert!(encoded.len() < lifecycle::WARP_NATIVE_PERSISTED_STATE_MAX_BYTES);
    assert!(!encoded
        .windows(giant_task_id.len())
        .any(|window| window == giant_task_id.as_bytes()));
    assert!(authority
        .persisted_state
        .checkpoint_frontier()
        .last_task_rowid
        .is_some());
}

#[test]
fn unrepresentable_cursor_key_returns_compatibility_before_sink_mutation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("cursor-key.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Cursor key");
    conn.execute(
        "insert into agent_tasks
         (id, conversation_id, task_id, task, last_modified_at)
         values (-1, 'conversation-1', 'negative-rowid', ?1, '2026-07-24 12:00:01')",
        [task("negative-rowid", &[])],
    )
    .unwrap();
    drop(conn);

    let mut sink = CollectingSink::default();
    let error = scan_warp_nativepath(&path, &mut sink).unwrap_err();
    assert!(error.to_string().contains("positive 64-bit source rowids"));
    assert!(sink.pages.is_empty());
    assert!(sink.pro_pages.is_empty());
    match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => assert_eq!(
            failure.kind,
            lifecycle::WarpNativeSourceFailureKind::SchemaIncompatible
        ),
        _ => panic!("unrepresentable Warp cursor key was not a typed compatibility failure"),
    }
}

#[test]
fn local_scale_scan_stays_within_page_bounds() {
    const SESSIONS: usize = 80;
    const MESSAGES_PER_SESSION: usize = 100;

    let directory = tempdir().unwrap();
    let path = directory.path().join("scale.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let transaction = conn.transaction().unwrap();
    for session_index in 0..SESSIONS {
        let conversation_id = format!("conversation-{session_index:04}");
        let parent = (session_index > 0).then_some("conversation-0000");
        insert_conversation(&transaction, &conversation_id, parent, "Scale");
        let task_id = format!("task-{session_index:04}");
        let mut messages = Vec::with_capacity(MESSAGES_PER_SESSION);
        for message_index in 0..MESSAGES_PER_SESSION {
            let sequence = session_index * MESSAGES_PER_SESSION + message_index;
            let arm = if message_index % 10 == 9 {
                tool_result_arm(&format!("request-{sequence:08}"), &[b'x'; 128])
            } else if message_index % 10 == 8 {
                tool_call_arm(2)
            } else if message_index % 2 == 0 {
                text_arm(2, &format!("user {sequence:08}"))
            } else {
                text_arm(3, &format!("assistant {sequence:08}"))
            };
            messages.push(message(
                &format!("message-{sequence:08}"),
                &task_id,
                &format!("request-{sequence:08}"),
                sequence as u64,
                &[arm],
            ));
        }
        insert_task(&transaction, &conversation_id, &task_id, &messages);
    }
    transaction.commit().unwrap();
    drop(conn);

    let (authority, sink) = scan(&path);
    let total_messages = (SESSIONS * MESSAGES_PER_SESSION) as u64;
    let excluded = (SESSIONS * (MESSAGES_PER_SESSION / 10)) as u64;
    assert_eq!(authority.counters.task_rows, SESSIONS as u64);
    assert_eq!(authority.counters.native_result_records, excluded);
    assert_eq!(
        authority.counters.retained_events,
        total_messages - excluded
    );
    assert!(sink.pages.len() > 1);
    assert!(sink.pages.iter().all(|page| {
        page.row_count() <= WARP_NATIVE_PAGE_MAX_ROWS
            && page.estimated_bytes <= WARP_NATIVE_PAGE_MAX_BYTES
    }));
}

#[test]
fn hundred_thousand_events_keep_identity_retention_task_local() {
    const TASKS: usize = 1_001;
    const MESSAGES_PER_TASK: usize = 100;
    const EVENTS: usize = TASKS * MESSAGES_PER_TASK;

    let directory = tempdir().unwrap();
    let path = directory.path().join("identity-scale.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let transaction = conn.transaction().unwrap();
    insert_conversation(&transaction, "conversation-scale", None, "Identity Scale");
    for task_index in 0..TASKS {
        let task_id = format!("task-{task_index:04}");
        let mut messages = Vec::with_capacity(MESSAGES_PER_TASK);
        for message_index in 0..MESSAGES_PER_TASK {
            messages.push(message(
                &format!("message-{message_index:03}"),
                &task_id,
                &format!("request-{message_index:03}"),
                (task_index * MESSAGES_PER_TASK + message_index) as u64,
                &[text_arm(
                    if message_index % 2 == 0 { 2 } else { 3 },
                    "bounded identity event",
                )],
            ));
        }
        insert_task(&transaction, "conversation-scale", &task_id, &messages);
    }
    transaction.commit().unwrap();
    drop(conn);

    let mut sink = DiscardingSink::default();
    let authority = complete(scan_warp_nativepath(&path, &mut sink).unwrap());

    assert_eq!(authority.counters.task_rows, TASKS as u64);
    assert_eq!(authority.counters.retained_events, EVENTS as u64);
    assert_eq!(
        authority.counters.peak_task_identity_entries,
        MESSAGES_PER_TASK as u64
    );
    assert_eq!(authority.counters.hierarchy_nodes_retained, 1);
    assert_eq!(authority.counters.peak_session_metadata_rows, 1);
    assert_eq!(sink.sessions, 1);
    assert_eq!(sink.events, EVENTS);
    assert_eq!(sink.rejections, 0);
    assert!(sink.pages > 1);
    assert!(sink.max_page_rows <= WARP_NATIVE_PAGE_MAX_ROWS);
    assert!(sink.max_page_bytes <= WARP_NATIVE_PAGE_MAX_BYTES);
}
