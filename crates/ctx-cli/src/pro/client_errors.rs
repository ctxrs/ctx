use ctx_pro_host_protocol::ProtocolError;

#[cfg(test)]
use ctx_pro_host_protocol::ErrorClass;

use super::super::diagnostic::BlameDiagnostic;
pub(crate) use super::super::diagnostic::RESOURCE_NOT_FOUND_DIAGNOSTIC;

pub(super) fn protocol_error(error: ProtocolError) -> anyhow::Error {
    anyhow::Error::new(BlameDiagnostic::from_protocol_error(error))
}

pub(crate) fn blame_diagnostic(error: &anyhow::Error) -> Option<BlameDiagnostic> {
    typed_blame_diagnostic(error)
        .cloned()
        .or_else(|| stable_error_code(error).and_then(BlameDiagnostic::for_stable_error_code))
}

pub(crate) fn typed_blame_diagnostic(error: &anyhow::Error) -> Option<&BlameDiagnostic> {
    error.downcast_ref::<BlameDiagnostic>()
}

pub(crate) fn stable_error_code(error: &anyhow::Error) -> Option<&'static str> {
    if let Some(diagnostic) = typed_blame_diagnostic(error) {
        return Some(diagnostic.error_code);
    }
    // Legacy host-side errors predate the typed diagnostic. Protocol errors
    // never reach this parser: `protocol_error` drops helper prose first.
    error
        .chain()
        .find_map(|cause| stable_error_code_from_text(&cause.to_string()))
}

pub(crate) fn stable_error_diagnostic(error: &anyhow::Error) -> Option<&'static str> {
    match stable_error_code(error)? {
        // Preserve the existing renderer contract until the output lane adopts
        // `blame_diagnostic` as one structured object on every surface.
        "resource_not_found" => blame_diagnostic(error).map(|value| value.message),
        _ => None,
    }
}

