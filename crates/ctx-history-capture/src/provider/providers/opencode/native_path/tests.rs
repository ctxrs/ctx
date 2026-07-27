use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection};
use serde_json::json;

use super::{
    OpenCodeNativeEventKind, OpenCodeNativePage, OpenCodeNativePageLimits,
    OpenCodeNativePathReader, OpenCodeNativeProOutputPage, OpenCodeNativeProfile,
    OpenCodeNativeRejectionKind, OpenCodeNativeScanSummary, OpenCodeNativeSchemaFamily,
    OpenCodeNativeSourceSelection,
};

fn create_session_table(conn: &Connection) {
    conn.execute_batch(
        "create table session (
             id text primary key,
             parent_id text,
             title text,
             directory text,
             time_created integer not null,
             time_updated integer not null
         );",
    )
    .unwrap();
}

pub(super) fn create_family_database(
    path: &Path,
    family: OpenCodeNativeSchemaFamily,
) -> Connection {
    let conn = Connection::open(path).unwrap();
    create_session_table(&conn);
    match family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => conn
            .execute_batch(
                "create table session_message (
                     id text primary key,
                     session_id text not null,
                     type text not null,
                     seq integer not null,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );",
            )
            .unwrap(),
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => conn
            .execute_batch(
                "create table session_message (
                     id text primary key,
                     session_id text not null,
                     type text not null,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );",
            )
            .unwrap(),
        OpenCodeNativeSchemaFamily::SessionEntry => conn
            .execute_batch(
                "create table session_entry (
                     id text primary key,
                     session_id text not null,
                     type text not null,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );",
            )
            .unwrap(),
        OpenCodeNativeSchemaFamily::LegacyMessage => conn
            .execute_batch(
                "create table message (
                     id text primary key,
                     session_id text not null,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );",
            )
            .unwrap(),
        OpenCodeNativeSchemaFamily::MessagePart => conn
            .execute_batch(
                "create table message (
                     id text primary key,
                     session_id text not null,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );
                 create table part (
                     id text primary key,
                     message_id text not null,
                     session_id text not null,
                     type text,
                     time_created integer not null,
                     time_updated integer not null,
                     data text not null
                 );",
            )
            .unwrap(),
    }
    conn.pragma_update(None, "user_version", 8).unwrap();
    conn
}

pub(super) fn insert_session(conn: &Connection, id: &str, parent_id: Option<&str>, created: i64) {
    conn.execute(
        "insert into session
         (id, parent_id, title, directory, time_created, time_updated)
         values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            parent_id,
            format!("title-{id}"),
            format!("/workspace/{id}"),
            created,
            created + 10,
        ],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)] // Keeps native schema fixture inserts explicit.
pub(super) fn insert_row_event(
    conn: &Connection,
    family: OpenCodeNativeSchemaFamily,
    id: &str,
    session_id: &str,
    entry_type: &str,
    sequence: i64,
    created: i64,
    data: &str,
) {
    match family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => {
            conn.execute(
                "insert into session_message
                 (id, session_id, type, seq, time_created, time_updated, data)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, session_id, entry_type, sequence, created, created, data],
            )
            .unwrap();
        }
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => {
            conn.execute(
                "insert into session_message
                 (id, session_id, type, time_created, time_updated, data)
                 values (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, session_id, entry_type, created, created, data],
            )
            .unwrap();
        }
        OpenCodeNativeSchemaFamily::SessionEntry => {
            conn.execute(
                "insert into session_entry
                 (id, session_id, type, time_created, time_updated, data)
                 values (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, session_id, entry_type, created, created, data],
            )
            .unwrap();
        }
        OpenCodeNativeSchemaFamily::LegacyMessage => {
            conn.execute(
                "insert into message
                 (id, session_id, time_created, time_updated, data)
                 values (?1, ?2, ?3, ?4, ?5)",
                params![id, session_id, created, created, data],
            )
            .unwrap();
        }
        OpenCodeNativeSchemaFamily::MessagePart => {
            panic!("row event helper does not insert message+part records")
        }
    }
}

#[allow(clippy::too_many_arguments)] // Keeps message/part relationship fixtures explicit.
pub(super) fn insert_part_event(
    conn: &Connection,
    message_id: &str,
    part_id: &str,
    session_id: &str,
    role: &str,
    part_type: &str,
    created: i64,
    data: &str,
) {
    conn.execute(
        "insert or ignore into message
         (id, session_id, time_created, time_updated, data)
         values (?1, ?2, ?3, ?4, ?5)",
        params![
            message_id,
            session_id,
            created,
            created,
            json!({"role": role, "time": {"created": created}}).to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "insert into part
         (id, message_id, session_id, type, time_created, time_updated, data)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![part_id, message_id, session_id, part_type, created, created, data],
    )
    .unwrap();
}

fn scan(
    path: &Path,
    limits: OpenCodeNativePageLimits,
) -> (Vec<OpenCodeNativePage>, OpenCodeNativeScanSummary) {
    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(path)).unwrap();
    let mut scanner = reader.scanner(limits).unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }
    let summary = scanner.finish().unwrap();
    (pages, summary)
}

fn scan_default(path: &Path) -> (Vec<OpenCodeNativePage>, OpenCodeNativeScanSummary) {
    scan(path, OpenCodeNativePageLimits::default())
}

