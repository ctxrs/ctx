use ctx_history_core::{
    AgentScope, CoreRecord, ProviderNativeSessionRelationship, ScannedSourceCounts,
    SourceAnchorScope,
};
use rusqlite::limits::Limit;

use super::{
    schema::DevinNativeSchema,
    schema_tests::{fixture_connection, fixture_path, mutable_fixture},
    source_backed::{devin_source_key_scoped, scan_devin_snapshot, DevinScanCounts},
};

#[path = "lineage_tests.rs"]
mod lineage_tests;

struct Scanned {
    records: Vec<CoreRecord>,
    counts: DevinScanCounts,
    scanned_counts: ScannedSourceCounts,
    fingerprint: [u8; 32],
    rejections: Vec<ctx_history_capture_runtime::SourceBackedRecordRejectionDraft>,
    omitted_rejections: usize,
}

fn scan(conn: &rusqlite::Connection) -> Scanned {
    let schema = DevinNativeSchema::probe(conn).unwrap();
    let source = devin_source_key_scoped(SourceAnchorScope::Unqualified).unwrap();
    let mut records = Vec::new();
    let selector = fixture_path().to_string_lossy().into_owned();
    let scan = scan_devin_snapshot(conn, &schema, &source, &selector, &mut |record| {
        records.push(record);
        Ok(())
    })
    .unwrap();
    let (rejections, omitted_rejections) = scan.record_rejections.clone().into_parts();
    Scanned {
        records,
        counts: scan.counts,
        scanned_counts: scan.scanned_counts().unwrap(),
        fingerprint: scan.logical_fingerprint,
        rejections,
        omitted_rejections,
    }
}

fn fixture_scan() -> Scanned {
    scan(&fixture_connection())
}

#[test]
fn the_fixture_projects_every_session_with_no_rejected_records() {
    let scanned = fixture_scan();
    assert_eq!(scanned.counts.sessions, 3);
    assert_eq!(scanned.counts.rejected_sessions, 0);
    assert_eq!(scanned.counts.rejected_records, 0);
    assert!(
        scanned.counts.complete_records >= 38,
        "expected at least one record per imported node, got {}",
        scanned.counts.complete_records
    );
    assert_eq!(
        scanned.counts.complete_records as usize,
        scanned.records.len()
    );
    assert_eq!(scanned.counts.rejected_lineages, 0);
    assert_eq!(scanned.counts.rejected_splices, 0);
    assert!(scanned.rejections.is_empty());
    assert_eq!(scanned.omitted_rejections, 0);
}

#[test]
fn compaction_copies_of_the_same_native_message_are_emitted_once() {
    let scanned = fixture_scan();
    let copies = scanned
        .records
        .iter()
        .filter(|record| {
            record.provider_session_id.as_deref() == Some("discovered-sandal")
                && record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains("devinclioracleskill"))
        })
        .count();
    assert_eq!(copies, 1, "the rewritten system-prefix message is one turn");
}

#[test]
fn malformed_off_chain_json_does_not_abort_valid_sessions() {
    let baseline = fixture_scan();
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set chat_message = '{', metadata = '{' \
         where session_id = 'abounding-crest' and node_id = 26",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.records, baseline.records);
    assert_eq!(
        scanned.counts.complete_records,
        baseline.counts.complete_records
    );
}

#[test]
fn malformed_scalar_rows_are_rejected_locally_during_a_mixed_validity_full_scan() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set main_chain_id = 'not-a-node' where id = 'abounding-crest'",
        [],
    )
    .unwrap();
    conn.execute(
        "update message_nodes set parent_node_id = 'not-a-node' \
         where session_id = 'discovered-sandal' and node_id = 24",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) \
             values ('discovered-sandal', 'bad-chain', 'not-a-node', 1789910400);
         insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) \
             values ('discovered-sandal', 'bad-time', 35, 'not-a-time');",
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_sessions, 1);
    assert_eq!(scanned.counts.rejected_records, 1);
    assert_eq!(scanned.counts.rejected_lineages, 2);
    assert!(scanned
        .records
        .iter()
        .all(|record| { record.provider_session_id.as_deref() != Some("abounding-crest") }));
    for healthy_session in ["discovered-sandal", "exclusive-bamboo"] {
        assert!(scanned
            .records
            .iter()
            .any(|record| record.provider_session_id.as_deref() == Some(healthy_session)));
    }
    for detail in [
        "main_chain_id has a non-integer SQLite scalar",
        "node 24 was rejected: parent_node_id has a non-integer SQLite scalar",
        "bad-chain was rejected: chain_node_id has a non-integer SQLite scalar",
        "bad-time was rejected: updated_at has a non-integer SQLite scalar",
    ] {
        assert!(
            scanned
                .rejections
                .iter()
                .any(|rejection| rejection.detail.contains(detail)),
            "missing rejection for {detail}: {:#?}",
            scanned.rejections
        );
    }
    assert_eq!(
        scanned.scanned_counts.complete_records,
        scanned.scanned_counts.retained_records
            + scanned.scanned_counts.rejected_records
            + scanned.scanned_counts.ignored_records
    );
}

