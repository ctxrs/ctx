//! Authored migration/superset controls, not native producer captures.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::super::{model::OpenCodeNativeSchemaFamily, schema::OpenCodeNativeSchema};
use crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table session (id text primary key, time_created integer, time_updated integer);
         create table message (id text primary key, session_id text, time_created integer,
                               time_updated integer, data text);
         create table part (id text primary key, message_id text, session_id text,
                            time_created integer, time_updated integer, data text);
         create index part_message_id_id_idx on part(message_id,id);
         create table session_message (id text primary key, session_id text, type text,
                                       seq integer, time_created integer, time_updated integer, data text);
         insert into session values ('session-1',1,1);
         insert into message values ('user-1','session-1',1,1,'{\"role\":\"user\"}');
         insert into message values ('assistant-1','session-1',2,2,'{\"role\":\"assistant\"}');",
    ).unwrap();
    part(&conn, "p1", "user-1", json!({"type":"text","text":"first"}));
    part(
        &conn,
        "p2",
        "user-1",
        json!({"type":"text","text":"second"}),
    );
    part(
        &conn,
        "p3",
        "assistant-1",
        json!({"type":"text","text":"answer"}),
    );
    part(
        &conn,
        "p4",
        "assistant-1",
        json!({
            "type":"reasoning","text":"reason","time":{"start":2,"end":3}
        }),
    );
    part(
        &conn,
        "p5",
        "assistant-1",
        json!({
            "type":"tool","callID":"call-1","tool":"read",
            "state":{"status":"completed","input":{"filePath":"example.rs"},
                     "output":"file contents","metadata":{},"time":{"start":3,"end":4}}
        }),
    );
    conn.execute(
        "insert into session_message values ('user-1','session-1','user',1,1,1,?1)",
        [json!({"text":"first\n\nsecond","time":{"created":1}}).to_string()],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values ('assistant-1','session-1','assistant',2,2,4,?1)",
        [json!({"content":[
            {"type":"text","text":"answer"},
            {"type":"reasoning","text":"reason","time":{"created":2,"completed":3}},
            {"type":"tool","id":"call-1","name":"read",
             "state":{"status":"completed","input":{"filePath":"example.rs"},
                      "content":[{"type":"text","text":"file contents"}],"metadata":{}},
             "time":{"created":3,"completed":4}}
        ],"time":{"created":2,"completed":4}})
        .to_string()],
    )
    .unwrap();
    conn
}

fn part(conn: &Connection, id: &str, message: &str, value: Value) {
    conn.execute(
        "insert into part values (?1,?2,'session-1',1,1,?3)",
        params![id, message, value.to_string()],
    )
    .unwrap();
}

fn selected(conn: &Connection) -> OpenCodeNativeSchema {
    OpenCodeNativeSchema::probe(conn, &OPENCODE_SQLITE_DIALECT).unwrap()
}

#[test]
fn ordinary_dual_written_content_and_migrated_superset_select_v2() {
    let conn = fixture();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    assert_eq!(selected(&conn).session_table, "session");
    conn.execute_batch(
        "create table session_v2 (id text primary key, time_created integer, time_updated integer);
         insert into session_v2 select * from session;
         insert into session_v2 values ('session-2',3,3);
         insert into session_message values ('new','session-2','user',1,3,3,'{\"text\":\"new session\"}');",
    ).unwrap();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    assert_eq!(selected(&conn).session_table, "session_v2");
    conn.execute_batch("drop table session").unwrap();
    assert_eq!(selected(&conn).session_table, "session_v2");
}

#[test]
fn session_or_message_coverage_cannot_hide_missing_divergent_or_corrupt_parts() {
    for mutation in [
        "delete from session_message where id='assistant-1'",
        "update session_message set session_id='other' where id='assistant-1'",
        "update session_message set type='user' where id='assistant-1'",
        "update session_message set data=json_remove(data,'$.content[1]') where id='assistant-1'",
        "update session_message set data=json_set(data,'$.content[0].text','divergent') where id='assistant-1'",
        "update session_message set data=json_set(data,'$.content[2].state.input.filePath','different.rs') where id='assistant-1'",
        "update session_message set data=json_set(data,'$.content[2].state.content[0].text','different output') where id='assistant-1'",
        "update session_message set data=json_set(data,'$.content[2].id','other-call') where id='assistant-1'",
        "update session_message set data=json_set(data,'$.text','second') where id='user-1'",
        "update session_message set time_created=9 where id='user-1'",
        "update part set session_id='other' where id='p3'",
        "update part set message_id='orphan' where id='p3'",
        "update part set data='{' where id='p3'",
        "update part set data=x'7b7d' where id='p3'",
        "update part set data='null' where id='p3'",
        "update part set data='{\"type\":\"text\",\"text\":\"lost\",\"text\":\"answer\"}' where id='p3'",
        "update message set data='{\"role\":\"user\"}' where id='assistant-1'",
        "update message set data='{' where id='assistant-1'",
        "update part set time_created='invalid' where id='p3'",
        "update part set data=json_set(data,'$.type','future-part') where id='p3'",
        "update part set data=json_set(data,'$.ignored',json('true')) where id='p1'",
        "insert into part select 'p30',message_id,session_id,time_created,time_updated,data from part where id='p3'",
        "delete from message where id='assistant-1'",
    ] {
        let conn = fixture();
        conn.execute_batch(mutation).unwrap();
        let error = OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).unwrap_err();
        assert!(error.to_string().contains("ambiguous populated"), "{mutation}: {error}");
    }
}