fn scan_profile(
    path: &Path,
    profile: OpenCodeNativeProfile,
) -> (
    Vec<OpenCodeNativePage>,
    Vec<OpenCodeNativeProOutputPage>,
    OpenCodeNativeScanSummary,
) {
    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(path)).unwrap();
    let mut scanner = reader
        .scanner_with_profile(profile, OpenCodeNativePageLimits::default())
        .unwrap();
    let mut core = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        core.push(page);
    }
    let mut pro = Vec::new();
    while let Some(page) = scanner.next_pro_output_page().unwrap() {
        pro.push(page);
    }
    let summary = scanner.finish().unwrap();
    (core, pro, summary)
}

fn page_events(pages: &[OpenCodeNativePage]) -> Vec<&super::OpenCodeNativeEvent> {
    pages.iter().flat_map(|page| page.events.iter()).collect()
}

fn populate_family_fixture(path: &Path, family: OpenCodeNativeSchemaFamily) {
    let conn = create_family_database(path, family);
    insert_session(&conn, "session-a", None, 100);
    insert_session(&conn, "session-b", Some("session-a"), 200);
    if family == OpenCodeNativeSchemaFamily::MessagePart {
        insert_part_event(
            &conn,
            "message-a",
            "part-text",
            "session-a",
            "user",
            "text",
            101,
            &json!({"type": "text", "text": "retained text"}).to_string(),
        );
        insert_part_event(
            &conn,
            "message-b",
            "part-output",
            "session-b",
            "assistant",
            "tool",
            201,
            &json!({
                "type": "tool",
                "state": {
                    "status": "completed",
                    "output": "excluded output"
                }
            })
            .to_string(),
        );
    } else {
        insert_row_event(
            &conn,
            family,
            "message-a",
            "session-a",
            "user",
            1,
            101,
            &json!({"role": "user", "text": "retained text"}).to_string(),
        );
        let (entry_type, role) = if family == OpenCodeNativeSchemaFamily::LegacyMessage {
            ("message", "tool")
        } else {
            ("tool_result", "tool")
        };
        insert_row_event(
            &conn,
            family,
            "message-output",
            "session-b",
            entry_type,
            1,
            201,
            &json!({"role": role, "output": "excluded output"}).to_string(),
        );
    }
    drop(conn);
}

#[test]
fn opencode_nativepath_routes_every_supported_schema_family_explicitly() {
    let families = [
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
        OpenCodeNativeSchemaFamily::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage,
        OpenCodeNativeSchemaFamily::MessagePart,
    ];
    for family in families {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join(format!("{}.db", family.label()));
        populate_family_fixture(&path, family);
        let (pages, summary) = scan_default(&path);
        assert_eq!(summary.schema_family, family);
        assert_eq!(summary.metrics.native_sessions, 2);
        assert_eq!(summary.metrics.native_events, 2);
        assert_eq!(summary.metrics.retained_events, 1);
        assert_eq!(summary.metrics.excluded_outputs, 1);
        assert_eq!(summary.metrics.rejected_records, 0);
        assert_eq!(summary.metrics.output_content_cells_transferred, 0);
        assert_eq!(summary.metrics.output_content_bytes_transferred, 0);
        assert_eq!(page_events(&pages)[0].searchable_text, "retained text");
        assert_eq!(summary.metrics.snapshot_session_rows_indexed, 2);
    }
}

#[test]
fn opencode_nativepath_routes_mixed_families_by_population_and_join_authority() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let migrated = temp.path().join("migrated.db");
    let conn = create_family_database(
        &migrated,
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
    );
    conn.execute_batch(
        "create table session_entry (
             id text primary key,
             session_id text not null,
             type text not null,
             time_created integer not null,
             time_updated integer not null,
             data text not null
         );",
    )
    .unwrap();
    insert_session(&conn, "session-entry-owner", None, 10);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionEntry,
        "entry-visible",
        "session-entry-owner",
        "user",
        0,
        11,
        r#"{"role":"user","text":"populated entry wins"}"#,
    );
    drop(conn);
    let (pages, summary) = scan_default(&migrated);
    assert_eq!(
        summary.schema_family,
        OpenCodeNativeSchemaFamily::SessionEntry
    );
    assert_eq!(
        page_events(&pages)[0].searchable_text,
        "populated entry wins"
    );

    let legacy = temp.path().join("legacy-with-empty-parts.db");
    let conn = create_family_database(&legacy, OpenCodeNativeSchemaFamily::MessagePart);
    insert_session(&conn, "legacy-owner", None, 20);
    conn.execute(
        "insert into message
         (id, session_id, time_created, time_updated, data)
         values ('legacy-visible', 'legacy-owner', 21, 21, ?1)",
        [r#"{"role":"user","text":"legacy survives empty part"}"#],
    )
    .unwrap();
    conn.execute(
        "insert into part
         (id, message_id, session_id, type, time_created, time_updated, data)
         values ('orphan-part', 'missing-parent', 'legacy-owner', 'text', 22, 22, ?1)",
        [r#"{"type":"text","text":"orphan must not select the join family"}"#],
    )
    .unwrap();
    drop(conn);
    let (pages, summary) = scan_default(&legacy);
    assert_eq!(
        summary.schema_family,
        OpenCodeNativeSchemaFamily::LegacyMessage
    );
    assert_eq!(
        page_events(&pages)[0].searchable_text,
        "legacy survives empty part"
    );
}

#[test]
fn opencode_nativepath_no_seq_families_synthesize_stable_native_order() {
    for family in [
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
        OpenCodeNativeSchemaFamily::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage,
    ] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("no-seq.db");
        let conn = create_family_database(&path, family);
        insert_session(&conn, "session-a", None, 10);
        insert_row_event(
            &conn,
            family,
            "message-late",
            "session-a",
            "assistant",
            0,
            30,
            r#"{"role":"assistant","text":"late"}"#,
        );
        insert_row_event(
            &conn,
            family,
            "message-early-z",
            "session-a",
            "user",
            0,
            20,
            r#"{"role":"user","text":"early z"}"#,
        );
        insert_row_event(
            &conn,
            family,
            "message-early-a",
            "session-a",
            "user",
            0,
            20,
            r#"{"role":"user","text":"early a"}"#,
        );
        drop(conn);
        let (pages, summary) = scan(&path, OpenCodeNativePageLimits::new(1, 4096).unwrap());
        assert_eq!(summary.schema_family, family);
        assert_eq!(
            page_events(&pages)
                .iter()
                .map(|event| event.native_identity.as_str())
                .collect::<Vec<_>>(),
            ["message-early-a", "message-early-z", "message-late"]
        );
    }
}

