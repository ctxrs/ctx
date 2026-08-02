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
