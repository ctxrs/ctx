use super::*;
use ctx_history_capture_model::{ProviderRootDefinition, ProviderRootSourceIdentity};

fn canonical_identity_hex(identity: StableEntityId) -> String {
    use std::fmt::Write as _;

    let mut hex = String::with_capacity(StableEntityId::CANONICAL_LEN * 2);
    for byte in identity.encode_canonical().unwrap() {
        write!(&mut hex, "{byte:02x}").unwrap();
    }
    hex
}

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

#[test]
fn named_v1_source_session_and_event_keep_released_identity_bytes() {
    let mut personal_root = ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Codex,
        path: PathBuf::from("/old/codex"),
        group: None,
        kind: None,
    };
    let personal_lineage = ProviderRootSourceIdentity::NamedV1
        .lineage(&personal_root)
        .unwrap();
    personal_root.path = PathBuf::from("/new/codex");
    assert_eq!(
        Some(personal_lineage),
        ProviderRootSourceIdentity::NamedV1.lineage(&personal_root)
    );
    let work_lineage = ProviderRootSourceIdentity::NamedV1
        .lineage(&ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: PathBuf::from("/same/codex"),
            group: None,
            kind: None,
        })
        .unwrap();

    let native_session_id = "019fb100-0000-7000-8000-000000000099";
    let released_key = TypedKey::composite(vec![
        TypedKey::bytes(personal_lineage.to_vec()).unwrap(),
        TypedKey::utf8(native_session_id).unwrap(),
    ])
    .unwrap();
    let released_source = SourceKey::derive_provider_native(
        CaptureProvider::Codex.as_str(),
        CODEX_SESSION_SOURCE_FORMAT,
        CODEX_SOURCE_SCHEMA_VARIANT,
        1,
        CODEX_SOURCE_ANCHOR_NAMESPACE,
        released_key,
    )
    .unwrap();
    let source = codex_source_key_in_root(Some(personal_lineage), native_session_id).unwrap();
    let work_source = codex_source_key_in_root(Some(work_lineage), native_session_id).unwrap();
    assert_ne!(source.identity(), work_source.identity());
    assert_eq!(
        released_source.identity().encode_canonical().unwrap(),
        source.identity().encode_canonical().unwrap()
    );

    let released_session = codex_session_identity(&released_source, native_session_id).unwrap();
    let session = codex_session_identity(&source, native_session_id).unwrap();
    assert_eq!(
        released_session.encode_canonical().unwrap(),
        session.encode_canonical().unwrap()
    );

    let provider_identity = CodexProviderEventIdentityV0 {
        kind: CodexProviderEventIdentityKindV0::CallId,
        value: "golden-call".to_owned(),
    };
    let (_, parts) =
        provider_event_key_parts("tool_output", Some("tool"), &provider_identity).unwrap();
    let (released_event, _) =
        event_identity_for_occurrence(&released_source, released_session, &parts, 0).unwrap();
    let (event, _) = event_identity_for_occurrence(&source, session, &parts, 0).unwrap();
    assert_eq!(
        released_event.encode_canonical().unwrap(),
        event.encode_canonical().unwrap()
    );

    assert_eq!(
        (
            source.identity().to_string(),
            session.to_string(),
            event.to_string(),
        ),
        (
            "796dd0f0-8a62-8d49-a3d2-d2efc73164e0".to_owned(),
            "0e7c1d4e-65ad-8c75-b40b-dcc7f3605668".to_owned(),
            "9bfb5308-a958-8ebd-9104-09e4edb38f33".to_owned(),
        )
    );
    assert_eq!(
        (
            canonical_identity_hex(source.identity()),
            canonical_identity_hex(session),
            canonical_identity_hex(event),
        ),
        (
            "000101796dd0f08a62ad4923d2d2efc73164e07696effd054cbdf71d6d2886385f1308796dd0f08a62ad4923d2d2efc73164e07696effd054cbdf71d6d2886385f13080000000000000000000000000000000000000000000000000000000000000000796dd0f08a628d49a3d2d2efc73164e0".to_owned(),
            "0001020e7c1d4e65ad5c75740bdcc7f3605668ab72d62a17fa9687bfd047b3152bbc89796dd0f08a62ad4923d2d2efc73164e07696effd054cbdf71d6d2886385f1308801164bc3ba2621810b4b3d426f7dd83895150524aec161db167168e7e0e9b7b0e7c1d4e65ad8c75b40bdcc7f3605668".to_owned(),
            "0001039bfb5308a9585ebd910409e4edb38f3313bfcd09230a9f60937f1c53477cfeff796dd0f08a62ad4923d2d2efc73164e07696effd054cbdf71d6d2886385f1308801164bc3ba2621810b4b3d426f7dd83895150524aec161db167168e7e0e9b7b9bfb5308a9588ebd910409e4edb38f33".to_owned(),
        )
    );
}