#[test]
fn opencode_nativepath_excludes_every_tool_result_before_hydration_and_derivation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("output.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::MessagePart);
    insert_session(&conn, "session-a", None, 10);
    insert_part_event(
        &conn,
        "message-text",
        "part-text",
        "session-a",
        "user",
        "text",
        11,
        r#"{"type":"text","text":"safe conversation"}"#,
    );
    insert_part_event(
        &conn,
        "message-call",
        "part-call",
        "session-a",
        "assistant",
        "tool",
        12,
        &json!({
            "type": "tool",
            "tool": "write_file",
            "state": {
                "status": "pending",
                "input": {"path": "src/safe.rs"}
            }
        })
        .to_string(),
    );
    let sentinel = format!(
        "OUTPUT_SENTINEL_DO_NOT_TRANSFER:{}:src/output-only.rs",
        "x".repeat(64 * 1024)
    );
    for (index, value) in [
        json!({
            "type": "tool",
            "state": {"status": "completed", "output": sentinel}
        }),
        json!({
            "type": "tool",
            "state": {"status": "failed", "output": "src/failed-output.rs"}
        }),
        json!({
            "type": "tool",
            "state": {"status": "timeout", "output": "src/timeout-output.rs"}
        }),
        json!({
            "type": "tool_result",
            "state": {"status": "future-unknown"}
        }),
    ]
    .into_iter()
    .enumerate()
    {
        insert_part_event(
            &conn,
            &format!("message-output-{index}"),
            &format!("part-output-{index}"),
            "session-a",
            "assistant",
            if index == 3 { "tool_result" } else { "tool" },
            20 + index as i64,
            &value.to_string(),
        );
    }
    drop(conn);

    let (pages, summary) = scan(&path, OpenCodeNativePageLimits::new(16, 4 * 1024).unwrap());
    assert_eq!(summary.metrics.retained_events, 4);
    assert_eq!(summary.metrics.excluded_outputs, 4);
    assert_eq!(summary.metrics.retained_content_cells_transferred, 4);
    assert_eq!(summary.metrics.output_content_cells_transferred, 0);
    assert_eq!(summary.metrics.output_content_bytes_transferred, 0);
    assert_eq!(summary.metrics.output_hashes_built, 0);
    assert_eq!(summary.metrics.output_previews_built, 0);
    assert_eq!(summary.metrics.output_touches_built, 0);
    assert_eq!(summary.metrics.output_fts_documents_built, 0);
    let events = page_events(&pages);
    assert_eq!(events[1].kind, OpenCodeNativeEventKind::ToolCall);
    assert_eq!(events[1].file_touches[0].path, "src/safe.rs");
    let retained_debug = format!("{events:?}");
    assert!(!retained_debug.contains("OUTPUT_SENTINEL_DO_NOT_TRANSFER"));
    assert!(!retained_debug.contains("src/failed-output.rs"));
    assert!(!retained_debug.contains("src/timeout-output.rs"));
    for event in events.iter().filter(|event| {
        matches!(
            event.kind,
            OpenCodeNativeEventKind::ToolOutput | OpenCodeNativeEventKind::CommandOutput
        )
    }) {
        assert!(event.body.get("body").is_none());
        assert!(event.body.get("output_preview").is_none());
    }
}

