use ctx_desktop_ipc::DesktopOpenExternalUrlReq;
use tauri_plugin_shell::ShellExt;
use url::Url;

#[tauri::command]
#[allow(deprecated)]
pub(super) fn desktop_open_external_url(
    app: tauri::AppHandle,
    req: DesktopOpenExternalUrlReq,
) -> Result<(), String> {
    let url = validate_external_url(&req.url)?;
    app.shell()
        .open(url.as_str(), None)
        .map_err(|err| format!("failed to open external URL: {err}"))
}

fn validate_external_url(raw: &str) -> Result<Url, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("external URL is required".to_string());
    }
    if value.chars().any(char::is_control) {
        return Err("external URL contains invalid control characters".to_string());
    }

    let url = Url::parse(value).map_err(|_| "external URL must be absolute".to_string())?;
    match url.scheme() {
        "http" | "https" => validate_http_url(url),
        "mailto" | "tel" => validate_handler_url(url),
        _ => Err("external URL scheme is not allowed".to_string()),
    }
}

fn validate_http_url(url: Url) -> Result<Url, String> {
    if url.host_str().is_none() {
        return Err("external HTTP URL requires a host".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("external HTTP URL credentials are not allowed".to_string());
    }
    Ok(url)
}

fn validate_handler_url(url: Url) -> Result<Url, String> {
    if url.path().trim().is_empty() {
        return Err("external handler URL requires a target".to_string());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::validate_external_url;

    #[test]
    fn desktop_external_url_allows_expected_schemes() {
        let cases = [
            "https://example.com/docs",
            "http://localhost:3717/health",
            "mailto:security@example.com",
            "tel:+15555550123",
        ];
        for case in cases {
            assert!(validate_external_url(case).is_ok(), "{case}");
        }
    }

    #[test]
    fn desktop_external_url_rejects_unexpected_authority() {
        let cases = [
            "",
            "   ",
            "/relative/path",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:pass@example.com",
            "http://",
            "mailto:",
            "tel:",
            "https://example.com/\nnext",
        ];
        for case in cases {
            assert!(validate_external_url(case).is_err(), "{case}");
        }
    }
}
