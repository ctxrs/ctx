use std::{fs, path::Path};

use chrono::DateTime;
use ctx_history_core::{AgentScope, EventRole, EventType, ProviderNativeSessionRelationship};
use serde_json::{json, Value};

use super::*;
use crate::provider::providers::auggie::{
    auggie_raw_lineage_authority, AuggieLineageClaim, AuggieSessionData,
};

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("AUGGIE_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("then_some(parsed.text)"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
    assert_eq!(
        AUGGIE_PARSER_REVISION,
        "auggie-nativepath-json-v5-agent-scope-raw-lineage"
    );
}

fn write_inventory_session(path: &Path, session_id: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "sessionId": session_id,
            "created": "2026-07-04T20:00:00.000Z",
            "modified": "2026-07-04T20:00:00.000Z",
            "chatHistory": []
        }))
        .unwrap(),
    )
    .unwrap();
}

fn discovered_leaf_count(path: &Path) -> usize {
    let inventory =
        discover_auggie_source_backed_unfenced(&AuggieSourceBackedRoot::explicit(path)).unwrap();
    assert_eq!(
        inventory.status,
        AuggieSourceBackedInventoryStatus::Complete
    );
    inventory.into_complete_tree().unwrap().leaves.len()
}

#[test]
fn adapter_inventory_accepts_both_flat_directory_selections() {
    let direct = tempfile::tempdir().unwrap();
    write_inventory_session(&direct.path().join("direct.json"), "direct-session");
    assert_eq!(discovered_leaf_count(direct.path()), 1);

    let parent = tempfile::tempdir().unwrap();
    write_inventory_session(&parent.path().join("sessions/child.json"), "sessions-child");
    assert_eq!(discovered_leaf_count(parent.path()), 1);
}

#[test]
fn adapter_inventory_ignores_nested_decoys_and_prefers_a_direct_sessions_child() {
    let nested = tempfile::tempdir().unwrap();
    write_inventory_session(&nested.path().join("nested/decoy.json"), "nested-decoy");
    assert_eq!(discovered_leaf_count(nested.path()), 0);

    let shadowed = tempfile::tempdir().unwrap();
    write_inventory_session(&shadowed.path().join("ignored.json"), "shadowed-direct");
    fs::create_dir(shadowed.path().join("sessions")).unwrap();
    assert_eq!(discovered_leaf_count(shadowed.path()), 0);
}

fn scope_record_with_claims(
    parent_session_claim: AuggieLineageClaim,
    root_session_claim: AuggieLineageClaim,
) -> CoreRecord {
    let provider_session_id = "auggie-scope-session";
    let source = auggie_source_key(provider_session_id).unwrap();
    let session_id = auggie_session_id(&source, provider_session_id).unwrap();
    let session = ParsedAuggieSession {
        provider_session_id: provider_session_id.to_owned(),
        parent_session_claim,
        root_session_claim,
        cwd: None,
    };
    let event = ParsedAuggieEvent {
        provider_event_index: 0,
        provider_event_hash: "auggie-scope-event-hash".to_owned(),
        event_type: EventType::Message,
        role: EventRole::User,
        occurred_at: DateTime::UNIX_EPOCH,
        text: "Auggie scope fixture".to_owned(),
        chat_index: 0,
        message_kind: "request",
        native_event_id: Some("auggie-scope-event".to_owned()),
    };
    auggie_core_record(
        &source,
        session_id,
        SourceAnchorScope::Unqualified,
        &session,
        [7; 32],
        event,
    )
    .unwrap()
}

#[test]
fn root_scope_distinguishes_native_sessions_and_unqualified_is_unchanged() {
    let native_session_id = "same-native-session";
    let legacy = auggie_source_key(native_session_id).unwrap();
    let unqualified =
        auggie_source_key_scoped(native_session_id, SourceAnchorScope::Unqualified).unwrap();
    let first =
        auggie_source_key_scoped(native_session_id, SourceAnchorScope::Lineage([1; 32])).unwrap();
    let second =
        auggie_source_key_scoped(native_session_id, SourceAnchorScope::Lineage([2; 32])).unwrap();

    assert!(legacy.exact_descriptor_eq(&unqualified));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        auggie_session_id(&first, native_session_id).unwrap(),
        auggie_session_id(&second, native_session_id).unwrap()
    );
    assert_ne!(
        related_auggie_session_id("same-parent", SourceAnchorScope::Lineage([1; 32])).unwrap(),
        related_auggie_session_id("same-parent", SourceAnchorScope::Lineage([2; 32])).unwrap()
    );
}

#[test]
fn root_scope_partitions_document_replay_without_changing_unqualified_fingerprints() {
    let fingerprint = [7; 32];
    let domain = b"ctx.auggie-document-root-scoped-test-v1\0";

    assert_eq!(
        scope_auggie_document_fingerprint(fingerprint, SourceAnchorScope::Unqualified, domain,),
        fingerprint
    );
    assert_ne!(
        scope_auggie_document_fingerprint(fingerprint, SourceAnchorScope::Lineage([1; 32]), domain,),
        scope_auggie_document_fingerprint(fingerprint, SourceAnchorScope::Lineage([2; 32]), domain,)
    );
}