#[test]
fn repeated_subagent_lineage_and_splice_diagnostics_are_unique_and_accounted_for() {
    let (_temp, conn) = mutable_fixture();
    conn.execute_batch(
        "insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) \
             values ('discovered-sandal', 'overlap-a', \
                 (select main_chain_id from sessions where id = 'discovered-sandal'), 1789910400); \
         insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) \
             values ('discovered-sandal', 'overlap-b', \
                 (select main_chain_id from sessions where id = 'discovered-sandal'), 1789910400);",
    )
    .unwrap();
    conn.execute_batch(
        "insert into sessions \
             (id, working_directory, backend_type, model, agent_mode, created_at, \
              last_activity_at, main_chain_id, hidden) \
             values ('splice-diagnostics', '/workspace', 'local', 'model', 'agent', 1, 1, 10002, 0); \
         insert into message_nodes \
             (session_id, node_id, parent_node_id, chat_message, created_at, metadata) \
             values ('splice-diagnostics', 10001, null, \
                     '{\"role\":\"assistant\",\"content\":\"first\"}', 1, \
                     '{\"summarized_from\":20001}'), \
                    ('splice-diagnostics', 10002, 10001, \
                     '{\"role\":\"assistant\",\"content\":\"second\"}', 2, \
                     '{\"summarized_from\":20002}');",
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_lineages, 2);
    assert_eq!(scanned.counts.rejected_splices, 2);
    let repeated_rejections = scanned
        .rejections
        .iter()
        .filter(|rejection| {
            rejection
                .detail
                .contains("invalid, ambiguous, or overlapping subagent lineage")
                || rejection
                    .detail
                    .contains("compaction splice whose referenced node is absent")
        })
        .count();
    assert_eq!(
        repeated_rejections + scanned.omitted_rejections,
        (scanned.counts.rejected_lineages + scanned.counts.rejected_splices) as usize
    );
    let unique_keys = scanned
        .rejections
        .iter()
        .map(|rejection| {
            (
                rejection.source.identity().digest(),
                rejection.provider.as_str(),
                rejection.source_selector.as_str(),
                rejection.line_number,
                rejection.payload_type.as_deref(),
                rejection.class.as_str(),
                rejection.detail.as_str(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique_keys.len(),
        scanned.rejections.len(),
        "{:#?}",
        scanned.rejections
    );
}

#[test]
fn malformed_payload_fields_reject_only_the_affected_chain_node() {
    for mutation in [
        "update message_nodes set created_at = 'bad-time' where session_id = 'abounding-crest' and node_id = 28",
        "update message_nodes set chat_message = x'80' where session_id = 'abounding-crest' and node_id = 28",
    ] {
        let (_temp, conn) = mutable_fixture();
        conn.execute(mutation, []).unwrap();

        let scanned = scan(&conn);
        assert_eq!(scanned.counts.rejected_sessions, 0, "{mutation}");
        assert_eq!(scanned.counts.rejected_records, 1, "{mutation}");
        assert!(scanned.records.iter().any(|record| {
            record.provider_session_id.as_deref() == Some("abounding-crest")
                && record.content.normalized_body.as_deref() == Some("devincliassistantoracle")
        }));
        assert!(scanned.records.iter().any(|record| {
            record.provider_session_id.as_deref() == Some("exclusive-bamboo")
        }));
    }
}

#[test]
fn malformed_session_metadata_and_head_ids_are_local_rejections() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set title = x'80' where id = 'abounding-crest'",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) \
         values ('discovered-sandal', x'61', 35, 1789910400)",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_sessions, 0);
    assert_eq!(scanned.counts.rejected_records, 1);
    assert_eq!(scanned.counts.rejected_lineages, 1);
    for session in ["abounding-crest", "exclusive-bamboo", "discovered-sandal"] {
        assert!(scanned
            .records
            .iter()
            .any(|record| record.provider_session_id.as_deref() == Some(session)));
    }
    for detail in [
        "ignored malformed session metadata fields: title",
        "<invalid-agent-id> was rejected: agent_id",
    ] {
        assert!(
            scanned
                .rejections
                .iter()
                .any(|rejection| rejection.detail.contains(detail)),
            "missing {detail}: {:#?}",
            scanned.rejections
        );
    }
}

#[test]
fn malformed_session_ids_are_rejected_without_poisoning_later_rows() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set id = x'80' where id = 'abounding-crest'",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_sessions, 1);
    for session in ["exclusive-bamboo", "discovered-sandal"] {
        assert!(scanned
            .records
            .iter()
            .any(|record| record.provider_session_id.as_deref() == Some(session)));
    }
    assert!(scanned.rejections.iter().any(|rejection| {
        rejection
            .detail
            .contains("id has SQLite storage class blob and 1 bytes")
    }));
}

