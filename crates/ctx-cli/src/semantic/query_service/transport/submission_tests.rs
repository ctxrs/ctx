use super::*;

#[test]
fn submission_marker_survives_context_and_preserves_the_transport_source() {
    let error = std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "response connection reset",
    );
    let error = mark_request_may_have_been_submitted(error.into());

    assert!(request_may_have_been_submitted(&error));
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::ConnectionReset)
    );
}

#[cfg(unix)]
#[test]
fn identical_disconnect_kind_is_unavailable_only_before_submission() {
    use super::super::{daemon_query_roundtrip_error_is_unavailable, DaemonQueryEndpoint};

    let endpoint = DaemonQueryEndpoint::Unix {
        path: "/tmp/ctx-pre-submit-classification.sock".into(),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    };
    let pre_submission = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "connect reset",
    ));
    let post_submission = mark_request_may_have_been_submitted(anyhow::Error::new(
        std::io::Error::new(std::io::ErrorKind::ConnectionReset, "response reset"),
    ));

    assert!(daemon_query_roundtrip_error_is_unavailable(
        &endpoint,
        &pre_submission
    ));
    assert!(!daemon_query_roundtrip_error_is_unavailable(
        &endpoint,
        &post_submission
    ));
}
