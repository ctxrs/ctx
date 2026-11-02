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
        json!({"id":" leading"}),
        json!({"id":"NUL"}),
        json!({"id":"trailing."}),
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

#[test]
fn identity_probe_rejects_header_rewrites_and_object_replacement() {
    for (field, replacement, replace_object) in [
        ("id", "bravo", false),
        ("cwd", "/work/changed", false),
        ("parentSession", "other-parent", false),
        ("id", "bravo", true),
    ] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        let mut value = json!({"type":"session", "version":3, "id":"alpha",
            "cwd":"/work/project", "parentSession":"first-parent",
            "timestamp":"2026-10-01T12:00:00Z", "rlmDepth":0});
        fs::write(&path, format!("{value}\n")).unwrap();
        // Establish a distinct observation even on filesystems whose clock
        // does not tick between the two writes in this deterministic test.
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        let authority = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
        let opened = Arc::new(authority.open_file(Path::new("session.jsonl")).unwrap());
        let (binding, probe) = probe_records_until(&path, &opened, 1, |record| {
            parse_header(record, &path, SourceAnchorScope::Unqualified)
        })
        .unwrap()
        .unwrap();
        value[field] = json!(replacement);
        let output = if replace_object {
            temp.path().join("replacement.jsonl")
        } else {
            path.clone()
        };
        fs::write(&output, format!("{value}\n")).unwrap();
        if replace_object {
            fs::rename(output, &path).unwrap();
        }
        assert!(
            bind_leaf(
                path,
                "session.jsonl".into(),
                authority,
                binding,
                probe,
                SourceAnchorScope::Unqualified
            )
            .is_err(),
            "{field}, replacement={replace_object}"
        );
    }
}

#[test]
fn replayed_header_remains_inside_the_logical_boundary() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    let value = json!({"type":"session", "version":3, "id":"session",
        "cwd":"/work/project", "timestamp":"2026-10-01T12:00:00Z"});
    let bytes = format!("{value}\n");
    fs::write(&path, &bytes).unwrap();
    let authority = Arc::new(ProviderSourceRoot::open(temp.path()).unwrap());
    let opened = Arc::new(authority.open_file(Path::new("session.jsonl")).unwrap());
    let (binding, probe) = probe_records_until(&path, &opened, 1, |record| {
        parse_header(record, &path, SourceAnchorScope::Unqualified)
    })
    .unwrap()
    .unwrap();
    let leaf = bind_leaf(
        path,
        "session.jsonl".into(),
        authority,
        binding,
        probe,
        SourceAnchorScope::Unqualified,
    )
    .unwrap();
    assert!(leaf.clone().with_logical_eof(0).is_err());
    assert!(leaf.clone().with_logical_eof(bytes.len() as u64).is_ok());
}
