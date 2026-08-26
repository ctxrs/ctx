use super::*;

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [include_str!("../source_backed.rs")];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("PARSER_REVISION"));
    assert!(production.contains("agent_scope = Some(AgentScope::Primary)"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("core.event.text.clone()"));
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
}

#[test]
fn root_scope_wraps_project_session_identity_once_and_unqualified_is_unchanged() {
    let legacy =
        codebuddy_source_key_for_identity(CodeBuddySourceShape::Cli, "project", "same-session")
            .unwrap();
    let unqualified = codebuddy_source_key_for_identity_scoped(
        CodeBuddySourceShape::Cli,
        "project",
        "same-session",
        SourceAnchorScope::Unqualified,
    )
    .unwrap();
    let first = codebuddy_source_key_for_identity_scoped(
        CodeBuddySourceShape::Cli,
        "project",
        "same-session",
        SourceAnchorScope::Lineage([1; 32]),
    )
    .unwrap();
    let second = codebuddy_source_key_for_identity_scoped(
        CodeBuddySourceShape::Cli,
        "project",
        "same-session",
        SourceAnchorScope::Lineage([2; 32]),
    )
    .unwrap();

    assert!(legacy.exact_descriptor_eq(&unqualified));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        codebuddy_session_id(&first, "same-session").unwrap(),
        codebuddy_session_id(&second, "same-session").unwrap()
    );
}
