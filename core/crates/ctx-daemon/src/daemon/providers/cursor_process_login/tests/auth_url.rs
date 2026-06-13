use crate::daemon::providers::cursor_process_login::auth_url::extract_auth_url;

#[test]
fn cursor_login_extract_auth_url_reconstructs_wrapped_url_lines() {
    let wrapped =
        "Open this URL: https://cursor.com/login/device?redirect_uri=http%3A%2F%2Flocalhost%3A\n64111%2Fauth%2Fcallback&state=abc";
    assert_eq!(
        extract_auth_url(wrapped).as_deref(),
        Some(
            "https://cursor.com/login/device?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fauth%2Fcallback&state=abc"
        )
    );
}

#[test]
fn cursor_login_extract_auth_url_reconstructs_wrapped_scheme_prefix() {
    let wrapped =
        "Open this URL: ht\ntps://cursor.com/login/device?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fauth%2Fcallback&state=abc";
    assert_eq!(
        extract_auth_url(wrapped).as_deref(),
        Some(
            "https://cursor.com/login/device?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fauth%2Fcallback&state=abc"
        )
    );
}

#[test]
fn cursor_login_extract_auth_url_strips_ansi_and_osc8_hyperlink() {
    let url = "https://cursor.com/login/device?redirect_uri=http%3A%2F%2Flocalhost%3A64111%2Fcallback&state=abc";
    let raw = format!("\u{1b}[90mOpen URL:\u{1b}[0m \u{1b}]8;;{url}\u{7}Sign in\u{1b}]8;;\u{7}\r");
    assert_eq!(extract_auth_url(&raw).as_deref(), Some(url));
}