#[test]
fn coverage_preserves_duplicate_part_multiplicity_and_known_metadata() {
    let conn = fixture();
    part(
        &conn,
        "p30",
        "assistant-1",
        json!({"type":"text","text":"answer"}),
    );
    // A repeated part is not proved by finding its text once anywhere in a row.
    assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err());
    conn.execute_batch(
        "update session_message set data=json_set(data,'$.content',json('[
        {\"type\":\"text\",\"text\":\"answer\"},
        {\"type\":\"text\",\"text\":\"answer\"},
        {\"type\":\"reasoning\",\"text\":\"reason\",\"time\":{\"created\":2,\"completed\":3}},
        {\"type\":\"tool\",\"id\":\"call-1\",\"name\":\"read\",\"state\":{
         \"status\":\"completed\",\"input\":{\"filePath\":\"example.rs\"},
         \"content\":[{\"type\":\"text\",\"text\":\"file contents\"}],\"metadata\":{}},
         \"time\":{\"created\":3,\"completed\":4}}
    ]')) where id='assistant-1'",
    )
    .unwrap();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    part(
        &conn,
        "p6",
        "assistant-1",
        json!({"type":"step-start","snapshot":"tree-a"}),
    );
    assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err());
    conn.execute_batch("update session_message set data=json_set(data,'$.snapshot.start','tree-a') where id='assistant-1'").unwrap();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
}

#[test]
fn empty_legacy_parts_and_synthetic_user_text_have_positive_controls() {
    let conn = fixture();
    conn.execute_batch("insert into message values ('empty','session-1',5,5,'{\"role\":\"assistant\"}');
        insert into session_message values ('empty','session-1','assistant',3,5,5,'{\"content\":[]}');
        update part set data=json_set(data,'$.synthetic',json('true')) where message_id='user-1';
        update session_message set type='synthetic' where id='user-1';").unwrap();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
}

#[test]
fn third_family_and_unsequenced_overlap_are_still_ambiguous() {
    for mutation in [
        "create table session_entry (id text primary key, session_id text, time_created integer, time_updated integer, data text);
         insert into session_entry values ('entry','session-1',1,1,'{\"role\":\"user\"}')",
        "alter table session_message drop column seq",
    ] {
        let conn = fixture();
        conn.execute_batch(mutation).unwrap();
        assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).unwrap_err().to_string().contains("ambiguous populated"));
    }
}

#[test]
fn completed_compaction_pairs_one_summary_and_requires_its_entire_text() {
    let conn = fixture();
    conn.execute_batch(
        "insert into message values ('compact','session-1',5,5,'{\"role\":\"user\"}');
         insert into message values ('summary','session-1',6,7,
             '{\"role\":\"assistant\",\"parentID\":\"compact\",\"summary\":true,\"time\":{\"completed\":7}}');
         insert into session_message values ('compact','session-1','compaction',3,5,7,
             '{\"status\":\"completed\",\"reason\":\"auto\",\"summary\":\"one\\n\\ntwo\"}');",
    ).unwrap();
    part(
        &conn,
        "compact-part",
        "compact",
        json!({"type":"compaction","auto":true}),
    );
    part(
        &conn,
        "summary-1",
        "summary",
        json!({"type":"text","text":"one"}),
    );
    part(
        &conn,
        "summary-2",
        "summary",
        json!({"type":"text","text":"two"}),
    );
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    conn.execute_batch(
        "update session_message set data=json_set(data,'$.summary','one') where id='compact'",
    )
    .unwrap();
    assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err());
    conn.execute_batch("update session_message set data=json_set(data,'$.summary','one' || char(10) || char(10) || 'two') where id='compact';
        insert into message select 'other-summary',session_id,time_created,time_updated,data from message where id='summary';").unwrap();
    assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err());
}

