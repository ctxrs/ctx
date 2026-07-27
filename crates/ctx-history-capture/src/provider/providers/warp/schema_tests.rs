use super::*;

fn create_schema(conn: &Connection) {
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
         );
         create unique index warp_agent_tasks_task_id
             on agent_tasks(task_id collate binary);
         create table ai_queries (
             exchange_id text not null,
             conversation_id text not null
         );",
    )
    .unwrap();
}

#[test]
fn schema_capability_owns_the_required_task_keyset_index() {
    let conn = Connection::open_in_memory().unwrap();
    create_schema(&conn);

    let schema = WarpSqliteSchema::detect(&conn).unwrap();
    assert_eq!(schema.task_keyset_index, "warp_agent_tasks_task_id");
    assert_eq!(schema.capability_digest.len(), 64);

    conn.execute_batch("drop index warp_agent_tasks_task_id;")
        .unwrap();
    let Err(error) = WarpSqliteSchema::detect(&conn) else {
        panic!("Warp schema detection accepted a missing task keyset index");
    };
    assert!(error
        .to_string()
        .contains("requires a non-partial ascending UNIQUE BINARY index"));
}

#[test]
fn capability_digest_tracks_optional_task_and_ai_query_schema() {
    let conn = Connection::open_in_memory().unwrap();
    create_schema(&conn);
    let baseline = WarpSqliteSchema::detect(&conn).unwrap().capability_digest;

    conn.execute_batch(
        "alter table agent_tasks
             add column task_schema_version integer not null default 1;
         create index idx_agent_tasks_conversation
             on agent_tasks(conversation_id, task_id);",
    )
    .unwrap();
    let task_change = WarpSqliteSchema::detect(&conn).unwrap().capability_digest;
    assert_ne!(task_change, baseline);

    conn.execute_batch("alter table ai_queries add column output_status text;")
        .unwrap();
    let query_change = WarpSqliteSchema::detect(&conn).unwrap().capability_digest;
    assert_ne!(query_change, task_change);
}

#[test]
fn capability_authority_uses_versioned_u64_little_endian_text_lengths() {
    assert_eq!(
        capability_text_authority_bytes("warp").unwrap(),
        [4_u64.to_le_bytes().as_slice(), b"warp"].concat()
    );

    let conn = Connection::open_in_memory().unwrap();
    create_schema(&conn);
    assert_eq!(
        WarpSqliteSchema::detect(&conn).unwrap().capability_digest,
        "d6f273c7c1015a52036495c62439abdc39aa846148ff913d6ccf8e19a16d4db7"
    );
}
