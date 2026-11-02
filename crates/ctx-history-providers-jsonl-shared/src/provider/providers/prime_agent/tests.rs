use super::*;
use serde_json::json;

fn header(extra: Value, path: &str) -> Result<Option<Binding>> {
    let mut value = json!({"type":"session", "version":3, "id":"child", "cwd":"/work/project",
        "timestamp":"2026-10-01T12:00:00Z", "rlmDepth":0});
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let bytes = serde_json::to_vec(&value).unwrap();
    parse_header(
        JsonlRecordRef::for_test(&bytes, 0),
        Path::new(path),
        SourceAnchorScope::Unqualified,
    )
}

#[test]
fn v3_admission_rejects_other_versions_and_unsafe_ids() {
    assert!(header(json!({}), "/sessions/child.jsonl")
        .unwrap()
        .is_some());
    for extra in [
        json!({"version":2}),
        json!({"version":4}),
        json!({"id":"../elsewhere"}),
        json!({"id":""}),
        json!({"timestamp":"invalid"}),
        json!({"cwd":null}),
    ] {
        assert!(header(extra, "/sessions/child.jsonl").is_err());
    }
}

#[test]
fn native_forks_and_rlm_delegation_do_not_require_parent_files() {
    let fork = header(
        json!({"parentSession":"/removed/parent.jsonl"}),
        "/sessions/child.jsonl",
    )
    .unwrap()
    .unwrap();
    let child = header(
        json!({"parentSession":"/removed/parent.jsonl", "rlmDepth":1}),
        "/agent/session-artifacts/parent/sub-example/child.jsonl",
    )
    .unwrap()
    .unwrap();
    assert_eq!(fork.parent, child.parent);
    assert_eq!(
        fork.relationship,
        Some(ProviderNativeSessionRelationship::Forked)
    );
    assert_eq!(
        child.relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(child.agent_scope, Some(AgentScope::Subagent));
    let forked_child = header(
        json!({"parentSession":"/removed/other-child.jsonl", "rlmDepth":1}),
        "/agent/session-artifacts/parent/sub-example/child.jsonl",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        forked_child.relationship,
        Some(ProviderNativeSessionRelationship::Forked)
    );
    assert_ne!(forked_child.parent, child.parent);
}

#[test]
fn source_and_parent_identity_respect_configured_root_lineage() {
    assert_ne!(
        source_key("session", SourceAnchorScope::Lineage([1; 32])).unwrap(),
        source_key("session", SourceAnchorScope::Lineage([2; 32])).unwrap()
    );
    assert_ne!(
        session_identity("parent", SourceAnchorScope::Lineage([1; 32])).unwrap(),
        session_identity("parent", SourceAnchorScope::Lineage([2; 32])).unwrap()
    );
}