#[test]
fn opencode_nativepath_core_is_profile_invariant_and_pro_fans_out_exact_subrecords() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("fanout.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::MessagePart);
    conn.execute("alter table session add column agent text", [])
        .unwrap();
    insert_session(&conn, "root", None, 1);
    insert_session(&conn, "child", Some("root"), 2);
    conn.execute(
        "update session set agent = 'build-agent' where id = 'child'",
        [],
    )
    .unwrap();
    insert_part_event(
        &conn,
        "message-text",
        "part-text",
        "child",
        "assistant",
        "text",
        3,
        r#"{"type":"text","text":"safe"}"#,
    );
    insert_part_event(
        &conn,
        "message-multiple",
        "part-multiple",
        "child",
        "assistant",
        "tool",
        4,
        &json!({
            "type": "tool",
            "tool": "bash",
            "callID": "call-multiple",
            "state": {
                "status": "completed",
                "input": {"command": "printf test"},
                "output": [
                    {"status": "completed", "output": "first"},
                    {"status": "failed", "exit_code": 7, "output": ""}
                ]
            }
        })
        .to_string(),
    );
    insert_part_event(
        &conn,
        "message-empty-array",
        "part-empty-array",
        "child",
        "assistant",
        "tool_result",
        5,
        r#"{"type":"tool_result","state":{"status":"completed","output":[]}}"#,
    );
    drop(conn);

    let (core_only, no_pro, core_summary) = scan_profile(&path, OpenCodeNativeProfile::CoreOnly);
    let (core_and_pro, pro_pages, pro_summary) =
        scan_profile(&path, OpenCodeNativeProfile::CoreAndPro);

    assert!(no_pro.is_empty());
    assert_eq!(core_only, core_and_pro);
    assert_eq!(core_summary.semantic_digest, pro_summary.semantic_digest);
    assert!(core_only.iter().all(|page| {
        page.accounting.logical_units <= 64
            && page.accounting.conservative_serialized_bytes <= 8 * 1024 * 1024
    }));
    assert!(pro_pages.iter().all(|page| {
        page.accounting.logical_units <= 64
            && page.accounting.conservative_serialized_bytes <= 8 * 1024 * 1024
    }));
    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.observations.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].content, b"first");
    assert!(outputs[1].content.is_empty());
    assert_eq!(outputs[2].content, b"[]");
    assert_eq!(outputs[1].outcome.outcome, crate::OutputOutcome::Failure);
    assert_eq!(outputs[1].outcome.exit_code, Some(7));
    assert_eq!(outputs[0].associations.direct_session_id, "child");
    assert_eq!(outputs[0].associations.root_session_id, "root");
    assert_eq!(
        outputs[0].associations.parent_session_id.as_deref(),
        Some("root")
    );
    assert_eq!(
        outputs[0].associations.agent_id.as_deref(),
        Some("build-agent")
    );
    assert_eq!(
        outputs[0].coordinate.unit_key,
        "opencode_sqlite:child:message-multiple:part-multiple:output"
    );
    assert_eq!(
        outputs[1].coordinate.unit_key,
        "opencode_sqlite:child:message-multiple:part-multiple:output:subrecord:1"
    );
    assert_eq!(
        outputs[0].locator.kind,
        super::super::content_locator::OPENCODE_LOCATOR_KIND
    );
    assert!(pro_pages.last().unwrap().terminal);
    assert!(pro_pages.last().unwrap().next_frontier.terminal);

    let core_debug = format!("{core_only:?}");
    assert!(!core_debug.contains("first"));
}

#[test]
fn opencode_nativepath_pro_pages_are_independently_bounded_and_rejections_do_not_touch_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("pro-bounds.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1);
    let outputs = (0..130)
        .map(|index| json!({"status": "completed", "output": format!("value-{index}")}))
        .collect::<Vec<_>>();
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "many",
        "session-a",
        "tool_result",
        1,
        2,
        &json!({
            "type": "tool_result",
            "state": {"status": "completed", "output": outputs}
        })
        .to_string(),
    );
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "oversized",
        "session-a",
        "tool_result",
        2,
        3,
        &json!({
            "type": "tool_result",
            "state": {
                "status": "completed",
                "output": "x".repeat(8 * 1024 * 1024)
            }
        })
        .to_string(),
    );
    conn.execute(
        r#"insert into session_message
           (id, session_id, type, seq, time_created, time_updated, data)
           values ('duplicate', 'session-a', 'tool_result', 3, 4, 4, ?1)"#,
        [r#"{"type":"tool_result","output":"first","output":"second"}"#],
    )
    .unwrap();
    drop(conn);

    let (core_only, _, _) = scan_profile(&path, OpenCodeNativeProfile::CoreOnly);
    let (core_and_pro, pro, _) = scan_profile(&path, OpenCodeNativeProfile::CoreAndPro);
    assert_eq!(core_only, core_and_pro);
    assert!(pro.len() >= 3);
    assert!(pro.iter().all(|page| {
        page.accounting.logical_units <= 64
            && page.accounting.conservative_serialized_bytes <= 8 * 1024 * 1024
    }));
    assert_eq!(
        pro.iter()
            .map(|page| page.observations.len())
            .sum::<usize>(),
        130
    );
    assert_eq!(
        pro.iter().map(|page| page.rejections.len()).sum::<usize>(),
        2
    );
    assert!(pro
        .iter()
        .flat_map(|page| page.rejections.iter())
        .any(|rejection| {
            rejection.kind == super::OpenCodeNativeProRejectionKind::OversizedOutput
        }));
    assert!(pro
        .iter()
        .flat_map(|page| page.rejections.iter())
        .any(|rejection| {
            rejection.kind == super::OpenCodeNativeProRejectionKind::MalformedOutput
        }));
    assert!(!format!("{core_only:?}").contains("value-0"));

    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(&path)).unwrap();
    let mut first_attempt = reader
        .scanner_with_profile(
            OpenCodeNativeProfile::CoreAndPro,
            OpenCodeNativePageLimits::default(),
        )
        .unwrap();
    let committed = first_attempt.next_pro_output_page().unwrap().unwrap();
    let restart_frontier = committed.next_frontier;
    let mut expected_remaining = Vec::new();
    while let Some(page) = first_attempt.next_pro_output_page().unwrap() {
        expected_remaining.push(page.identity);
    }
    let receipt = first_attempt.finish_pro_replay().unwrap();
    assert!(receipt.complete);
    assert!(receipt.frontier.terminal);

    let restarted_reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(&path)).unwrap();
    let mut restarted = restarted_reader
        .scanner_with_profile(
            OpenCodeNativeProfile::CoreAndPro,
            OpenCodeNativePageLimits::default(),
        )
        .unwrap();
    restarted.resume_pro_from(restart_frontier).unwrap();
    let mut actual_remaining = Vec::new();
    while let Some(page) = restarted.next_pro_output_page().unwrap() {
        actual_remaining.push(page.identity);
    }
    assert_eq!(actual_remaining, expected_remaining);
    assert!(restarted.finish_pro_replay().unwrap().complete);
}

