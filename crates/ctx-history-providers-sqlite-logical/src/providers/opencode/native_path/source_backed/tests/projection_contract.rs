use super::*;

#[test]
fn direct_core_projection_uses_neutral_v3_content() {
    let production = [
        include_str!("../../source_backed.rs"),
        include_str!("../projection.rs"),
    ]
    .join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("CoreActivity"));
    assert!(production.contains("ActivityJsonCapture"));
    assert!(production.contains("ProviderDeclaredFact"));
    assert!(production.contains("omit_structured_content_if_aggregate_exceeds_limit"));
    for forbidden in [
        concat!("Repository", "Attributor"),
        concat!("repository_", "bindings"),
        concat!("repository_", "abstentions"),
        concat!("result_", "outcome"),
        concat!("file_", "touches"),
    ] {
        assert!(!production.contains(forbidden), "found {forbidden}");
    }
}

#[test]
fn exact_provider_strings_are_not_trimmed_by_helpers() {
    assert_eq!(
        nonempty("  /literal/path  ".to_owned()).as_deref(),
        Some("  /literal/path  ")
    );
    assert_eq!(nonempty(String::new()), None);
}

#[test]
fn only_payload_content_size_failures_are_record_local() {
    assert!(record_local_core_projection_failure(
        &CoreRecordError::FieldTooLarge {
            field: "selected_content",
            actual: ctx_history_core::MAX_CORE_CONTENT_BYTES + 1,
            maximum: ctx_history_core::MAX_CORE_CONTENT_BYTES,
        }
    ));
    assert!(!record_local_core_projection_failure(
        &CoreRecordError::InvalidIdentityRelationship
    ));
    assert!(!record_local_core_projection_failure(
        &CoreRecordError::InvalidSessionRelationship
    ));
    assert!(!record_local_core_projection_failure(
        &CoreRecordError::InvalidActivity
    ));
}
