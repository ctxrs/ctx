use super::*;

#[test]
fn repository_scope_distinguishes_omitted_empty_and_exact_identity() {
    let omitted: BlameTarget = serde_json::from_str(r#"{"kind":"commit","oid":"abc123"}"#).unwrap();
    assert!(matches!(
        &omitted,
        BlameTarget::Commit {
            repository: None,
            ..
        }
    ));
    omitted.validate().unwrap();

    for repository in ["", "   ", "\t"] {
        let error = BlameTarget::Commit {
            oid: "abc123".to_owned(),
            repository: Some(repository.to_owned()),
        }
        .validate()
        .unwrap_err();
        assert_eq!(error.class, ErrorClass::InvalidRequest);
    }

    let identity = "workspace:CaseSensitiveRepo";
    let exact = BlameTarget::Commit {
        oid: "abc123".to_owned(),
        repository: Some(identity.to_owned()),
    };
    exact.validate().unwrap();
    assert!(matches!(
        exact,
        BlameTarget::Commit {
            repository: Some(repository),
            ..
        } if repository == identity
    ));
}

#[test]
fn commit_selectors_are_four_to_sixty_four_ascii_hex_characters() {
    for oid in ["abcd".to_owned(), "AbCdEf12".to_owned(), "F".repeat(64)] {
        BlameTarget::Commit {
            oid,
            repository: None,
        }
        .validate()
        .unwrap();
    }

    for oid in [
        "abc".to_owned(),
        "a".repeat(65),
        "HEAD".to_owned(),
        "abcg".to_owned(),
        "deadbeef^".to_owned(),
        "abcd\n".to_owned(),
        "ａｂｃｄ".to_owned(),
    ] {
        let error = BlameTarget::Commit {
            oid,
            repository: None,
        }
        .validate()
        .unwrap_err();
        assert_eq!(error.class, ErrorClass::InvalidRequest);
        assert_eq!(
            error.message,
            "commit selector must contain 4 to 64 ASCII hexadecimal characters"
        );
    }
}

#[test]
fn logical_repository_canonicalization_only_lowercases_the_forge_host() {
    assert_eq!(
        canonical_logical_repository_id("forge:GitHub.COM/ctxrs/ctx?view=1#readme"),
        "forge:github.com/ctxrs/ctx?view=1#readme"
    );
    for identity in [
        "https://GitHub.COM/ctxrs/ctx",
        "forge:github.com/ctxrs/ctx/",
        "forge:github.com/ctxrs/ctx?view=1",
        "forge:github.com/ctxrs/ctx#readme",
        "workspace:CaseSensitiveRepo",
    ] {
        assert_eq!(canonical_logical_repository_id(identity), identity);
    }
}

#[test]
fn repository_scope_preserves_scheme_trailing_slash_query_and_fragment() {
    let resolved = ResourceRef {
        id: "repository:1".to_owned(),
        kind: ResourceKind::Repository,
        display: "forge:github.com/ctxrs/ctx".to_owned(),
    };
    assert!(repository_selector_matches(
        Some("forge:GitHub.COM/ctxrs/ctx"),
        &resolved
    ));
    for distinct in [
        "https://github.com/ctxrs/ctx",
        "forge:github.com/ctxrs/ctx/",
        "forge:github.com/ctxrs/ctx?view=1",
        "forge:github.com/ctxrs/ctx#readme",
    ] {
        assert!(!repository_selector_matches(Some(distinct), &resolved));
    }
}

fn test_resource(kind: ResourceKind, display: &str) -> ResourceRef {
    ResourceRef {
        id: format!("{kind:?}:{display}"),
        kind,
        display: display.to_owned(),
    }
}

#[test]
fn chronology_fields_preserve_exact_milliseconds_and_strict_shapes() {
    let attribution = AgentAttribution {
        id: "fact:production".to_owned(),
        relationship: ProductionRelationship::ProducedBy,
        producing_session: test_resource(ResourceKind::Session, "producer"),
        parent_session: None,
        direct_actor: None,
        owning_root: None,
        fact_occurred_at_ms: Some(1_721_000_000_123),
        confidence: FactConfidence::Explicit,
        state: FactState::Asserted,
        evidence_numbers: vec![1],
    };
    let encoded = serde_json::to_value(&attribution).unwrap();
    assert_eq!(encoded["fact_occurred_at_ms"], 1_721_000_000_123_i64);
    assert_eq!(
        serde_json::from_value::<AgentAttribution>(encoded.clone()).unwrap(),
        attribution
    );

    let mut unknown = encoded;
    unknown
        .as_object_mut()
        .unwrap()
        .insert("commit_time_ms".to_owned(), serde_json::json!(0));
    assert!(serde_json::from_value::<AgentAttribution>(unknown).is_err());

    let membership = PullRequestCommit {
        fact_id: "fact:membership".to_owned(),
        relationship: PullRequestCommitRelationship::ContainsCommit,
        commit: test_resource(ResourceKind::Commit, "deadbeef"),
        fact_occurred_at_ms: Some(-123),
        production: vec![attribution],
        evidence_numbers: vec![2],
    };
    let encoded = serde_json::to_value(&membership).unwrap();
    assert_eq!(encoded["fact_occurred_at_ms"], -123);
    assert_eq!(
        serde_json::from_value::<PullRequestCommit>(encoded).unwrap(),
        membership
    );
}

#[test]
fn missing_chronology_remains_quiet_nullable_data() {
    let attribution: AgentAttribution = serde_json::from_value(serde_json::json!({
        "id": "fact:production",
        "relationship": "produced_by",
        "producing_session": {"id": "session:producer", "kind": "session", "display": "producer"},
        "parent_session": null,
        "direct_actor": null,
        "owning_root": null,
        "confidence": "explicit",
        "state": "asserted",
        "evidence_numbers": []
    }))
    .unwrap();
    assert_eq!(attribution.fact_occurred_at_ms, None);
}