#[test]
fn opencode_nativepath_core_profile_equality_holds_for_every_schema_family() {
    for family in [
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
        OpenCodeNativeSchemaFamily::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage,
        OpenCodeNativeSchemaFamily::MessagePart,
    ] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join(format!("profile-{}.db", family.label()));
        populate_family_fixture(&path, family);
        let (core_only, _, _) = scan_profile(&path, OpenCodeNativeProfile::CoreOnly);
        let (core_and_pro, pro, _) = scan_profile(&path, OpenCodeNativeProfile::CoreAndPro);
        assert_eq!(core_only, core_and_pro, "family {}", family.label());
        assert!(core_only.last().unwrap().terminal);
        assert_eq!(
            core_only.last().unwrap().next_frontier.phase,
            super::OpenCodeNativeScanPhase::Complete
        );
        assert!(core_only
            .windows(2)
            .all(|pages| { pages[0].next_frontier == pages[1].expected_frontier }));
        assert!(core_only.iter().all(|page| page.identity.0 != [0_u8; 32]));
        assert_eq!(
            pro.iter()
                .map(|page| page.observations.len())
                .sum::<usize>(),
            1,
            "family {}",
            family.label()
        );
    }
}

#[test]
fn opencode_nativepath_output_only_rewrite_changes_pro_without_changing_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("output-rewrite.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 1);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "result-a",
        "session-a",
        "tool_result",
        1,
        2,
        r#"{"type":"tool_result","state":{"status":"completed","output":"before"}}"#,
    );
    drop(conn);
    let (before_core, before_pro, _) = scan_profile(&path, OpenCodeNativeProfile::CoreAndPro);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update session_message
         set data = '{\"type\":\"tool_result\",\"state\":{\"status\":\"completed\",\"output\":\"after\"}}'
         where id = 'result-a'",
        [],
    )
    .unwrap();
    drop(conn);
    let (after_core, after_pro, _) = scan_profile(&path, OpenCodeNativeProfile::CoreAndPro);

    assert_eq!(before_core, after_core);
    assert_eq!(before_pro[0].observations[0].content, b"before");
    assert_eq!(after_pro[0].observations[0].content, b"after");
}