#[test]
fn an_empty_session_id_is_rejected_without_poisoning_later_rows() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set id = '' where id = 'abounding-crest'",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_sessions, 1);
    for session in ["exclusive-bamboo", "discovered-sandal"] {
        assert!(scanned
            .records
            .iter()
            .any(|record| record.provider_session_id.as_deref() == Some(session)));
    }
    assert!(scanned.rejections.iter().any(|rejection| {
        rejection
            .detail
            .contains("id has SQLite storage class text and 0 bytes")
    }));
}

#[test]
fn session_page_byte_rollover_preserves_every_session() {
    let (_temp, conn) = mutable_fixture();
    let large_title = "t".repeat(5 * 1024 * 1024);
    for session in ["abounding-crest", "exclusive-bamboo"] {
        conn.execute(
            "update sessions set title = ?1 where id = ?2",
            rusqlite::params![&large_title, session],
        )
        .unwrap();
    }

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.sessions, 3);
    assert_eq!(scanned.counts.rejected_sessions, 0);
    for session in ["abounding-crest", "exclusive-bamboo", "discovered-sandal"] {
        assert!(scanned
            .records
            .iter()
            .any(|record| record.provider_session_id.as_deref() == Some(session)));
    }
}

#[test]
fn an_oversized_foreground_agent_id_rejects_only_that_lineage() {
    let baseline = fixture_scan();
    let (_temp, conn) = mutable_fixture();
    let oversized = "x".repeat(super::chain::DEVIN_SUBAGENT_AGENT_ID_BYTES + 1);
    conn.execute(
        "update message_nodes \
         set chat_message = replace(chat_message, 'd8a8ea4c', ?1) \
         where session_id = 'discovered-sandal' and node_id = 37",
        [&oversized],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(
        scanned.counts.rejected_lineages,
        baseline.counts.rejected_lineages + 1
    );
    for session in ["abounding-crest", "exclusive-bamboo", "discovered-sandal"] {
        assert!(scanned
            .records
            .iter()
            .any(|record| record.provider_session_id.as_deref() == Some(session)));
    }
    assert!(!scanned.records.iter().any(|record| {
        record
            .provider_session_id
            .as_deref()
            .is_some_and(|id| id.starts_with("discovered-sandal/subagents/"))
    }));
}

#[test]
fn overlapping_malformed_categories_classify_an_off_chain_row_once() {
    let baseline = fixture_scan();
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set created_at = 'bad-time', \
             metadata = '{\"x\":1,\"x\":2}' \
         where session_id = 'abounding-crest' and node_id = 26",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.records, baseline.records);
    assert_eq!(
        scanned.counts.rejected_records,
        baseline.counts.rejected_records + 1
    );
    assert_eq!(
        scanned.scanned_counts.complete_records,
        scanned.scanned_counts.retained_records
            + scanned.scanned_counts.rejected_records
            + scanned.scanned_counts.ignored_records
    );
}

#[test]
fn oversized_node_and_tool_state_values_are_rejected_under_the_production_limit() {
    let baseline = fixture_scan();
    let oversized = "x".repeat(crate::provider::sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES + 1);

    let (_node_temp, node_conn) = mutable_fixture();
    node_conn
        .execute(
            "update message_nodes set chat_message = ?1 \
             where session_id = 'abounding-crest' and node_id = 26",
            [&oversized],
        )
        .unwrap();
    node_conn
        .set_limit(
            Limit::SQLITE_LIMIT_LENGTH,
            crate::provider::sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES as i32,
        )
        .unwrap();
    let node_scan = scan(&node_conn);
    assert_eq!(node_scan.records, baseline.records);
    assert_eq!(
        node_scan.counts.rejected_records,
        baseline.counts.rejected_records + 1
    );

    let (_tool_temp, tool_conn) = mutable_fixture();
    tool_conn
        .execute(
            "update tool_call_state set tool_call_json = ?1 \
             where session_id = 'abounding-crest' and tool_call_id = 'exec_0'",
            [&oversized],
        )
        .unwrap();
    tool_conn
        .set_limit(
            Limit::SQLITE_LIMIT_LENGTH,
            crate::provider::sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES as i32,
        )
        .unwrap();
    let tool_scan = scan(&tool_conn);
    assert_eq!(
        tool_scan.counts.rejected_records,
        baseline.counts.rejected_records + 1
    );
    assert!(tool_scan.rejections.iter().any(|rejection| {
        rejection
            .detail
            .contains("tool_call_json exceeds the provider value byte bound")
    }));
}

#[test]
fn planning_facts_from_an_unhydratable_node_still_rotate_the_fingerprint() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set created_at = 'bad-time', \
             chat_message = replace(chat_message, 'd8a8ea4c', 'fingerprint-a') \
         where session_id = 'discovered-sandal' and node_id = 37",
        [],
    )
    .unwrap();
    let first = scan(&conn);
    conn.execute(
        "update message_nodes set chat_message = replace(chat_message, 'fingerprint-a', 'fingerprint-b') \
         where session_id = 'discovered-sandal' and node_id = 37",
        [],
    )
    .unwrap();
    let second = scan(&conn);
    assert_ne!(first.fingerprint, second.fingerprint);
    assert!(first.records.iter().any(|record| {
        record
            .provider_session_id
            .as_deref()
            .is_some_and(|id| id.ends_with("/subagents/fingerprint-a"))
    }));
    assert!(second.records.iter().any(|record| {
        record
            .provider_session_id
            .as_deref()
            .is_some_and(|id| id.ends_with("/subagents/fingerprint-b"))
    }));
}