fn lineage_claim(value: Option<&str>) -> AuggieLineageClaim {
    value.map_or(AuggieLineageClaim::Absent, |value| {
        AuggieLineageClaim::Exact(value.to_owned())
    })
}

fn scope_record(parent: Option<&str>, root: Option<&str>) -> CoreRecord {
    scope_record_with_claims(lineage_claim(parent), lineage_claim(root))
}

fn scope_record_from_session_json(session: &Value) -> CoreRecord {
    let context = ProviderAdapterContext {
        imported_at: DateTime::UNIX_EPOCH,
        ..ProviderAdapterContext::default()
    };
    let parsed =
        AuggieSessionData::parse_with_lineage_authority(session, &context, Default::default())
            .unwrap();
    scope_record_with_claims(
        parsed.parent_session_claim.clone(),
        parsed.root_session_claim.clone(),
    )
}

fn scope_record_from_session_raw(raw: &str) -> CoreRecord {
    let session = serde_json::from_str::<Value>(raw).unwrap();
    let context = ProviderAdapterContext {
        imported_at: DateTime::UNIX_EPOCH,
        ..ProviderAdapterContext::default()
    };
    let parsed = AuggieSessionData::parse_with_lineage_authority(
        &session,
        &context,
        auggie_raw_lineage_authority(raw.as_bytes()).unwrap(),
    )
    .unwrap();
    scope_record_with_claims(parsed.parent_session_claim, parsed.root_session_claim)
}

#[test]
fn absent_or_self_root_lineage_is_primary_without_edges() {
    for root in [None, Some("auggie-scope-session")] {
        let record = scope_record(None, root);
        assert_eq!(record.agent_scope, Some(AgentScope::Primary));
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
    }
}

#[test]
fn durable_parent_lineage_is_subagent_with_native_edges() {
    for root in [None, Some("auggie-root")] {
        let record = scope_record(Some("auggie-parent"), root);
        assert_eq!(record.agent_scope, Some(AgentScope::Subagent));
        assert_eq!(
            record.parent_session_id,
            Some(
                related_auggie_session_id("auggie-parent", SourceAnchorScope::Unqualified,)
                    .unwrap(),
            )
        );
        assert_eq!(
            record.root_session_id,
            root.map(|native_id| {
                related_auggie_session_id(native_id, SourceAnchorScope::Unqualified).unwrap()
            })
        );
        assert_eq!(
            record.session_relationship,
            Some(ProviderNativeSessionRelationship::Delegated)
        );
    }
}

#[test]
fn contradictory_or_insufficient_lineage_remains_unknown_without_edges() {
    for (parent, root) in [
        (None, Some("foreign-root")),
        (Some("auggie-scope-session"), None),
        (Some("auggie-parent"), Some("auggie-scope-session")),
    ] {
        let record = scope_record(parent, root);
        assert_eq!(record.agent_scope, None);
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
    }
}

#[test]
fn malformed_or_conflicting_lineage_aliases_remain_unknown_without_edges() {
    for session in [
        json!({
            "sessionId": "auggie-scope-session",
            "chatHistory": [],
            "parentSessionId": 7
        }),
        json!({
            "sessionId": "auggie-scope-session",
            "chatHistory": [],
            "parentConversationId": "auggie-parent",
            "parent_session_id": "conflicting-parent"
        }),
        json!({
            "sessionId": "auggie-scope-session",
            "chatHistory": [],
            "rootSessionId": null
        }),
        json!({
            "sessionId": "auggie-scope-session",
            "chatHistory": [],
            "rootConversationId": "auggie-root",
            "root_session_id": "conflicting-root"
        }),
    ] {
        let record = scope_record_from_session_json(&session);
        assert_eq!(record.agent_scope, None);
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
    }
}

#[test]
fn duplicate_lineage_keys_remain_unknown_without_edges() {
    for raw in [
        r#"{
            "sessionId": "auggie-scope-session",
            "chatHistory": [],
            "parentSessionId": "auggie-parent",
            "parentSessionId": "conflicting-parent"
        }"#,
        r#"{
            "sessionId": "auggie-scope-session",
            "chatHistory": [],
            "rootSessionId": "auggie-scope-session",
            "rootSessionId": "conflicting-root"
        }"#,
    ] {
        let record = scope_record_from_session_raw(raw);
        assert_eq!(record.agent_scope, None);
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
        assert_eq!(record.session_relationship, None);
    }

    let unrelated_duplicate = scope_record_from_session_raw(
        r#"{
            "sessionId": "auggie-scope-session",
            "chatHistory": [],
            "title": "first",
            "title": "second"
        }"#,
    );
    assert_eq!(unrelated_duplicate.agent_scope, Some(AgentScope::Primary));
}
