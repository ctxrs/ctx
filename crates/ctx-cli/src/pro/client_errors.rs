use anyhow::anyhow;
use ctx_pro_host_protocol::{ErrorClass, ProtocolError};

pub(super) fn protocol_error(error: ProtocolError) -> anyhow::Error {
    let code = match error.class {
        ErrorClass::EntitlementExpired => "entitlement_expired",
        ErrorClass::KeyStoreUnavailable => "key_store_unavailable",
        ErrorClass::KeyStoreLocked => "key_store_locked",
        ErrorClass::NotMaterialized => "not_materialized",
        ErrorClass::ProtocolMismatch => "protocol_mismatch",
        ErrorClass::MissingSource => "source_unavailable",
        ErrorClass::MissingRepository => "repository_unavailable",
        ErrorClass::StaleFact => "stale_fact",
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
    let text = error.to_string();
    let code = text.split(':').next().unwrap_or_default();
    match code {
        "pro_not_installed" => Some("pro_not_installed"),
        "commercial_unavailable" => Some("commercial_unavailable"),
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
        "repository_unavailable" => Some("repository_unavailable"),
        "stale_fact" => Some("stale_fact"),
        "ambiguous" => Some("ambiguous"),
        "corrupt_graph" => Some("corrupt_graph"),
        "invalid_request" => Some("invalid_request"),
        "invalid_response" => Some("invalid_response"),
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
            (ErrorClass::StaleFact, "stale_fact"),
        ] {
            let mapped = protocol_error(ProtocolError::new(class, "untrusted helper detail"));
            assert_eq!(mapped.to_string(), expected);
            assert_eq!(stable_error_code(&mapped), Some(expected));
        }
    }
}
