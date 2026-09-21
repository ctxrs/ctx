use super::*;

#[test]
fn direct_materialization_predecessor_requires_a_clean_rebuild() {
    assert_ne!(
        PRE_DIRECT_MATERIALIZATION_SEGMENT_SCHEMA_IDENTITY,
        SEGMENT_SCHEMA_IDENTITY
    );
    assert!(schema_identity_requires_clean_rebuild(
        PRE_DIRECT_MATERIALIZATION_SEGMENT_SCHEMA_IDENTITY
    ));
    assert!(schema_identity_requires_clean_rebuild(
        crate::graph::segment_graph::PRE_OPTIONAL_ROOT_SEGMENT_SCHEMA_IDENTITY
    ));
    assert!(schema_identity_requires_clean_rebuild(
        PRE_STATIC_SHELL_QUOTING_SEGMENT_SCHEMA_IDENTITY
    ));
    assert!(!schema_identity_requires_clean_rebuild(
        SEGMENT_SCHEMA_IDENTITY
    ));
    for unsupported in [
        "sha256:d798972ae1412933c1b4c858ad5431a453898fb989621fba7492232cc6d9b1fa",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    ] {
        assert_ne!(unsupported, SEGMENT_SCHEMA_IDENTITY);
        assert!(!schema_identity_requires_clean_rebuild(unsupported));
    }
}