#[test]
fn opencode_nativepath_audited_visitor_blocks_adversarial_result_shapes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("adversarial-output.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::MessagePart);
    insert_session(&conn, "session-a", None, 10);
    insert_part_event(
        &conn,
        "message-safe",
        "part-safe",
        "session-a",
        "user",
        "text",
        11,
        r#"{"type":"text","text":"safe sibling"}"#,
    );
    let sentinel = "OUTPUT_SENTINEL_MUST_NOT_CROSS";
    let adversarial = [
        (
            "duplicate",
            format!(
                r#"{{"type":"tool","state":{{"status":"pending","input":{{"command":"safe"}}}},"state":{{"status":"completed","output":"{sentinel}"}}}}"#
            ),
            "tool",
        ),
        (
            "nested",
            format!(
                r#"{{"type":"text","content":[{{"type":"tool_result","text":"{sentinel}"}}]}}"#
            ),
            "text",
        ),
        (
            "unknown",
            format!(r#"{{"type":"future","nested":{{"result":"{sentinel}"}}}}"#),
            "future",
        ),
        (
            "short",
            r#"{"type":"future","result":"x"}"#.to_owned(),
            "future",
        ),
        (
            "numeric",
            r#"{"type":"future","result":7}"#.to_owned(),
            "future",
        ),
        (
            "input-nested",
            format!(r#"{{"type":"text","input":{{"result":"{sentinel}"}}}}"#),
            "text",
        ),
        ("numeric-body", "7".to_owned(), "tool_result"),
    ];
    for (offset, (label, data, part_type)) in adversarial.into_iter().enumerate() {
        insert_part_event(
            &conn,
            &format!("message-{label}"),
            &format!("part-{label}"),
            "session-a",
            "assistant",
            part_type,
            20 + offset as i64,
            &data,
        );
    }
    drop(conn);

    let (pages, summary) = scan_default(&path);
    assert_eq!(summary.metrics.retained_events, 1);
    assert_eq!(summary.metrics.excluded_outputs, 7);
    assert_eq!(summary.metrics.rejected_records, 0);
    assert_eq!(summary.metrics.output_content_cells_transferred, 0);
    assert_eq!(page_events(&pages)[0].searchable_text, "safe sibling");
    assert!(!format!("{pages:?}").contains(sentinel));
}

#[test]
fn opencode_nativepath_supports_current_variants_without_retaining_output_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("current-schema.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 10);
    let sentinel = "CURRENT_OUTPUT_SENTINEL_MUST_NOT_CROSS";
    let retained = [
        (
            "assistant-current",
            "assistant",
            r#"{"role":"assistant","content":[{"type":"text","text":"current assistant"}],"tokens":{"input":3,"output":7}}"#
                .to_owned(),
        ),
        (
            "running-tool-input-only",
            "tool",
            r#"{"type":"tool","callID":"call-safe","tool":"bash","state":{"status":"running","input":{"command":"safe"}}}"#.to_owned(),
        ),
        (
            "agent",
            "agent-switched",
            r#"{"role":"assistant"}"#.to_owned(),
        ),
        (
            "model",
            "model-switched",
            r#"{"role":"assistant"}"#.to_owned(),
        ),
        (
            "synthetic",
            "synthetic",
            r#"{"role":"assistant"}"#.to_owned(),
        ),
        (
            "compaction",
            "compaction",
            r#"{"role":"assistant"}"#.to_owned(),
        ),
    ];
    for (offset, (id, kind, data)) in retained.into_iter().enumerate() {
        insert_row_event(
            &conn,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
            id,
            "session-a",
            kind,
            offset as i64,
            20 + offset as i64,
            &data,
        );
    }
    for (offset, data) in [
        format!(r#"{{"type":"text","content":[{{"type":"output","text":"{sentinel}"}}]}}"#),
        format!(r#"{{"type":"text","content":[{{"type":"tool_output","text":"{sentinel}"}}]}}"#),
        format!(r#"{{"type":"text","content":[{{"type":"command_output","text":"{sentinel}"}}]}}"#),
        format!(r#"{{"type":"tool","callID":"call-content","tool":"bash","state":{{"status":"running","input":{{"command":"safe"}},"content":"{sentinel}"}}}}"#),
        format!(r#"{{"type":"tool","callID":"call-structured","tool":"bash","state":{{"status":"running","input":{{"command":"safe"}},"structured":{{"secret":"{sentinel}"}}}}}}"#),
        format!(r#"{{"role":"assistant","content":[{{"type":"tool","callID":"call-nested","tool":"bash","state":{{"status":"running","input":{{"command":"safe"}},"content":"{sentinel}","structured":{{"secret":"{sentinel}"}}}}}}]}}"#),
        format!(r#"{{"type":"tool","callID":"call-legacy","tool":"bash","state":{{"status":"running","input":{{"command":"safe"}}}},"content":"{sentinel}","structured":{{"secret":"{sentinel}"}}}}"#),
    ]
    .into_iter()
    .enumerate()
    {
        insert_row_event(
            &conn,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
            &format!("output-{offset}"),
            "session-a",
            "assistant",
            20 + offset as i64,
            40 + offset as i64,
            &data,
        );
    }
    drop(conn);

    let (pages, summary) = scan_default(&path);
    assert_eq!(summary.metrics.retained_events, 6);
    assert_eq!(summary.metrics.excluded_outputs, 7);
    assert_eq!(summary.metrics.output_content_cells_transferred, 0);
    assert_eq!(page_events(&pages)[0].searchable_text, "current assistant");
    assert!(format!("{pages:?}").contains("call-safe"));
    assert!(!format!("{pages:?}").contains(sentinel));
}

#[test]
fn opencode_nativepath_bounds_session_metadata_per_cell_and_page() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("session-pages.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    for number in 0..3 {
        let id = format!("session-{number}");
        insert_session(&conn, &id, None, number);
        conn.execute(
            "update session set title = ?1, directory = ?2 where id = ?3",
            params!["t".repeat(320), "d".repeat(320), id],
        )
        .unwrap();
    }
    drop(conn);
    let (pages, _) = scan(&path, OpenCodeNativePageLimits::new(64, 1_024).unwrap());
    let session_pages = pages
        .iter()
        .filter(|page| !page.sessions.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(session_pages.len(), 3);
    assert!(session_pages.iter().all(|page| {
        page.sessions
            .iter()
            .map(|session| {
                session.native_identity.len()
                    + session.parent_identity.as_deref().map_or(0, str::len)
                    + session.title.as_deref().map_or(0, str::len)
                    + session.directory.as_deref().map_or(0, str::len)
                    + 64
            })
            .sum::<usize>()
            <= 1_024
    }));

    let oversized = temp.path().join("oversized-session.db");
    let conn = create_family_database(&oversized, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 10);
    conn.execute(
        "update session set title = ?1 where id = 'session-a'",
        ["x".repeat(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES)],
    )
    .unwrap();
    drop(conn);
    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(&oversized))
            .unwrap();
    assert!(reader.scanner(OpenCodeNativePageLimits::default()).is_err());
}

#[test]
fn opencode_nativepath_rejects_malformed_json_and_storage_cells_locally() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("malformed.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 10);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "valid",
        "session-a",
        "user",
        1,
        11,
        r#"{"role":"user","text":"valid sibling"}"#,
    );
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "malformed-result",
        "session-a",
        "tool_result",
        2,
        12,
        r#"{"role":"tool","output":"#,
    );
    conn.execute(
        "insert into session_message
         (id, session_id, type, seq, time_created, time_updated, data)
         values ('blob-cell', 'session-a', 'assistant', 3, 13, 13, ?1)",
        [rusqlite::types::Value::Blob(b"\xff\x00not-text".to_vec())],
    )
    .unwrap();
    drop(conn);

    let (pages, summary) = scan_default(&path);
    assert_eq!(summary.metrics.retained_events, 1);
    assert_eq!(summary.metrics.rejected_records, 2);
    assert_eq!(page_events(&pages)[0].searchable_text, "valid sibling");
    let kinds = pages
        .iter()
        .flat_map(|page| page.rejections.iter())
        .map(|rejection| rejection.kind)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        kinds,
        std::collections::BTreeSet::from([
            OpenCodeNativeRejectionKind::MalformedResultJson,
            OpenCodeNativeRejectionKind::UnsupportedStorageClass,
        ])
    );
}

#[test]
fn opencode_nativepath_preflights_oversized_json_before_the_visitor() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("oversized.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 10);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "valid-before",
        "session-a",
        "user",
        1,
        11,
        r#"{"role":"user","text":"valid before oversize"}"#,
    );
    let oversized = format!(
        r#"{{"role":"user","text":"{}"}}"#,
        "x".repeat(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES)
    );
    assert!(oversized.len() > crate::MAX_PROVIDER_SQLITE_VALUE_BYTES);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "oversized",
        "session-a",
        "user",
        2,
        12,
        &oversized,
    );
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "valid-after",
        "session-a",
        "assistant",
        3,
        13,
        r#"{"role":"assistant","text":"valid after oversize"}"#,
    );
    drop(conn);

    let (pages, summary) = scan_default(&path);
    assert_eq!(summary.metrics.native_events, 3);
    assert_eq!(summary.metrics.retained_events, 2);
    assert_eq!(summary.metrics.rejected_records, 1);
    assert_eq!(summary.metrics.json_records_visited, 2);
    assert!(summary.metrics.json_bytes_visited < crate::MAX_PROVIDER_SQLITE_VALUE_BYTES as u64);
    assert_eq!(
        page_events(&pages)
            .iter()
            .map(|event| event.searchable_text.as_str())
            .collect::<Vec<_>>(),
        ["valid before oversize", "valid after oversize"]
    );
    assert!(pages
        .iter()
        .flat_map(|page| &page.rejections)
        .any(|rejection| rejection.kind == OpenCodeNativeRejectionKind::OversizedRetainedContent));
}

#[test]
fn opencode_nativepath_snapshot_is_immutable_and_live_mutation_invalidates_finish() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("mutable.db");
    let conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "session-a", None, 10);
    insert_row_event(
        &conn,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-a",
        "session-a",
        "assistant",
        1,
        11,
        r#"{"role":"assistant","text":"before mutation"}"#,
    );
    drop(conn);

    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(&path)).unwrap();
    let writer = Connection::open(&path).unwrap();
    writer
        .execute(
            "update session_message
             set data = '{\"role\":\"assistant\",\"text\":\"after mutation\"}',
                 time_updated = 99
             where id = 'message-a'",
            [],
        )
        .unwrap();
    drop(writer);
    assert!(!reader.revalidate_live().unwrap());

    let mut scanner = reader.scanner(OpenCodeNativePageLimits::default()).unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }
    assert_eq!(page_events(&pages)[0].searchable_text, "before mutation");
    assert!(matches!(
        scanner.finish(),
        Err(crate::CaptureError::SourceChangedDuringCapture)
    ));

    let (pages, _) = scan_default(&path);
    assert_eq!(page_events(&pages)[0].searchable_text, "after mutation");
}