#[test]
fn a_traversed_malformed_parent_rejects_that_session_with_its_row_diagnostic() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set parent_node_id = 'not-a-node' \
         where session_id = 'abounding-crest' and node_id = 30",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_sessions, 1);
    assert_eq!(scanned.counts.rejected_records, 1);
    assert!(scanned
        .records
        .iter()
        .any(|record| { record.provider_session_id.as_deref() == Some("discovered-sandal") }));
    assert!(scanned.rejections.iter().any(|rejection| {
        rejection.detail.contains(
            "chain traversed node 30, whose parent_node_id has a non-integer SQLite scalar",
        )
    }));
}

#[test]
fn duplicate_json_keys_cannot_establish_subagent_or_compaction_relationships() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set chat_message = \
         '{\"message_id\":\"ambiguous-subagent\",\"role\":\"assistant\",\"content\":\"ambiguous\",\"metadata\":{\"extensions\":{\"subagent/chain_node_id\":35,\"subagent/chain_node_id\":35,\"subagent/agent_id\":\"ambiguous\"}}}' \
         where session_id = 'discovered-sandal' and node_id = 37",
        [],
    )
    .unwrap();
    conn.execute(
        "update message_nodes set metadata = '{\"summarized_from\":42,\"summarized_from\":42}' \
         where session_id = 'discovered-sandal' and node_id = 48",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert!(scanned
        .records
        .iter()
        .any(|record| { record.provider_session_id.as_deref() == Some("abounding-crest") }));
    assert!(scanned.records.iter().all(|record| {
        !record
            .provider_session_id
            .as_deref()
            .is_some_and(|id| id.contains("/subagents/ambiguous"))
    }));
    assert!(scanned.records.iter().all(|record| {
        !record
            .content
            .normalized_body
            .as_deref()
            .is_some_and(|body| body.contains("Full conversation history saved"))
    }));
    assert!(scanned.counts.rejected_records >= 2);
    assert!(scanned.rejections.iter().any(|rejection| {
        rejection
            .detail
            .contains("node 48 has ambiguous duplicate metadata JSON keys")
    }));
}

#[test]
fn empty_tool_text_is_enriched_through_the_full_scan() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        concat!(
            "update message_nodes set chat_message = json_set(chat_message, '$.content', '', ",
            "'$.metadata.extensions.\"chisel/terminal_output\".text', '') ",
            "where session_id = 'abounding-crest' and node_id = 28"
        ),
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    let expected_call_id = ctx_history_core::TypedKey::utf8("exec_0").unwrap();
    let result = scanned
        .records
        .iter()
        .find(|record| {
            record.content.activity.as_ref().is_some_and(|activity| {
                activity.provider_call_id.as_ref() == Some(&expected_call_id)
                    && activity.result.is_some()
            })
        })
        .expect("empty native result must survive until ACP enrichment");
    assert!(result
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("devinclitooloracle")));
}