#[test]
fn native_step_metadata_aggregation_accepts_multiple_steps_but_not_divergent_totals() {
    let conn = fixture();
    for (id, value) in [
        ("p6", json!({"type":"step-start","snapshot":"first"})),
        (
            "p7",
            json!({"type":"step-finish","snapshot":"middle","cost":0.25,"tokens":{"input":10,"output":5}}),
        ),
        ("p8", json!({"type":"step-start","snapshot":"middle"})),
        (
            "p9",
            json!({"type":"step-finish","snapshot":"last","cost":0.5,"tokens":{"input":20,"output":8}}),
        ),
    ] {
        part(&conn, id, "assistant-1", value);
    }
    conn.execute_batch(
        "update session_message set data=json_set(data,
        '$.snapshot',json('{\"start\":\"first\",\"end\":\"last\"}'),
        '$.cost',0.75,'$.tokens',json('{\"input\":20,\"output\":8}')) where id='assistant-1'",
    )
    .unwrap();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    conn.execute_batch(
        "update session_message set data=json_set(data,'$.cost',0.5) where id='assistant-1'",
    )
    .unwrap();
    assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err());
}

#[test]
fn covering_json_timestamp_must_be_valid_and_equal_to_its_sql_creation_time() {
    for timestamp in [
        json!("invalid"),
        Value::Null,
        json!(i64::MAX),
        json!(2),
        json!(1.5),
    ] {
        let conn = fixture();
        conn.execute("update session_message set data=json_set(data,'$.time.created',json(?1)) where id='user-1'",
            [timestamp.to_string()]).unwrap();
        assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT)
            .unwrap_err()
            .to_string()
            .contains("ambiguous populated"));
    }
    let conn = fixture();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    conn.execute_batch(
        "update session_message set data=json_remove(data,'$.time') where id='user-1'",
    )
    .unwrap();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        "missing JSON time uses the validated native SQL time"
    );
}

#[test]
fn known_parts_cannot_hide_unmapped_native_metadata_or_unknown_retained_fields() {
    for mutation in [
        "update part set data=json_set(data,'$.metadata',json('{\"note\":\"legacy-only\"}')) where id='p1'",
        "update part set data=json_set(data,'$.future',json('{\"nested\":\"legacy-only\"}')) where id='p3'",
        "update part set data=json_set(data,'$.future',json('{\"nested\":\"legacy-only\"}')) where id='p5'",
        "update part set data=json_set(data,'$.state.future',json('{\"nested\":\"legacy-only\"}')) where id='p5'",
        "update part set data=json_set(data,'$.time.future','legacy-only') where id='p4'",
        "update part set data=json_set(data,'$.state.time.future','legacy-only') where id='p5'",
        "update part set data=json_set(data,'$.state.title','original') where id='p5'; update session_message set data=json_set(data,'$.content[2].state.title','different') where id='assistant-1'",
        "update part set data=json_set(data,'$.metadata',json('{\"note\":\"missing\"}')) where id='p3'",
        "update part set data=json_set(data,'$.time',json('{\"start\":1,\"end\":9}')) where id='p3'; update session_message set data=json_set(data,'$.content[0].time',json('{\"created\":2,\"completed\":4}')) where id='assistant-1'",
    ] {
        let conn = fixture();
        conn.execute_batch(mutation).unwrap();
        assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err(), "{mutation}");
    }
    let conn = fixture();
    conn.execute_batch("update part set data=json_set(data,'$.metadata',json('{}'),'$.synthetic',json('false'),'$.ignored',json('false')) where id='p1';
        update part set data=json_set(data,'$.metadata',json('{\"note\":{\"nested\":\"preserved\"}}'),'$.time',json('{\"start\":2,\"end\":4}')) where id='p3';
        update session_message set data=json_set(data,'$.content[0].state',json('{\"note\":{\"nested\":\"preserved\"}}')) where id='assistant-1';").unwrap();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    conn.execute_batch("update session_message set data=json_set(data,'$.content[0].state.note.nested','different') where id='assistant-1'").unwrap();
    assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err());
}

#[test]
fn native_token_total_can_be_omitted_only_when_reconstructible() {
    let canonical = json!({"input":10,"output":5,"reasoning":3,"cache":{"read":2,"write":1}});
    let mut with_total = canonical.clone();
    with_total["total"] = json!(21);
    let mut cases = vec![(canonical.clone(), true), (with_total.clone(), true)];
    for (pointer, value) in [
        ("/total", json!(22)),
        ("/total", Value::Null),
        ("/cache/read", json!(3)),
        ("/input", json!(-1)),
    ] {
        let mut invalid = with_total.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        cases.push((invalid, false));
    }
    let mut unknown = with_total.clone();
    unknown["future"] = json!(7);
    cases.push((unknown, false));
    let mut unknown_cache = with_total.clone();
    unknown_cache["cache"]["future"] = json!(7);
    cases.push((unknown_cache, false));
    let mut incomplete = with_total.clone();
    incomplete.as_object_mut().unwrap().remove("reasoning");
    cases.push((incomplete, false));
    for (tokens, expected) in cases {
        let conn = fixture();
        part(
            &conn,
            "p6",
            "assistant-1",
            json!({"type":"step-finish","cost":0.25,"reason":"stop","tokens":tokens}),
        );
        conn.execute("update session_message set data=json_set(data,'$.cost',0.25,'$.finish','stop','$.tokens',json(?1)) where id='assistant-1'", [canonical.to_string()]).unwrap();
        assert_eq!(
            OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_ok(),
            expected,
            "{tokens}"
        );
    }
}

