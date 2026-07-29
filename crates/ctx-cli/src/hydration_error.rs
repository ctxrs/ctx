use ctx_history_capture::complete_content::CompleteContentErrorKind;
use ctx_history_core::{HydrationFailure, HydrationFailureKind};
use serde_json::{json, Value};

use crate::semantic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PublicHydrationClass {
    error_kind: CompleteContentErrorKind,
    failure_kind: &'static str,
    safe_detail: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceHydrationErrorContract {
    class: PublicHydrationClass,
    retryable: bool,
}

impl SourceHydrationErrorContract {
    pub(crate) fn from_failure(failure: &HydrationFailure, retryable: bool) -> Self {
        Self {
            class: public_hydration_class(failure.kind),
            retryable,
        }
    }

    pub(crate) fn detail(&self) -> &'static str {
        self.class.safe_detail
    }

    pub(crate) fn structured(&self) -> Value {
        let error_code = self.class.error_kind.as_str();
        let error = format!("{error_code}/{}", self.class.failure_kind);
        json!({
            "error": error,
            "error_code": error_code,
            "failure_kind": self.class.failure_kind,
            "detail": self.class.safe_detail,
            "retryable": self.retryable,
        })
    }
}

pub(crate) fn source_hydration_error_contract(
    error: &anyhow::Error,
) -> Option<SourceHydrationErrorContract> {
    let retryable = semantic::PinnedSourceBackedGeneration::source_hydration_retryable(error);
    let failure = semantic::PinnedSourceBackedGeneration::source_hydration_failure(error)?;
    Some(SourceHydrationErrorContract::from_failure(
        &failure, retryable,
    ))
}

fn public_hydration_class(kind: HydrationFailureKind) -> PublicHydrationClass {
    match kind {
        HydrationFailureKind::TemporarilyUnavailable => PublicHydrationClass {
            error_kind: CompleteContentErrorKind::SourceUnreadable,
            failure_kind: "temporarily_unavailable",
            safe_detail: "source hydration is temporarily unavailable",
        },
        HydrationFailureKind::ConfirmedDeleted => PublicHydrationClass {
            error_kind: CompleteContentErrorKind::SourceMissing,
            failure_kind: "confirmed_deleted",
            safe_detail: "the indexed source is no longer available",
        },
        HydrationFailureKind::StaleSourceEvidence => PublicHydrationClass {
            error_kind: CompleteContentErrorKind::SourceChanged,
            failure_kind: "stale_source_evidence",
            safe_detail: "the source identity changed after indexing",
        },
        HydrationFailureKind::StaleRecordEvidence => PublicHydrationClass {
            error_kind: CompleteContentErrorKind::ContentVerificationFailed,
            failure_kind: "stale_record_evidence",
            safe_detail: "the source record changed after indexing",
        },
        HydrationFailureKind::MissingRecord => PublicHydrationClass {
            error_kind: CompleteContentErrorKind::SourceRecordMissing,
            failure_kind: "missing_record",
            safe_detail: "the indexed source record is no longer available",
        },
        HydrationFailureKind::UnsupportedParserRevision => PublicHydrationClass {
            error_kind: CompleteContentErrorKind::HydrationUnsupported,
            failure_kind: "unsupported_parser_revision",
            safe_detail: "the source parser revision does not support hydration",
        },
        HydrationFailureKind::InvalidLocator => PublicHydrationClass {
            error_kind: CompleteContentErrorKind::ContentVerificationFailed,
            failure_kind: "invalid_locator",
            safe_detail: "the indexed source locator could not be verified",
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn seven_internal_failures_map_to_exactly_six_safe_public_states() {
        let cases = [
            (
                HydrationFailureKind::TemporarilyUnavailable,
                "source_unreadable",
                "temporarily_unavailable",
                true,
            ),
            (
                HydrationFailureKind::ConfirmedDeleted,
                "source_missing",
                "confirmed_deleted",
                true,
            ),
            (
                HydrationFailureKind::StaleSourceEvidence,
                "source_changed",
                "stale_source_evidence",
                true,
            ),
            (
                HydrationFailureKind::StaleRecordEvidence,
                "content_verification_failed",
                "stale_record_evidence",
                true,
            ),
            (
                HydrationFailureKind::MissingRecord,
                "source_record_missing",
                "missing_record",
                true,
            ),
            (
                HydrationFailureKind::UnsupportedParserRevision,
                "hydration_unsupported",
                "unsupported_parser_revision",
                false,
            ),
            (
                HydrationFailureKind::InvalidLocator,
                "content_verification_failed",
                "invalid_locator",
                false,
            ),
        ];
        let mut public_codes = BTreeSet::new();

        for (kind, error_code, failure_kind, retryable) in cases {
            let failure = HydrationFailure {
                kind,
                detail: "secret provider content at /private/source/path".to_owned(),
            };
            let contract = SourceHydrationErrorContract::from_failure(&failure, retryable);
            let structured = contract.structured();
            let encoded = serde_json::to_string(&structured).unwrap();
            let reparsed: Value = serde_json::from_str(&encoded).unwrap();

            public_codes.insert(error_code);
            assert_eq!(reparsed["error"], format!("{error_code}/{failure_kind}"));
            assert_eq!(reparsed["error_code"], error_code);
            assert_eq!(reparsed["failure_kind"], failure_kind);
            assert_eq!(reparsed["retryable"], retryable);
            assert!(reparsed["detail"]
                .as_str()
                .is_some_and(|value| !value.is_empty()));
            assert!(!encoded.contains("secret provider content"));
            assert!(!encoded.contains("/private/source/path"));
            assert_eq!(
                failure.detail, "secret provider content at /private/source/path",
                "public mapping must borrow, not consume, internal cause detail"
            );
        }

        assert_eq!(
            public_codes,
            BTreeSet::from([
                "content_verification_failed",
                "hydration_unsupported",
                "source_changed",
                "source_missing",
                "source_record_missing",
                "source_unreadable",
            ])
        );
    }

    #[test]
    fn ordinary_cli_json_is_byte_stable_and_budget_projection_stays_generic() {
        let ordinary = HydrationFailure {
            kind: HydrationFailureKind::StaleRecordEvidence,
            detail: "secret provider content at /private/source/path".to_owned(),
        };
        let ordinary_json = serde_json::to_string(
            &SourceHydrationErrorContract::from_failure(&ordinary, true).structured(),
        )
        .unwrap();
        assert_eq!(
            ordinary_json,
            r#"{"detail":"the source record changed after indexing","error":"content_verification_failed/stale_record_evidence","error_code":"content_verification_failed","failure_kind":"stale_record_evidence","retryable":true}"#
        );

        let budget = HydrationFailure {
            kind: HydrationFailureKind::TemporarilyUnavailable,
            detail: "hydration_budget_exceeded/content_too_large at /private/source/path"
                .to_owned(),
        };
        let public = SourceHydrationErrorContract::from_failure(&budget, false).structured();
        assert_eq!(
            public,
            json!({
                "error": "source_unreadable/temporarily_unavailable",
                "error_code": "source_unreadable",
                "failure_kind": "temporarily_unavailable",
                "detail": "source hydration is temporarily unavailable",
                "retryable": false,
            })
        );
        assert!(budget.detail.contains("hydration_budget_exceeded"));
        assert!(!public.to_string().contains("hydration_budget_exceeded"));
        assert!(!public.to_string().contains("/private/source/path"));
    }
}