#[test]
fn malformed_optional_tool_state_is_visible_without_aborting_a_mixed_validity_full_scan() {
    let baseline = fixture_scan();
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update tool_call_state set tool_call_json = x'00', tool_call_update_json = '{' \
         where session_id = 'abounding-crest' and tool_call_id = 'exec_0'",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_ne!(scanned.fingerprint, baseline.fingerprint);
    assert_eq!(
        scanned.counts.complete_records,
        baseline.counts.complete_records
    );
    assert_eq!(
        scanned.counts.rejected_records,
        baseline.counts.rejected_records + 2
    );
    assert!(scanned
        .records
        .iter()
        .any(|record| { record.provider_session_id.as_deref() == Some("exclusive-bamboo") }));
    for detail in [
        "node 28 ignored optional tool_call_state enrichment for call exec_0: tool_call_json has a BLOB SQLite scalar",
        "node 28 ignored optional tool_call_state enrichment for call exec_0: tool_call_update_json is not valid JSON",
    ] {
        assert!(
            scanned
                .rejections
                .iter()
                .any(|rejection| rejection.detail.contains(detail)),
            "missing rejection for {detail}: {:#?}",
            scanned.rejections
        );
    }
}

#[test]
fn megabyte_message_ids_are_deduplicated_with_a_fixed_size_key_during_full_scans() {
    let (_temp, conn) = mutable_fixture();
    let message = serde_json::json!({
        "message_id": "m".repeat(1024 * 1024),
        "role": "assistant",
        "content": "megabyte dedup oracle",
    })
    .to_string();
    conn.execute(
        "update message_nodes set chat_message = ?1 \
         where session_id = 'abounding-crest' and node_id = 30",
        [&message],
    )
    .unwrap();
    conn.execute(
        "insert into message_nodes (session_id, node_id, parent_node_id, chat_message, created_at, metadata) \
         values ('abounding-crest', 999, 30, ?1, 1789910400, null)",
        [&message],
    )
    .unwrap();
    conn.execute(
        "update sessions set main_chain_id = 999 where id = 'abounding-crest'",
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(
        scanned
            .records
            .iter()
            .filter(
                |record| record.content.normalized_body.as_deref() == Some("megabyte dedup oracle")
            )
            .count(),
        1,
    );
    assert_eq!(
        std::mem::size_of_val(&super::source_backed::message_pair_digest(
            &message, &message
        )),
        32,
    );
}

#[test]
fn every_record_carries_an_explicit_scope_and_a_provider_session_id() {
    let scanned = fixture_scan();
    for record in &scanned.records {
        assert!(record.agent_scope.is_some(), "{:?}", record.event_id);
        assert!(record.provider_session_id.is_some());
        assert!(record.occurred_at_unix_ms.is_some());
        assert!(record.role.is_some());
        assert!(record
            .content
            .normalized_body
            .as_deref()
            .is_some_and(|body| !body.is_empty()));
    }
}

#[test]
fn the_primary_transcript_claims_no_lineage_and_the_subagent_claims_an_exact_one() {
    let scanned = fixture_scan();

    let primaries = scanned
        .records
        .iter()
        .filter(|record| record.agent_scope == Some(AgentScope::Primary))
        .collect::<Vec<_>>();
    assert!(!primaries.is_empty());
    for record in &primaries {
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
    }

    let subagents = scanned
        .records
        .iter()
        .filter(|record| record.agent_scope == Some(AgentScope::Subagent))
        .collect::<Vec<_>>();
    assert!(!subagents.is_empty(), "the fixture links one subagent");
    let primary_session_id = scanned
        .records
        .iter()
        .find(|record| record.provider_session_id.as_deref() == Some("discovered-sandal"))
        .map(|record| record.session_id)
        .expect("primary session present");
    for record in &subagents {
        assert_eq!(
            record.provider_session_id.as_deref(),
            Some("discovered-sandal/subagents/d8a8ea4c")
        );
        assert_eq!(record.parent_session_id, Some(primary_session_id));
        assert_eq!(record.root_session_id, Some(primary_session_id));
        assert_eq!(
            record.session_relationship,
            Some(ProviderNativeSessionRelationship::Delegated)
        );
        assert_ne!(record.session_id, primary_session_id);
    }
}

#[test]
fn a_durable_background_head_projects_a_child_and_rotates_with_its_evidence() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        concat!(
            "insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) ",
            "values ('discovered-sandal', 'background-sidekick', 30, 1789910400)"
        ),
        [],
    )
    .unwrap();

    let with_head = scan(&conn);
    let primary_session_id = with_head
        .records
        .iter()
        .find(|record| record.provider_session_id.as_deref() == Some("discovered-sandal"))
        .map(|record| record.session_id)
        .expect("primary session present");
    let child = with_head
        .records
        .iter()
        .filter(|record| {
            record.provider_session_id.as_deref()
                == Some("discovered-sandal/subagents/background-sidekick")
        })
        .collect::<Vec<_>>();
    assert!(!child.is_empty(), "durable background child present");
    for record in child {
        assert_eq!(record.agent_scope, Some(AgentScope::Subagent));
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, Some(primary_session_id));
        assert_eq!(
            record.session_relationship,
            Some(ProviderNativeSessionRelationship::Delegated)
        );
    }

    conn.execute(
        concat!(
            "update subagent_heads set updated_at = updated_at + 1 ",
            "where session_id = 'discovered-sandal' and agent_id = 'background-sidekick'"
        ),
        [],
    )
    .unwrap();
    assert_ne!(
        scan(&conn).fingerprint,
        with_head.fingerprint,
        "updated_at is part of durable-head evidence"
    );
}