#[test]
fn opencode_nativepath_reads_committed_wal_without_touching_provider_files() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let build_path = temp.path().join("build.db");
    let writer = Connection::open(&build_path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    create_session_table(&writer);
    writer
        .execute_batch(
            "create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );",
        )
        .unwrap();
    writer.pragma_update(None, "user_version", 8).unwrap();
    insert_session(&writer, "session-a", None, 10);
    insert_row_event(
        &writer,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "message-a",
        "session-a",
        "assistant",
        1,
        11,
        r#"{"role":"assistant","text":"checkpointed"}"#,
    );
    writer
        .query_row("pragma wal_checkpoint(truncate)", [], |_row| Ok(()))
        .unwrap();
    writer
        .execute(
            "update session_message
             set data = '{\"role\":\"assistant\",\"text\":\"committed wal\"}'
             where id = 'message-a'",
            [],
        )
        .unwrap();

    let source_dir = temp.path().join("provider");
    fs::create_dir(&source_dir).unwrap();
    let source_path = source_dir.join("opencode.db");
    let build_wal = PathBuf::from(format!("{}-wal", build_path.display()));
    let source_wal = PathBuf::from(format!("{}-wal", source_path.display()));
    fs::copy(&build_path, &source_path).unwrap();
    fs::copy(&build_wal, &source_wal).unwrap();
    let source_shm = PathBuf::from(format!("{}-shm", source_path.display()));
    let database_before = fs::read(&source_path).unwrap();
    let wal_before = fs::read(&source_wal).unwrap();
    assert!(!source_shm.exists());

    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(&source_path))
            .unwrap();
    assert_ne!(reader.snapshot_path(), source_path);
    let mut scanner = reader.scanner(OpenCodeNativePageLimits::default()).unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }
    let summary = scanner.finish().unwrap();
    assert_eq!(page_events(&pages)[0].searchable_text, "committed wal");
    assert!(summary.complete);
    assert_eq!(fs::read(&source_path).unwrap(), database_before);
    assert_eq!(fs::read(&source_wal).unwrap(), wal_before);
    assert!(!source_shm.exists());
    drop(writer);
}

