//! Structural supplements to the native fixture; these are not provider captures.

use ctx_history_core::{AgentScope, CoreRecord, ProviderNativeSessionRelationship};
use rusqlite::{params, Connection};
use serde_json::json;

use super::{mutable_fixture, scan, Scanned};

fn session(conn: &Connection) {
    conn.execute_batch(
        "insert into sessions \
         (id, working_directory, backend_type, model, agent_mode, created_at, \
          last_activity_at, main_chain_id, hidden) \
         values ('lineage-oracle', '/workspace', 'local', 'model', 'agent', 1, 1, 2, 0);",
    )
    .unwrap();
}

fn node(
    conn: &Connection,
    id: i64,
    parent: Option<i64>,
    link: Option<(&str, i64)>,
    summary: Option<i64>,
) {
    let mut message = json!({"role": "assistant", "content": format!("lineage node {id}")});
    if let Some((agent, tip)) = link {
        message["metadata"] = json!({"extensions": {
            "subagent/agent_id": agent, "subagent/chain_node_id": tip
        }});
    }
    conn.execute(
        "insert into message_nodes \
         (session_id, node_id, parent_node_id, chat_message, created_at, metadata) \
         values ('lineage-oracle', ?1, ?2, ?3, 1, ?4)",
        params![
            id,
            parent,
            message.to_string(),
            summary.map(|tip| json!({"summarized_from": tip}).to_string())
        ],
    )
    .unwrap();
}

fn record(scanned: &Scanned, id: i64) -> &CoreRecord {
    let text = format!("lineage node {id}");
    let matches = scanned
        .records
        .iter()
        .filter(|record| record.content.meaningful_text() == text)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "node {id} must appear exactly once");
    matches[0]
}

#[test]
fn nested_foreground_history_survives_parent_and_child_compaction() {
    let (_temp, conn) = mutable_fixture();
    session(&conn);
    node(&conn, 0, None, None, None);
    node(&conn, 1, Some(0), Some(("parent", 12)), None);
    node(&conn, 2, Some(1), None, None);
    node(&conn, 10, None, None, None);
    node(&conn, 11, Some(10), None, None);
    node(&conn, 12, Some(11), Some(("child", 22)), None);
    node(&conn, 20, None, None, None);
    node(&conn, 21, Some(20), None, None);
    node(&conn, 22, Some(21), None, None);
    let before = scan(&conn);
    let root_id = record(&before, 0).session_id;
    let parent_id = record(&before, 12).session_id;
    let child = record(&before, 22);
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(root_id));
    assert_eq!(child.agent_scope, Some(AgentScope::Subagent));
    assert_eq!(
        child.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );

    node(&conn, 30, Some(10), None, Some(11));
    node(&conn, 31, Some(30), Some(("child", 41)), None);
    node(&conn, 40, Some(20), None, Some(21));
    node(&conn, 41, Some(40), None, None);
    conn.execute_batch(
        "update message_nodes set chat_message = \
         '{\"role\":\"assistant\",\"content\":\"lineage node 2\",\"metadata\":{\"extensions\":{\"subagent/agent_id\":\"parent\",\"subagent/chain_node_id\":31}}}' \
         where session_id = 'lineage-oracle' and node_id = 2; \
         insert into subagent_heads values ('lineage-oracle', 'parent', 31, 2); \
         insert into subagent_heads values ('lineage-oracle', 'child', 41, 2);",
    ).unwrap();
    let after = scan(&conn);
    assert!(after.rejections.is_empty());
    assert_eq!(record(&after, 22).event_id, child.event_id);
    for id in [20, 21, 22, 40, 41] {
        let item = record(&after, id);
        assert_eq!(item.session_id, child.session_id);
        assert_eq!(item.parent_session_id, Some(parent_id));
        assert_eq!(item.root_session_id, Some(root_id));
    }
    assert_eq!(record(&after, 31).session_id, parent_id);
    assert_eq!(record(&after, 31).parent_session_id, Some(root_id));
    assert_eq!(scan(&conn).records, after.records);
    assert_eq!(scan(&conn).fingerprint, after.fingerprint);
}

#[test]
fn durable_head_order_does_not_override_a_nested_foreground_parent() {
    let (_temp, conn) = mutable_fixture();
    session(&conn);
    node(&conn, 0, None, None, None);
    node(&conn, 2, Some(0), None, None);
    node(&conn, 10, None, Some(("a-child", 20)), None);
    node(&conn, 20, None, None, None);
    conn.execute_batch(
        "insert into subagent_heads values ('lineage-oracle', 'a-child', 20, 1); \
         insert into subagent_heads values ('lineage-oracle', 'z-parent', 10, 1);",
    )
    .unwrap();
    let scanned = scan(&conn);
    let root_id = record(&scanned, 0).session_id;
    let parent = record(&scanned, 10);
    let child = record(&scanned, 20);
    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.root_session_id, Some(root_id));
    // A session-scoped head identifies the root, not an immediate parent.
    assert_eq!(parent.parent_session_id, None);
    assert_eq!(parent.root_session_id, Some(root_id));
}

#[test]
fn multiple_callers_preserve_the_child_history_without_choosing_a_parent() {
    let (_temp, conn) = mutable_fixture();
    session(&conn);
    node(&conn, 0, None, Some(("parent", 10)), None);
    node(&conn, 2, Some(0), Some(("child", 21)), None);
    node(&conn, 10, None, Some(("child", 20)), None);
    node(&conn, 20, None, None, None);
    node(&conn, 21, Some(20), None, None);
    let scanned = scan(&conn);
    assert!(scanned.rejections.is_empty());
    let root = record(&scanned, 0).session_id;
    for id in [20, 21] {
        let child = record(&scanned, id);
        assert_eq!(child.parent_session_id, None);
        assert_eq!(child.root_session_id, Some(root));
    }
}