#[test]
fn tool_and_reasoning_metadata_must_survive_with_exact_nested_values() {
    for change in [
        "update session_message set data=json_remove(data,'$.content[2].providerState') where id='assistant-1'",
        "update session_message set data=json_set(data,'$.content[2].state.metadata.nested.note','different') where id='assistant-1'",
        "update session_message set data=json_set(data,'$.content[1].state.nested.note','different') where id='assistant-1'",
    ] {
        let conn = fixture();
        conn.execute_batch("update part set data=json_set(data,'$.metadata',json('{\"nested\":{\"note\":\"preserved\"}}')) where id in ('p4','p5');
            update part set data=json_set(data,'$.state.metadata',json('{\"nested\":{\"note\":\"preserved\"}}')) where id='p5';
            update session_message set data=json_set(data,
                '$.content[1].state',json('{\"nested\":{\"note\":\"preserved\"}}'),
                '$.content[2].providerState',json('{\"nested\":{\"note\":\"preserved\"}}'),
                '$.content[2].state.metadata',json('{\"nested\":{\"note\":\"preserved\"}}')) where id='assistant-1';").unwrap();
        assert_eq!(selected(&conn).family, OpenCodeNativeSchemaFamily::SessionMessageSeq);
        conn.execute_batch(change).unwrap();
        assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err(), "{change}");
    }
}

#[test]
fn retained_output_projection_requires_exact_metadata_and_unambiguous_json() {
    for mutation in [
        "update session_message set data=json_set(data,'$.content[2].state.metadata.output','different') where id='assistant-1'",
        "update session_message set data=json_remove(data,'$.content[2].state.metadata.output') where id='assistant-1'",
        "update session_message set data=replace(data,'\"output\":\"preserved\"','\"output\":\"preserved\",\"output\":\"preserved\"') where id='assistant-1'",
    ] {
        let conn = fixture();
        conn.execute_batch("update part set data=json_set(data,'$.state.metadata.output','preserved') where id='p5';
            update session_message set data=json_set(data,'$.content[2].state.metadata.output','preserved') where id='assistant-1';").unwrap();
        assert_eq!(selected(&conn).family, OpenCodeNativeSchemaFamily::SessionMessageSeq);
        conn.execute_batch(mutation).unwrap();
        assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err(), "{mutation}");
    }
}

#[test]
fn overlap_requires_native_lookup_indexes_and_exact_primary_identities() {
    let conn = fixture();
    assert_eq!(
        selected(&conn).family,
        OpenCodeNativeSchemaFamily::SessionMessageSeq
    );
    for mutation in [
        "drop index part_message_id_id_idx",
        "drop index part_message_id_id_idx; create index part_message_id_id_idx on part(message_id collate nocase,id)",
        "drop index part_message_id_id_idx; create index part_message_id_id_idx on part(message_id,id) where id='p1'",
        "alter table session_message rename to old;
         create table session_message (id text collate nocase primary key, session_id text, type text, seq integer, time_created integer, time_updated integer, data text);
         insert into session_message select * from old; drop table old",
        "alter table session_message rename to old;
         create table session_message (id text, session_id text, type text, seq integer, time_created integer, time_updated integer, data text, primary key(id,session_id));
         insert into session_message select * from old; drop table old",
        "alter table session rename to old;
         create table session (id text collate nocase primary key, time_created integer, time_updated integer);
         insert into session select * from old; drop table old",
    ] {
        let conn = fixture();
        conn.execute_batch(mutation).unwrap();
        assert!(OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).is_err(), "{mutation}");
    }
}

#[test]
fn native_binary_identities_keep_case_folded_owners_and_message_references_distinct() {
    for mutation in [
        "update part set session_id='SESSION-1' where id='p1'",
        "update part set message_id='USER-1' where id='p1'",
    ] {
        // Native BINARY keys are the nearest supported positive. Each negative
        // starts clean so an earlier bad owner cannot hide a bad message link.
        let conn = fixture();
        assert_eq!(
            selected(&conn).family,
            OpenCodeNativeSchemaFamily::SessionMessageSeq
        );
        conn.execute_batch(mutation).unwrap();
        let error = OpenCodeNativeSchema::probe(&conn, &OPENCODE_SQLITE_DIALECT).unwrap_err();
        assert!(
            error.to_string().contains("ambiguous populated"),
            "{mutation}: {error}"
        );
    }
}