fn stable_error_code_from_text(text: &str) -> Option<&'static str> {
    let code = text.split(':').next().unwrap_or_default();
    match code {
        "pro_not_installed" => Some("pro_not_installed"),
        "commercial_unavailable" => Some("commercial_unavailable"),
        "commercial_access_locked" => Some("commercial_access_locked"),
        "commercial_identity_conflict" => Some("commercial_identity_conflict"),
        "checkout_expired" => Some("checkout_expired"),
        "checkout_timeout" => Some("checkout_timeout"),
        "anonymous_trial_already_consumed" => Some("anonymous_trial_already_consumed"),
        "anonymous_trial_identity_ambiguous" => Some("anonymous_trial_identity_ambiguous"),
        "anonymous_trial_installation_limit" => Some("anonymous_trial_installation_limit"),
        "helper_upgrade_required" => Some("helper_upgrade_required"),
        "entitlement_required" => Some("entitlement_required"),
        "entitlement_expired" => Some("entitlement_expired"),
        "entitlement_invalid" => Some("entitlement_invalid"),
        "key_store_unavailable" => Some("key_store_unavailable"),
        "key_store_locked" => Some("key_store_locked"),
        "not_materialized" => Some("not_materialized"),
        "needs_rebuild" => Some("needs_rebuild"),
        "partial" => Some("partial"),
        "needs_resume" => Some("needs_resume"),
        "protocol_mismatch" => Some("protocol_mismatch"),
        "source_unavailable" => Some("source_unavailable"),
        "stale_source" => Some("stale_source"),
        "repository_unavailable" => Some("repository_unavailable"),
        "resource_not_found" => Some("resource_not_found"),
        "operation_unavailable" => Some("operation_unavailable"),
        "stale_fact" => Some("stale_fact"),
        "line_out_of_range" => Some("line_out_of_range"),
        "stale_snapshot" => Some("stale_snapshot"),
        "ambiguous" => Some("ambiguous"),
        "corrupt_graph" => Some("corrupt_graph"),
        "invalid_request" => Some("invalid_request"),
        "invalid_response" => Some("invalid_response"),
        "authentication_denied" => Some("authentication_denied"),
        "authentication_expired" => Some("authentication_expired"),
        "authentication_required" => Some("authentication_required"),
        "rate_limited" => Some("rate_limited"),
        "service_unavailable" => Some("service_unavailable"),
        "referral_unavailable" => Some("referral_unavailable"),
        "referral_payout_unavailable" => Some("referral_payout_unavailable"),
        "referral_codename_conflict" => Some("referral_codename_conflict"),
        "referral_not_found" => Some("referral_not_found"),
        "referral_not_eligible" => Some("referral_not_eligible"),
        "cancelled" => Some("cancelled"),
        "helper_crashed" => Some("helper_crashed"),
        "helper_timeout" => Some("helper_timeout"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn generic_protocol_failures_map_to_stable_public_codes() {
        for (class, expected) in [
            (ErrorClass::EntitlementExpired, "entitlement_expired"),
            (ErrorClass::KeyStoreUnavailable, "key_store_unavailable"),
            (ErrorClass::KeyStoreLocked, "key_store_locked"),
            (ErrorClass::NotMaterialized, "not_materialized"),
            (ErrorClass::ProtocolMismatch, "protocol_mismatch"),
            (ErrorClass::MissingSource, "source_unavailable"),
            (ErrorClass::MissingRepository, "repository_unavailable"),
            (ErrorClass::ResourceNotFound, "resource_not_found"),
            (ErrorClass::StaleFact, "stale_fact"),
            (ErrorClass::LineOutOfRange, "line_out_of_range"),
            (ErrorClass::StaleSnapshot, "stale_snapshot"),
            (ErrorClass::Ambiguous, "ambiguous"),
            (ErrorClass::Corrupt, "corrupt_graph"),
            (ErrorClass::InvalidRequest, "invalid_request"),
            (ErrorClass::Bounds, "invalid_request"),
            (ErrorClass::RebuildRequired, "needs_rebuild"),
            (ErrorClass::Sequence, "invalid_response"),
            (ErrorClass::Internal, "helper_crashed"),
        ] {
            let mapped = protocol_error(ProtocolError::new(class, "untrusted helper detail"));
            assert_eq!(mapped.to_string(), expected);
            assert_eq!(stable_error_code(&mapped), Some(expected));
            assert!(!mapped.to_string().contains("untrusted helper detail"));
            assert_eq!(blame_diagnostic(&mapped).unwrap().error_code, expected);
        }
    }

    #[test]
    fn protocol_error_never_retains_malicious_helper_detail_or_source_chain() {
        let mut helper_error = ProtocolError::new(
            ErrorClass::MissingRepository,
            "key-store failed at /home/alice/.ctx/pro: token=secret",
        );
        helper_error.retryable = true;

        let error = protocol_error(helper_error).context("caller context");
        let diagnostic = blame_diagnostic(&error).unwrap();
        let serialized = serde_json::to_string(&diagnostic).unwrap();

        assert_eq!(diagnostic.error, "repository_unavailable");
        assert_eq!(diagnostic.error_code, diagnostic.error);
        assert!(diagnostic.retryable);
        assert!(!format!("{error:#}").contains("alice"));
        assert!(!format!("{error:?}").contains("token=secret"));
        assert!(!serialized.contains("/home/"));
        assert!(!serialized.contains("token=secret"));
    }

    #[test]
    fn operation_and_entitlement_codes_have_stable_typed_diagnostics() {
        for (code, reason) in [
            (
                "operation_unavailable",
                super::super::super::diagnostic::BlameDiagnosticReason::OperationNotCovered,
            ),
            (
                "entitlement_required",
                super::super::super::diagnostic::BlameDiagnosticReason::EntitlementRequired,
            ),
            (
                "entitlement_invalid",
                super::super::super::diagnostic::BlameDiagnosticReason::EntitlementInvalid,
            ),
        ] {
            let error = anyhow!("{code}: ignored legacy host detail");
            assert_eq!(stable_error_code(&error), Some(code));
            let diagnostic = blame_diagnostic(&error).unwrap();
            assert_eq!(diagnostic.error_code, code);
            assert_eq!(diagnostic.reason, reason);
            assert!(!diagnostic.message.contains("ignored legacy host detail"));
        }
    }

    #[test]
    fn commercial_and_referral_codes_survive_anyhow_context_without_changing_error_text() {
        for code in [
            "authentication_denied",
            "authentication_expired",
            "checkout_expired",
            "checkout_timeout",
            "commercial_identity_conflict",
            "rate_limited",
            "referral_not_eligible",
            "service_unavailable",
        ] {
            let error = anyhow!("{code}: bounded service cause").context("referral request failed");
            assert_eq!(error.to_string(), "referral request failed");
            assert_eq!(stable_error_code(&error), Some(code));
        }
    }

    #[test]
    fn missing_resource_has_trusted_prose_without_exposing_helper_detail() {
        let error = protocol_error(ProtocolError::new(
            ErrorClass::ResourceNotFound,
            "untrusted helper detail at /secret/graph/path",
        ));
        assert_eq!(stable_error_code(&error), Some("resource_not_found"));
        assert_eq!(
            stable_error_diagnostic(&error),
            Some(RESOURCE_NOT_FOUND_DIAGNOSTIC)
        );
        assert!(!error.to_string().contains("untrusted helper detail"));
    }
}
