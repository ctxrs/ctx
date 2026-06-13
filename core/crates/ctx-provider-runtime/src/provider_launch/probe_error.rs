use ctx_harness_sources::HarnessEndpointVerificationStatus;

pub fn classify_probe_error(
    message: &str,
) -> (
    &'static str,
    Option<bool>,
    HarnessEndpointVerificationStatus,
) {
    let lower = message.to_ascii_lowercase();
    let auth_required = [
        "401",
        "403",
        "unauthorized",
        "forbidden",
        "authentication required",
        "auth required",
        "auth_required",
        "auth failed",
        "auth_failed",
        "auth error",
        "auth_error",
        "not authenticated",
        "not logged in",
        "login required",
        "sign in",
        "api key",
        "missing token",
        "invalid token",
        "expired token",
        "access token",
        "bearer token",
        "active account",
        "account env",
        "configure an active account",
    ];
    if auth_required.iter().any(|needle| lower.contains(needle)) {
        return (
            "auth_required",
            Some(true),
            HarnessEndpointVerificationStatus::Invalid,
        );
    }
    let protocol_error = [
        "invalid message",
        "invalid acp",
        "invalid crp",
        "models.list response",
        "models.list probe",
    ];
    if protocol_error.iter().any(|needle| lower.contains(needle)) {
        return (
            "error",
            Some(false),
            HarnessEndpointVerificationStatus::Error,
        );
    }
    if lower.contains("timeout")
        || lower.contains("connection refused")
        || lower.contains("network")
        || lower.contains("econn")
        || lower.contains("dns")
        || lower.contains("tls")
    {
        return (
            "network_error",
            Some(false),
            HarnessEndpointVerificationStatus::Error,
        );
    }
    (
        "error",
        Some(false),
        HarnessEndpointVerificationStatus::Error,
    )
}

#[cfg(test)]
mod tests {
    use ctx_harness_sources::HarnessEndpointVerificationStatus;

    use super::classify_probe_error;

    #[test]
    fn classify_probe_error_detects_auth_failures() {
        assert_eq!(
            classify_probe_error("401 unauthorized: missing token"),
            (
                "auth_required",
                Some(true),
                HarnessEndpointVerificationStatus::Invalid
            )
        );
    }

    #[test]
    fn classify_probe_error_detects_protocol_failures() {
        assert_eq!(
            classify_probe_error("models.list response was malformed"),
            (
                "error",
                Some(false),
                HarnessEndpointVerificationStatus::Error
            )
        );
    }

    #[test]
    fn classify_probe_error_detects_network_failures() {
        assert_eq!(
            classify_probe_error("connection refused while dialing endpoint"),
            (
                "network_error",
                Some(false),
                HarnessEndpointVerificationStatus::Error
            )
        );
    }

    #[test]
    fn classify_probe_error_defaults_to_generic_error() {
        assert_eq!(
            classify_probe_error("unexpected provider probe failure"),
            (
                "error",
                Some(false),
                HarnessEndpointVerificationStatus::Error
            )
        );
    }
}
