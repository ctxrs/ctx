use super::*;

#[test]
fn schema_capability_owns_the_required_task_keyset_index() {
    let conn = Connection::open_in_memory().unwrap();
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
             on agent_tasks(task_id collate binary);",
    )
    .unwrap();

    let schema = WarpSqliteSchema::detect(&conn).unwrap();
    assert_eq!(schema.task_keyset_index, "warp_agent_tasks_task_id");

    conn.execute_batch("drop index warp_agent_tasks_task_id;")
        .unwrap();
    let Err(error) = WarpSqliteSchema::detect(&conn) else {
        panic!("Warp schema detection accepted a missing task keyset index");
    };
    assert!(error
        .to_string()
        .contains("requires a non-partial ascending UNIQUE BINARY index"));
}
