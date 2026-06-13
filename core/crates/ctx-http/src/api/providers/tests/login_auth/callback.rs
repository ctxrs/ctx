use super::*;

#[test]
fn expected_callback_extracts_loopback_redirect() {
    let auth_url = "https://chat.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A6543%2Fauth%2Fcallback";
    let expected = expected_callback_from_auth_url(auth_url);
    assert_eq!(
        expected.as_deref(),
        Some("http://localhost:6543/auth/callback")
    );
}

#[test]
fn callback_validation_rejects_non_loopback_host() {
    let err = validate_callback_url(
        "http://example.com:1234/auth/callback?code=abc",
        Some("http://localhost:1234/auth/callback"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("loopback"));
}

#[test]
fn callback_validation_accepts_expected_port_path_and_query() {
    validate_callback_url(
        "http://127.0.0.1:4321/auth/callback?code=abc&state=def",
        Some("http://localhost:4321/auth/callback"),
    )
    .expect("callback URL should validate");
}
