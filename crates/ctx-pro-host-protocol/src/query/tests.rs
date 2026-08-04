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
