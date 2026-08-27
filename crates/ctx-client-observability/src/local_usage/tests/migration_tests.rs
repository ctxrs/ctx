use std::fs;

use super::{operation, private_tempdir};
use crate::local_usage::{read_report, store};

fn expected_output_bytes(schema_version: i64) -> u64 {
    if schema_version == store::LEGACY_SCHEMA_VERSION {
        1_300
    } else {
        1_323
    }
}

fn source_user_version(root: &std::path::Path) -> i64 {
    let bytes = fs::read(store::usage_path(root)).unwrap();
    i64::from(u32::from_be_bytes(bytes[60..64].try_into().unwrap()))
}

fn assert_neutral_fixture_report(root: &std::path::Path, schema_version: i64) {
    let report = read_report(root, true, true);
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains("blame"));
    assert!(!serialized.contains("pro_status"));
    assert!(!serialized.contains("citation_count"));
    assert!(!serialized.contains("pro_blame"));
    assert!(!serialized.contains("\"operation\":\"sql\""));

    let definitions = report.definitions.unwrap();
    assert_eq!(definitions.len(), 1);
    let definition = &definitions[0];
    assert_eq!(
        definition.definition_version,
        if schema_version == 1 { 1 } else { 2 }
    );
    assert_eq!(definition.summary.calls, 7);
    assert_eq!(definition.summary.successful_calls, 6);
    assert_eq!(definition.summary.failed_calls, 1);
    assert_eq!(definition.summary.result_count, 8);
    assert_eq!(
        definition.summary.delivered_output_bytes,
        expected_output_bytes(schema_version)
    );
    assert_eq!(
        definition.summary.delivered_context_bytes,
        if schema_version == 1 { 0 } else { 120 }
    );
    assert_eq!(
        definition.summary.matched_normalized_session_bytes,
        if schema_version == 1 { 0 } else { 300 }
    );
    assert_eq!(
        definition.summary.complete_context_eligible_calls,
        if schema_version == 1 { 0 } else { 3 }
    );
}

#[test]
fn every_released_predecessor_detached_migration_is_deterministic_and_source_read_only() {
    for schema_version in 1..=4 {
        let root = private_tempdir();
        store::create_released_fixture_for_test(root.path(), schema_version).unwrap();
        let path = store::usage_path(root.path());
        let before = fs::read(&path).unwrap();

        assert_neutral_fixture_report(root.path(), schema_version);
        assert_neutral_fixture_report(root.path(), schema_version);

        assert_eq!(fs::read(path).unwrap(), before);
        assert_eq!(source_user_version(root.path()), schema_version);
    }
}

#[test]
fn every_released_predecessor_write_migration_preserves_neutral_counts_and_is_idempotent() {
    for schema_version in 1..=4 {
        let root = private_tempdir();
        store::create_released_fixture_for_test(root.path(), schema_version).unwrap();

        store::record(root.path(), operation("doctor")).unwrap();
        assert_eq!(source_user_version(root.path()), store::SCHEMA_VERSION);
        let first = read_report(root.path(), true, true);
        let second = read_report(root.path(), true, true);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );

        if schema_version == store::LEGACY_SCHEMA_VERSION {
            assert!(first.estimates.is_none());
        } else {
            let estimates = first.estimates.as_ref().unwrap();
            assert_eq!(
                estimates.approximate_context_tokens.delivered_context_bytes,
                120
            );
            assert_eq!(
                estimates
                    .estimated_context_reduction
                    .comparison_baseline_bytes,
                300
            );
        }

        let definitions = first.definitions.unwrap();
        assert_eq!(definitions.len(), 2);
        if schema_version == store::LEGACY_SCHEMA_VERSION {
            assert_eq!(definitions[0].definition_version, 1);
            assert_eq!(definitions[0].summary.calls, 7);
            assert_eq!(definitions[0].summary.delivered_output_bytes, 1_300);
        } else {
            assert_eq!(definitions[0].definition_version, 2);
            assert_eq!(definitions[0].summary.calls, 7);
            assert_eq!(definitions[0].summary.delivered_output_bytes, 1_323);
            assert_eq!(definitions[0].summary.delivered_context_bytes, 120);
            assert_eq!(definitions[0].summary.matched_normalized_session_bytes, 300);
        }
        assert_eq!(definitions[1].definition_version, 3);
        assert_eq!(definitions[1].summary.calls, 1);
        assert_eq!(definitions[1].summary.delivered_output_bytes, 1);
    }
}

#[test]
fn every_released_predecessor_migration_recovers_after_precommit_failure() {
    for schema_version in 1..=4 {
        let root = private_tempdir();
        store::create_released_fixture_for_test(root.path(), schema_version).unwrap();

        assert!(store::fail_migration_before_commit_for_test(root.path()).is_err());
        assert_eq!(source_user_version(root.path()), schema_version);
        assert_neutral_fixture_report(root.path(), schema_version);

        store::record(root.path(), operation("doctor")).unwrap();
        assert_eq!(source_user_version(root.path()), store::SCHEMA_VERSION);
    }
}