#[test]
fn no_record_ever_claims_an_event_copy() {
    // Devin records nothing that would prove one, so the channel stays unset.
    for record in fixture_scan().records {
        assert!(record.event_copy.is_none(), "{:?}", record.event_id);
    }
}

#[test]
fn the_oracle_turns_are_projected_with_their_exact_text() {
    let scanned = fixture_scan();
    let bodies = scanned
        .records
        .iter()
        .filter_map(|record| record.content.normalized_body.clone())
        .collect::<Vec<_>>();
    for expected in ["devincliassistantoracle", "devincliedited"] {
        assert!(
            bodies.iter().any(|body| body == expected),
            "missing assistant oracle {expected}"
        );
    }
    for expected in ["devinclitooloracle", "devinclifileoracle"] {
        assert!(
            bodies.iter().any(|body| body.contains(expected)),
            "missing tool oracle {expected}"
        );
    }
}

#[test]
fn a_command_tool_call_records_its_command_and_links_its_result() {
    let scanned = fixture_scan();
    let call = scanned
        .records
        .iter()
        .find(|record| {
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.invocation.as_ref())
                .is_some_and(|invocation| invocation.tool == "exec")
        })
        .expect("the fixture runs one exec tool call");
    let activity = call.content.activity.as_ref().unwrap();
    assert!(activity.provider_call_id.is_some());
    assert_eq!(
        call.content.normalized_body.as_deref(),
        Some("echo devinclitooloracle")
    );
    assert!(activity
        .facts
        .iter()
        .any(|fact| fact.kind == ctx_history_core::LiteralFactKind::Command));

    // The matching result carries the same provider call id.
    let call_id = activity.provider_call_id.clone().unwrap();
    let result = scanned
        .records
        .iter()
        .find(|record| {
            record.content.activity.as_ref().is_some_and(|activity| {
                activity.result.is_some() && activity.provider_call_id.as_ref() == Some(&call_id)
            })
        })
        .expect("the exec call has a terminal result");
    let result_activity = result.content.activity.as_ref().unwrap();
    assert_eq!(
        result_activity.result.as_ref().unwrap().status.as_deref(),
        Some("completed")
    );
}

#[test]
fn a_session_cwd_fact_is_attached_to_every_session() {
    let scanned = fixture_scan();
    assert!(scanned
        .records
        .iter()
        .any(|record| record
            .content
            .activity
            .as_ref()
            .is_some_and(|activity| activity.facts.iter().any(|fact| {
                fact.kind == ctx_history_core::LiteralFactKind::SessionCwd
                    && fact.value == "/tmp/devin-fixture"
            }))));
}

#[test]
fn event_identities_are_unique_and_stable_across_scans() {
    let first = fixture_scan();
    let second = fixture_scan();
    assert_eq!(first.fingerprint, second.fingerprint);

    let ids = first
        .records
        .iter()
        .map(|record| record.event_id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        ids.len(),
        first.records.len(),
        "one node fanning out must not collide on event identity"
    );

    let second_ids = second
        .records
        .iter()
        .map(|record| record.event_id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids, second_ids);
}

