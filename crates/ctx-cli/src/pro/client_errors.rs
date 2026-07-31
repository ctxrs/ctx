use anyhow::anyhow;
use ctx_pro_host_protocol::{ErrorClass, ProtocolError};

pub(crate) const RESOURCE_NOT_FOUND_DIAGNOSTIC: &str =
    "No indexed Pro resource matches the requested blame target.";

pub(super) fn protocol_error(error: ProtocolError) -> anyhow::Error {
    let code = match error.class {
        ErrorClass::EntitlementExpired => "entitlement_expired",
        ErrorClass::KeyStoreUnavailable => "key_store_unavailable",
        ErrorClass::KeyStoreLocked => "key_store_locked",
        ErrorClass::NotMaterialized => "not_materialized",
        ErrorClass::ProtocolMismatch => "protocol_mismatch",
        ErrorClass::MissingSource => "source_unavailable",
        ErrorClass::MissingRepository => "repository_unavailable",
        ErrorClass::ResourceNotFound => "resource_not_found",
        ErrorClass::StaleFact => "stale_fact",
        ErrorClass::LineOutOfRange => "line_out_of_range",
        ErrorClass::StaleSnapshot => "stale_snapshot",
        ErrorClass::Ambiguous => "ambiguous",
        ErrorClass::Corrupt => "corrupt_graph",
        ErrorClass::InvalidRequest | ErrorClass::Bounds => "invalid_request",
        ErrorClass::Sequence => "invalid_response",
        ErrorClass::Internal => "helper_crashed",
    };
    // Helper error details are untrusted and can contain local paths or key-store diagnostics.
    // The typed class is the complete stable public error contract.
    anyhow!(code)
}

pub(crate) fn stable_error_code(error: &anyhow::Error) -> Option<&'static str> {
    error
        .chain()
        .find_map(|cause| stable_error_code_from_text(&cause.to_string()))
}

pub(crate) fn stable_error_diagnostic(error: &anyhow::Error) -> Option<&'static str> {
    match stable_error_code(error)? {
        "resource_not_found" => Some(RESOURCE_NOT_FOUND_DIAGNOSTIC),
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
        "entitlement_expired" => Some("entitlement_expired"),
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

    #[test]
    fn generic_protocol_failures_map_to_stable_public_codes() {
        for (class, expected) in [
            (ErrorClass::ProtocolMismatch, "protocol_mismatch"),
            (ErrorClass::MissingSource, "source_unavailable"),
            (ErrorClass::MissingRepository, "repository_unavailable"),
            (ErrorClass::ResourceNotFound, "resource_not_found"),
            (ErrorClass::StaleFact, "stale_fact"),
            (ErrorClass::LineOutOfRange, "line_out_of_range"),
            (ErrorClass::StaleSnapshot, "stale_snapshot"),
            (ErrorClass::Sequence, "invalid_response"),
            (ErrorClass::Internal, "helper_crashed"),
        ] {
            let mapped = protocol_error(ProtocolError::new(class, "untrusted helper detail"));
            assert_eq!(mapped.to_string(), expected);
            assert_eq!(stable_error_code(&mapped), Some(expected));
            assert!(!mapped.to_string().contains("untrusted helper detail"));
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
