use rusqlite::{params, Connection};

use super::stream::{read_session_facts_with_test_limit, SESSION_FACTS_SQL};

fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table message_nodes (
             session_id,
             node_id,
             parent_node_id,
             chat_message,
             created_at,
             metadata
         );
         create unique index message_nodes_session_node on message_nodes (session_id, node_id);",
    )
    .unwrap();
    conn
}

fn insert(
    conn: &Connection,
    node_id: impl rusqlite::ToSql,
    parent_node_id: impl rusqlite::ToSql,
    chat_message: impl rusqlite::ToSql,
    created_at: impl rusqlite::ToSql,
    metadata: impl rusqlite::ToSql,
) {
    conn.execute(
        "insert into message_nodes (session_id, node_id, parent_node_id, chat_message, created_at, metadata)
         values ('session', ?1, ?2, ?3, ?4, ?5)",
        params![node_id, parent_node_id, chat_message, created_at, metadata],
    )
    .unwrap();
}

#[test]
fn planning_uses_the_native_index_and_a_linear_duplicate_key_check() {
    assert!(!SESSION_FACTS_SQL.contains("json_tree"));
    let conn = connection();
    insert(
        &conn,
        1_i64,
        Option::<i64>::None,
        r#"{"metadata":{"extensions":{"subagent/agent_id":"first","subagent/agent_id":"second"}}}"#,
        1_i64,
        r#"{"nested":{"key":1,"key":2}}"#,
    );

    let facts = read_session_facts_with_test_limit(&conn, "session", 8).unwrap();
    assert!(facts.facts.contains_key(&1));
    assert_eq!(facts.facts[&1].summarized_from, None);
    assert_eq!(facts.facts[&1].subagent_agent_id, None);
    assert_eq!(facts.ambiguous_json_nodes.len(), 1);
    assert!(facts.ambiguous_json_nodes[0].metadata);
    assert!(facts.ambiguous_json_nodes[0].chat_message);

    let plan = conn
        .prepare(&format!("explain query plan {SESSION_FACTS_SQL}"))
        .unwrap()
        .query_map(
            params![
                "session",
                9_i64,
                super::stream::DEVIN_HYDRATION_SINGLETON_MAX_BYTES as i64,
                crate::provider::sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES as i64,
                super::chain::DEVIN_SUBAGENT_AGENT_ID_BYTES as i64,
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter().any(|detail| detail.contains("USING INDEX")),
        "{plan:?}"
    );
}

#[test]
fn oversized_json_integers_are_not_decoded_as_i64() {
    let conn = connection();
    insert(
        &conn,
        1_i64,
        Option::<i64>::None,
        r#"{"metadata":{"extensions":{"subagent/chain_node_id":9223372036854775808,"subagent/agent_id":"agent"}}}"#,
        1_i64,
        r#"{"summarized_from":9223372036854775808}"#,
    );

    let facts = read_session_facts_with_test_limit(&conn, "session", 8).unwrap();
    let fact = &facts.facts[&1];
    assert_eq!(fact.summarized_from, None);
    assert_eq!(fact.subagent_chain_node_id, None);
    assert_eq!(fact.subagent_agent_id.as_deref(), Some("agent"));
}

#[test]
fn malformed_affinities_are_row_local_rejections() {
    let conn = connection();
    insert(&conn, 1_i64, "parent", vec![1_u8], "created", vec![2_u8]);
    insert(
        &conn,
        vec![3_u8],
        Option::<i64>::None,
        "{}",
        1_i64,
        Option::<String>::None,
    );

    let facts = read_session_facts_with_test_limit(&conn, "session", 8).unwrap();
    assert!(facts.facts.is_empty());
    assert_eq!(facts.total_rows, 2);
    assert_eq!(facts.malformed_nodes.len(), 2);
    assert_eq!(facts.malformed_nodes[0].node_id, Some(1));
    assert!(facts.malformed_nodes[0].malformed_parent_node_id);
    assert!(facts.malformed_nodes[0].malformed_chat_message);
    assert!(facts.malformed_nodes[0].malformed_created_at);
    assert!(facts.malformed_nodes[0].malformed_metadata);
    assert_eq!(facts.malformed_nodes[1].node_id, None);
    assert!(facts.malformed_nodes[1].malformed_node_id);
    assert_eq!(facts.malformed_parent_nodes[0].node_id, 1);
}

#[test]
fn overflow_counts_malformed_rows_without_retaining_the_sentinel() {
    let conn = connection();
    insert(
        &conn,
        0_i64,
        Option::<i64>::None,
        "{}",
        1_i64,
        Option::<String>::None,
    );
    insert(&conn, 1_i64, "parent", "{}", 1_i64, Option::<String>::None);
    insert(
        &conn,
        2_i64,
        Option::<i64>::None,
        "{}",
        1_i64,
        Option::<String>::None,
    );
    for node_id in [3_i64, 4_i64] {
        insert(
            &conn,
            node_id,
            Option::<i64>::None,
            "{}",
            1_i64,
            Option::<String>::None,
        );
    }

    let facts = read_session_facts_with_test_limit(&conn, "session", 2).unwrap();
    assert_eq!(facts.total_rows, 5);
    assert!(facts.overflowed);
    assert_eq!(facts.facts.keys().copied().collect::<Vec<_>>(), vec![0]);
    assert_eq!(facts.malformed_parent_nodes[0].node_id, 1);
    assert!(!facts.facts.contains_key(&2));
}

#[test]
fn aggregate_foreground_agent_ids_over_the_text_budget_reject_locally() {
    let conn = connection();
    let suffix = "x".repeat(super::chain::DEVIN_SUBAGENT_AGENT_ID_BYTES - 8);
    for node_id in 0_i64..513 {
        let agent_id = format!("{node_id:08}{suffix}");
        let chat_message = format!(
            "{{\"metadata\":{{\"extensions\":{{\"subagent/chain_node_id\":{node_id},\"subagent/agent_id\":\"{agent_id}\"}}}}}}"
        );
        insert(
            &conn,
            node_id,
            Option::<i64>::None,
            chat_message,
            1_i64,
            Option::<String>::None,
        );
    }

    let facts = read_session_facts_with_test_limit(&conn, "session", 1024).unwrap();
    assert_eq!(facts.facts.len(), 513);
    assert_eq!(
        facts
            .facts
            .values()
            .filter(|fact| fact.subagent_agent_id.as_deref() == Some(""))
            .count(),
        1
    );
}

#[test]
fn aggregate_durable_head_ids_over_the_text_budget_reject_locally() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table subagent_heads (
             session_id text not null,
             agent_id text not null,
             chain_node_id integer not null,
             updated_at integer not null,
             primary key (session_id, agent_id)
         );",
    )
    .unwrap();
    let suffix = "x".repeat(super::chain::DEVIN_SUBAGENT_AGENT_ID_BYTES - 8);
    let transaction = conn.transaction().unwrap();
    for node_id in 0_i64..513 {
        let agent_id = format!("{node_id:08}{suffix}");
        transaction
            .execute(
                "insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) \
                 values ('session', ?1, ?2, 1)",
                params![agent_id, node_id],
            )
            .unwrap();
    }
    transaction.commit().unwrap();

    let heads =
        super::stream::read_subagent_heads_with_row_shape_rejections(&conn, "session").unwrap();
    assert_eq!(heads.heads.len(), 512);
    assert_eq!(heads.malformed_heads.len(), 1);
    assert!(heads.malformed_heads[0].exceeded_bound);
}
