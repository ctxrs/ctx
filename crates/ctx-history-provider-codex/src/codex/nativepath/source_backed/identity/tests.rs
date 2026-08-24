use super::*;

#[test]
fn copied_result_targets_the_exact_duplicate_provider_occurrence() {
    let copy = CodexProviderNativeEventCopyV0 {
        ancestor_native_session_id: "019fb100-0000-7000-8000-000000000001".to_owned(),
        result_call_id: "duplicate-provider-call".to_owned(),
    };
    let provider_identity = CodexProviderEventIdentityV0 {
        kind: CodexProviderEventIdentityKindV0::CallId,
        value: "duplicate-provider-call".to_owned(),
    };
    let first = copied_result_event_copy(
        &copy,
        &provider_identity,
        "tool_output",
        Some("tool"),
        0,
        None,
    )
    .unwrap()
    .unwrap();
    let second = copied_result_event_copy(
        &copy,
        &provider_identity,
        "tool_output",
        Some("tool"),
        1,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        first.proof,
        ctx_history_core::ProviderNativeCopyProof::NativeCallResultIdentity
    );
    assert_ne!(first.ancestor_event_id, second.ancestor_event_id);
}

#[test]
fn copied_result_abstains_without_an_exact_call_identity() {
    let copy = CodexProviderNativeEventCopyV0 {
        ancestor_native_session_id: "019fb100-0000-7000-8000-000000000002".to_owned(),
        result_call_id: "duplicate-provider-call".to_owned(),
    };
    let provider_identity = CodexProviderEventIdentityV0 {
        kind: CodexProviderEventIdentityKindV0::Id,
        value: "duplicate-provider-call".to_owned(),
    };
    assert!(copied_result_event_copy(
        &copy,
        &provider_identity,
        "tool_output",
        Some("tool"),
        0,
        None,
    )
    .unwrap()
    .is_none());
}

#[test]
fn session_tree_sources_are_distinct_across_homes_and_stable_within_one_home() {
    let personal_sessions =
        codex_session_tree_source_root_lineage(Path::new("/tmp/personal/sessions")).unwrap();
    let personal_archive =
        codex_session_tree_source_root_lineage(Path::new("/tmp/personal/archived_sessions"))
            .unwrap();
    let work = codex_session_tree_source_root_lineage(Path::new("/tmp/work/sessions")).unwrap();
    assert_eq!(personal_sessions, personal_archive);
    assert_ne!(personal_sessions, work);

    let native_session_id = "019fb100-0000-7000-8000-000000000099";
    let personal = codex_source_key_in_root(Some(personal_sessions), native_session_id).unwrap();
    let archived = codex_source_key_in_root(Some(personal_archive), native_session_id).unwrap();
    let work = codex_source_key_in_root(Some(work), native_session_id).unwrap();
    assert!(personal.exact_descriptor_eq(&archived));
    assert!(!personal.exact_descriptor_eq(&work));
}