#[test]
fn the_fingerprint_notices_content_metadata_and_disposition_changes() {
    let baseline = fixture_scan().fingerprint;

    // Changed payload.
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set chat_message = json_set(chat_message, '$.content', 'mutated') \
         where session_id = 'abounding-crest' and node_id = 30",
        [],
    )
    .unwrap();
    assert_ne!(scan(&conn).fingerprint, baseline, "payload change");

    // Changed session metadata.
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set hidden = 1 where id = 'abounding-crest'",
        [],
    )
    .unwrap();
    assert_ne!(scan(&conn).fingerprint, baseline, "hidden flag change");

    // A session becoming metadata-only stays valid but rotates its evidence.
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set main_chain_id = null where id = 'abounding-crest'",
        [],
    )
    .unwrap();
    let dropped = scan(&conn);
    assert_ne!(dropped.fingerprint, baseline, "disposition change");
    assert_eq!(dropped.counts.rejected_sessions, 0);

    // A dangling non-null anchor is structurally invalid.
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set main_chain_id = 999999 where id = 'abounding-crest'",
        [],
    )
    .unwrap();
    assert_eq!(scan(&conn).counts.rejected_sessions, 1);

    // A vanished tool_call_state row.
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "delete from tool_call_state where session_id = 'abounding-crest'",
        [],
    )
    .unwrap();
    assert_ne!(scan(&conn).fingerprint, baseline, "tool state removal");
}

#[test]
fn malformed_sessions_and_lineages_are_visible_in_scanned_rejections() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update sessions set main_chain_id = 999999 where id = 'abounding-crest'",
        [],
    )
    .unwrap();
    conn.execute(
        concat!(
            "insert into subagent_heads (session_id, agent_id, chain_node_id, updated_at) ",
            "values ('discovered-sandal', 'missing-sidekick', 999999, 1789910400)"
        ),
        [],
    )
    .unwrap();

    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_sessions, 1);
    assert_eq!(scanned.counts.rejected_lineages, 1);
    assert_eq!(scanned.counts.rejected_splices, 0);
    assert_eq!(
        scanned.scanned_counts.rejected_records,
        scanned.counts.rejected_records + 2
    );
    assert_eq!(
        scanned.scanned_counts.complete_records,
        scanned.scanned_counts.retained_records
            + scanned.scanned_counts.rejected_records
            + scanned.scanned_counts.ignored_records
    );
    assert_eq!(scanned.rejections.len(), 2);
    assert_eq!(scanned.omitted_rejections, 0);
    assert!(scanned
        .rejections
        .iter()
        .any(|rejection| rejection.detail.contains("abounding-crest")
            && rejection.detail.contains("main_chain_id")));
    assert!(scanned
        .rejections
        .iter()
        .any(|rejection| rejection.detail.contains("discovered-sandal")
            && rejection.detail.contains("subagent lineage")));
}

#[test]
fn rejected_nodes_and_all_invalid_sources_retain_bounded_diagnostics() {
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set chat_message = '{' \
         where session_id = 'abounding-crest' and node_id = 30",
        [],
    )
    .unwrap();
    let scanned = scan(&conn);
    assert_eq!(scanned.counts.rejected_records, 1);
    assert_eq!(scanned.rejections.len(), 1);
    assert_eq!(scanned.rejections[0].line_number, 30);
    assert!(scanned.rejections[0].detail.contains("abounding-crest"));

    let (_temp, conn) = mutable_fixture();
    conn.execute("update sessions set main_chain_id = 999999", [])
        .unwrap();
    let scanned = scan(&conn);
    assert!(scanned.records.is_empty());
    assert_eq!(scanned.scanned_counts.rejected_records, 3);
    assert_eq!(scanned.rejections.len(), 3);
    assert_eq!(scanned.omitted_rejections, 0);
}

#[test]
fn row_id_is_not_part_of_the_fingerprint() {
    // A vacuum or recopy can renumber row_id without changing history, so the
    // fingerprint must not move when only that column does.
    let baseline = fixture_scan().fingerprint;
    let (_temp, conn) = mutable_fixture();
    conn.execute_batch(
        "create table nodes_copy as select * from message_nodes;
         delete from message_nodes;
         insert into message_nodes (row_id, session_id, node_id, parent_node_id, chat_message, created_at, metadata)
             select row_id + 100000, session_id, node_id, parent_node_id, chat_message, created_at, metadata
             from nodes_copy;
         drop table nodes_copy;",
    )
    .unwrap();
    assert_eq!(scan(&conn).fingerprint, baseline);
}