#[test]
fn opencode_nativepath_small_scale_uses_set_wise_pages_and_zero_output_transfer() {
    const EVENTS: i64 = 1_024;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("scale.db");
    let mut conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    insert_session(&conn, "scale-session", None, 10);
    let transaction = conn.transaction().unwrap();
    {
        let mut statement = transaction
            .prepare(
                "insert into session_message
                 (id, session_id, type, seq, time_created, time_updated, data)
                 values (?1, 'scale-session', ?2, ?3, ?4, ?4, ?5)",
            )
            .unwrap();
        for sequence in 1..=EVENTS {
            let (entry_type, data) = if sequence % 4 == 0 {
                (
                    "tool_result",
                    json!({
                        "role": "tool",
                        "output": format!("excluded-{sequence}")
                    })
                    .to_string(),
                )
            } else {
                (
                    "user",
                    json!({
                        "role": "user",
                        "text": format!("retained-{sequence}")
                    })
                    .to_string(),
                )
            };
            statement
                .execute(params![
                    format!("message-{sequence:05}"),
                    entry_type,
                    sequence,
                    100 + sequence,
                    data,
                ])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    drop(conn);

    let (_, summary) = scan(
        &path,
        OpenCodeNativePageLimits::new(64, 512 * 1024).unwrap(),
    );
    assert_eq!(summary.metrics.native_sessions, 1);
    assert_eq!(summary.metrics.native_events, EVENTS as u64);
    assert_eq!(summary.metrics.retained_events, 768);
    assert_eq!(summary.metrics.excluded_outputs, 256);
    assert_eq!(summary.metrics.session_page_queries, 2);
    assert_eq!(summary.metrics.event_metadata_page_queries, 17);
    assert_eq!(summary.metrics.retained_hydration_queries, 0);
    assert_eq!(summary.metrics.output_content_cells_transferred, 0);
    assert_eq!(summary.metrics.output_content_bytes_transferred, 0);
    assert_eq!(summary.metrics.source_session_rows_scanned, 1);
    assert_eq!(summary.metrics.source_event_rows_scanned, EVENTS as u64);
    assert_eq!(summary.metrics.snapshot_session_rows_indexed, 1);
    assert_eq!(summary.metrics.snapshot_event_rows_indexed, EVENTS as u64);
    assert_eq!(summary.metrics.snapshot_ordering_passes, 2);
    assert_eq!(summary.metrics.indexed_session_rows_read, 1);
    assert_eq!(summary.metrics.indexed_event_rows_read, EVENTS as u64);
    assert_eq!(summary.metrics.json_records_visited, 768);
}

#[test]
fn opencode_nativepath_snapshot_index_pages_use_the_integer_primary_key() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("query-plan.db");
    populate_family_fixture(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
    let reader =
        OpenCodeNativePathReader::acquire(OpenCodeNativeSourceSelection::exact(&path)).unwrap();
    let scanner = reader.scanner(OpenCodeNativePageLimits::default()).unwrap();
    let plan = scanner.index.event_page_query_plan().unwrap();
    assert!(plan
        .iter()
        .any(|detail| detail.contains("INTEGER PRIMARY KEY")));
    assert!(plan
        .iter()
        .all(|detail| !detail.contains("TEMP B-TREE") && !detail.starts_with("SCAN ")));
}

#[test]
#[ignore = "exact M1/L2 scale proof; run explicitly for NativePath acceptance"]
fn opencode_nativepath_exact_m1_l2_work_is_linear() {
    const PAGE_ROWS: u64 = 64;
    const M1_EVENTS: u64 = 65_536;
    const L2_EVENTS: u64 = 655_360;
    for events in [M1_EVENTS, L2_EVENTS] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join(format!("scale-{events}.db"));
        let mut conn = create_family_database(&path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
        insert_session(&conn, "scale-session", None, 10);
        let transaction = conn.transaction().unwrap();
        {
            let mut statement = transaction
                .prepare(
                    "insert into session_message
                     (id, session_id, type, seq, time_created, time_updated, data)
                     values (?1, 'scale-session', ?2, ?3, ?3, ?3, ?4)",
                )
                .unwrap();
            for sequence in 1..=events {
                statement
                    .execute(params![
                        format!("message-{sequence:07}"),
                        "user",
                        i64::try_from(sequence).unwrap(),
                        r#"{"role":"user","text":"r"}"#,
                    ])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        drop(conn);

        let (_, summary) = scan(
            &path,
            OpenCodeNativePageLimits::new(PAGE_ROWS as usize, 8 * 1024 * 1024).unwrap(),
        );
        assert_eq!(summary.metrics.source_event_rows_scanned, events);
        assert_eq!(summary.metrics.snapshot_event_rows_indexed, events);
        assert_eq!(summary.metrics.indexed_event_rows_read, events);
        assert_eq!(
            summary.metrics.event_metadata_page_queries,
            events.div_ceil(PAGE_ROWS) + 1
        );
        assert_eq!(summary.metrics.snapshot_ordering_passes, 2);
        assert_eq!(
            summary.metrics.source_event_rows_scanned
                + summary.metrics.snapshot_event_rows_indexed
                + summary.metrics.indexed_event_rows_read,
            events * 3
        );
    }
}
