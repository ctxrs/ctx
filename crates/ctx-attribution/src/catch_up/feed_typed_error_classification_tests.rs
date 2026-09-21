use super::stable_core_error_code;
use crate::materializer::SegmentMaterializerError;
use anyhow::anyhow;

#[test]
fn direct_materializer_errors_keep_their_stable_codes_through_context() {
    for (error, expected) in [
        (SegmentMaterializerError::RebuildRequired, "needs_rebuild"),
        (SegmentMaterializerError::Bounds, "bounds"),
    ] {
        let error: anyhow::Error = error.into();
        assert_eq!(stable_core_error_code(&error), Some(expected));
        assert_eq!(
            stable_core_error_code(&error.context("activate direct Core generation")),
            Some(expected)
        );
    }
}

#[test]
fn materializer_type_owns_classification_over_context_text() {
    let error = anyhow::Error::new(SegmentMaterializerError::Bounds)
        .context("needs_rebuild: activation context");
    assert_eq!(stable_core_error_code(&error), Some("bounds"));
    let error = anyhow::Error::new(SegmentMaterializerError::RebuildRequired)
        .context("bounds: activation context");
    assert_eq!(stable_core_error_code(&error), Some("needs_rebuild"));
}

#[test]
fn unrelated_errors_and_existing_port_codes_keep_their_classification() {
    for error in [
        SegmentMaterializerError::Busy,
        SegmentMaterializerError::Conflict,
        SegmentMaterializerError::Corrupt("needs_rebuild"),
    ] {
        assert_eq!(stable_core_error_code(&error.into()), None);
    }
    assert_eq!(
        stable_core_error_code(&anyhow!("unclassified failure")),
        None
    );
    let error = anyhow!("source_busy: immutable Core generation changed")
        .context("open pinned Core generation");
    assert_eq!(stable_core_error_code(&error), Some("source_busy"));
}