#[test]
fn an_off_chain_node_does_not_move_the_fingerprint() {
    // Off-chain nodes are counted, not fingerprinted, so editing one must not
    // rotate the digest; the ignored count is what records it.
    let baseline = fixture_scan();
    let (_temp, conn) = mutable_fixture();
    conn.execute(
        "update message_nodes set chat_message = json_set(chat_message, '$.content', 'mutated') \
         where session_id = 'abounding-crest' and node_id = 26",
        [],
    )
    .unwrap();
    let mutated = scan(&conn);
    assert_eq!(mutated.fingerprint, baseline.fingerprint);
    assert_eq!(mutated.counts.ignored_nodes, baseline.counts.ignored_nodes);
}

/// Drives the full snapshot lifecycle the route uses: retain the parent
/// authority, open a pinned snapshot, project it, revalidate, and finish.
fn scan_through_a_pinned_snapshot(
    database_path: &std::path::Path,
    data_root: &std::path::Path,
) -> Result<(usize, [u8; 32]), super::source_backed::DevinSourceBackedError> {
    use super::database::DevinSqliteDatabase;

    let database = DevinSqliteDatabase::open(data_root, database_path)?;
    let opening = database.evidence().clone();
    let source = devin_source_key_scoped(SourceAnchorScope::Unqualified)?;
    let mut emitted = 0_usize;
    let result = (|| {
        let connection = database.connection()?;
        let schema = DevinNativeSchema::probe(connection)?;
        let selector = database_path.to_string_lossy();
        let scan = scan_devin_snapshot(connection, &schema, &source, &selector, &mut |_| {
            emitted += 1;
            Ok(())
        })?;
        // A certification must be derivable, since that is what the route
        // publishes as the source's terminal observation.
        let certificate = scan.certify(source.clone())?;
        assert_eq!(*certificate.content_digest(), scan.logical_fingerprint);
        assert_eq!(certificate.counts().retained_records, emitted as u64);
        // The runtime rejects a terminal whose indexed count disagrees with the
        // Core records the adapter forwarded.
        assert_eq!(certificate.counts().indexed_documents, emitted as u64);
        Ok::<[u8; 32], super::source_backed::DevinSourceBackedError>(scan.logical_fingerprint)
    })();
    let fingerprint = result?;
    database.revalidate()?;
    let closing = database.finish()?;
    assert_eq!(
        closing, opening,
        "the snapshot must not move under the scan"
    );
    Ok((emitted, fingerprint))
}

#[test]
fn the_route_lifecycle_projects_the_fixture_from_a_pinned_snapshot() {
    let data_root = ctx_history_source_sqlite::test_support::tempdir().unwrap();
    let staged = ctx_history_source_sqlite::test_support::tempdir().unwrap();
    let database_path = staged.path().join("sessions.db");
    std::fs::copy(super::schema_tests::fixture_path(), &database_path).unwrap();

    let (emitted, fingerprint) =
        scan_through_a_pinned_snapshot(&database_path, data_root.path()).unwrap();
    assert_eq!(emitted, fixture_scan().records.len());
    assert_eq!(fingerprint, fixture_scan().fingerprint);

    // Reading the same untouched database again must agree, which is what lets
    // a refresh publish a no-op generation.
    let (again, repeated) =
        scan_through_a_pinned_snapshot(&database_path, data_root.path()).unwrap();
    assert_eq!(again, emitted);
    assert_eq!(repeated, fingerprint);

    // No write-ahead or journal sidecar may be left behind by a read-only
    // import of a database nobody else has open.
    for sidecar in ["sessions.db-wal", "sessions.db-journal", "sessions.db-shm"] {
        assert!(
            !staged.path().join(sidecar).exists(),
            "import created {sidecar}"
        );
    }
}

#[test]
fn a_non_sqlite_file_at_the_route_path_is_refused() {
    use super::database::require_devin_sqlite_format;
    use crate::DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT;

    let staged = ctx_history_source_sqlite::test_support::tempdir().unwrap();
    let path = staged.path().join("sessions.db");
    std::fs::write(&path, b"this is not a database").unwrap();
    assert!(matches!(
        require_devin_sqlite_format(&path, DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT),
        Err(super::source_backed::DevinSourceBackedError::UnsupportedFormat(_))
    ));

    // The real fixture passes the same guard. It is copied first because the
    // guard refuses symlinks and Bazel delivers fixtures through a runfiles
    // tree of symlinks; a copy is the regular file production would see.
    let copied = staged.path().join("copied.db");
    std::fs::copy(super::schema_tests::fixture_path(), &copied).unwrap();
    require_devin_sqlite_format(&copied, DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT)
        .expect("the committed fixture must satisfy the format guard");
}
