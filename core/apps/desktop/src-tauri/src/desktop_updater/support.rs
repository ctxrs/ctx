use super::*;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use url::Url;

pub(super) const MISSING_EMBEDDED_UPDATER_PUBKEY_MESSAGE: &str =
    "native updater is not configured (missing embedded updater public key)";
pub(super) const MISSING_EMBEDDED_UPDATER_PUBKEY_MESSAGE_SENTENCE: &str =
    "Native updater is not configured (missing embedded updater public key).";
pub(super) const REMOTE_BOOTSTRAP_INSECURE_LOOPBACK_UPDATER_ENV: &str =
    "CTX_DESKTOP_ALLOW_INSECURE_LOCAL_UPDATER_FOR_REMOTE_BOOTSTRAP";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedVersion {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

fn parse_semver_like(value: &str) -> Option<ParsedVersion> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let (without_build, _) = normalized.split_once('+').unwrap_or((normalized, ""));
    let (core, pre) = without_build
        .split_once('-')
        .map(|(lhs, rhs)| (lhs, Some(rhs.to_string())))
        .unwrap_or((without_build, None));
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts
        .next()
        .map(|v| v.parse::<u64>().ok())
        .unwrap_or(Some(0))?;
    let patch = parts
        .next()
        .map(|v| v.parse::<u64>().ok())
        .unwrap_or(Some(0))?;
    if parts.next().is_some() {
        return None;
    }
    Some(ParsedVersion {
        major,
        minor,
        patch,
        pre,
    })
}

fn compare_prerelease_segments(lhs: &str, rhs: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    for (left_seg, right_seg) in lhs.split('.').zip(rhs.split('.')) {
        let left_num = left_seg.parse::<u64>().ok();
        let right_num = right_seg.parse::<u64>().ok();
        let ord = match (left_num, right_num) {
            (Some(l), Some(r)) => l.cmp(&r),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => left_seg.cmp(right_seg),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    lhs.split('.').count().cmp(&rhs.split('.').count())
}

fn compare_semver_like(lhs: &ParsedVersion, rhs: &ParsedVersion) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let core_cmp = (lhs.major, lhs.minor, lhs.patch).cmp(&(rhs.major, rhs.minor, rhs.patch));
    if core_cmp != Ordering::Equal {
        return core_cmp;
    }
    match (&lhs.pre, &rhs.pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(l), Some(r)) => compare_prerelease_segments(l, r),
    }
}

pub(super) fn version_is_strictly_newer(candidate: &str, current: &str) -> bool {
    let Some(candidate_ver) = parse_semver_like(candidate) else {
        return false;
    };
    let Some(current_ver) = parse_semver_like(current) else {
        return false;
    };
    compare_semver_like(&candidate_ver, &current_ver).is_gt()
}

pub(super) fn version_is_at_or_above(current: &str, required: &str) -> bool {
    match (parse_semver_like(current), parse_semver_like(required)) {
        (Some(current_ver), Some(required_ver)) => {
            !compare_semver_like(&current_ver, &required_ver).is_lt()
        }
        _ => current.trim() == required.trim(),
    }
}

pub(super) fn normalize_latest_version(
    current_version: &str,
    latest_from_feed: Option<&str>,
    pending_restart_version: Option<&str>,
) -> Option<String> {
    if let Some(pending) = pending_restart_version {
        let trimmed = pending.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let candidate = latest_from_feed?.trim();
    if candidate.is_empty() {
        return None;
    }
    if version_is_strictly_newer(candidate, current_version) {
        return Some(candidate.to_string());
    }
    None
}

pub(super) fn normalize_download_id(raw: Option<&str>) -> Option<String> {
    let candidate = raw?.trim();
    if candidate.is_empty() || candidate.len() > 64 {
        return None;
    }
    if !candidate
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | ':'))
    {
        return None;
    }
    Some(candidate.to_string())
}

pub(super) fn endpoint_with_download_id(
    endpoint: &str,
    download_id: Option<&str>,
) -> Result<Url, String> {
    let mut parsed = Url::parse(endpoint).map_err(|e| format!("invalid update endpoint: {e}"))?;
    if let Some(download_id) = download_id {
        parsed
            .query_pairs_mut()
            .append_pair("ctx_download_id", download_id);
    }
    Ok(parsed)
}

pub(super) fn resolve_native_updater_config(
    channel: &str,
) -> Result<DesktopNativeUpdaterConfig, String> {
    let target = desktop_platform_key()?;
    let base_url = default_download_base_url();
    let endpoint_default = format!(
        "{}/releases/{}/latest-tauri.json",
        base_url.trim_end_matches('/'),
        channel
    );
    let endpoint = std::env::var("CTX_DESKTOP_UPDATER_ENDPOINT")
        .ok()
        .and_then(|raw| expand_updater_endpoint_template(&raw, channel))
        .unwrap_or(endpoint_default);
    let runtime_override = if cfg!(debug_assertions) {
        std::env::var("CTX_DESKTOP_UPDATER_PUBKEY").ok()
    } else {
        None
    };
    let pubkey = resolve_updater_pubkey(
        runtime_override,
        option_env!("CTX_DESKTOP_EMBEDDED_UPDATER_PUBKEY_B64"),
    );
    Ok(DesktopNativeUpdaterConfig {
        target: target.to_string(),
        endpoint,
        pubkey,
    })
}

pub(super) fn normalize_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn should_allow_remote_bootstrap_insecure_loopback_updater(
    explicit_override: bool,
    endpoint: &str,
) -> bool {
    if !explicit_override {
        return false;
    }
    let Ok(parsed) = Url::parse(endpoint) else {
        return false;
    };
    if parsed.scheme() != "http" {
        return false;
    }
    matches!(
        parsed.host_str().map(|value| value.trim()),
        Some("127.0.0.1") | Some("localhost") | Some("::1")
    )
}

pub(super) fn remote_bootstrap_insecure_loopback_override_enabled() -> bool {
    remote_bootstrap_insecure_loopback_override_enabled_for_build(
        cfg!(feature = "automation"),
        cfg!(debug_assertions),
    )
}

pub(super) fn remote_bootstrap_insecure_loopback_override_enabled_for_build(
    is_automation_build: bool,
    is_debug_build: bool,
) -> bool {
    if is_automation_build {
        return true;
    }
    if !is_debug_build {
        return false;
    }
    std::env::var(REMOTE_BOOTSTRAP_INSECURE_LOOPBACK_UPDATER_ENV)
        .ok()
        .as_deref()
        == Some("1")
}

pub(super) fn updater_stage_error(stage: &str, err: impl std::fmt::Display) -> String {
    format!("native updater {stage} failed: {err}")
}

pub(super) fn resolve_updater_pubkey(
    runtime_value: Option<String>,
    build_value: Option<&str>,
) -> Option<String> {
    let runtime = runtime_value
        .as_deref()
        .and_then(normalize_nonempty)
        .and_then(normalize_updater_pubkey);
    if runtime.is_some() {
        return runtime;
    }
    build_value
        .and_then(normalize_nonempty)
        .and_then(normalize_updater_pubkey)
}

fn normalize_updater_pubkey(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(normalized_plain) = normalize_minisign_pubkey_text(trimmed) {
        return Some(BASE64_STANDARD.encode(normalized_plain.as_bytes()));
    }
    let compact: String = trimmed
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    if let Some(decoded_plain) = decode_base64_minisign_pubkey(&compact) {
        return Some(BASE64_STANDARD.encode(decoded_plain.as_bytes()));
    }
    None
}

fn decode_base64_minisign_pubkey(encoded: &str) -> Option<String> {
    let decoded_bytes = BASE64_STANDARD.decode(encoded.as_bytes()).ok()?;
    let decoded_text = String::from_utf8(decoded_bytes).ok()?;
    normalize_minisign_pubkey_text(&decoded_text)
}

fn normalize_minisign_pubkey_text(raw: &str) -> Option<String> {
    let normalized = raw.replace("\r\n", "\n");
    let mut lines = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let header = lines.next()?;
    if !header.starts_with("untrusted comment: minisign public key:") {
        return None;
    }
    let key_line = lines.next()?;
    if key_line.is_empty() || lines.next().is_some() {
        return None;
    }
    Some(format!("{header}\n{key_line}\n"))
}

fn default_download_base_url() -> String {
    std::env::var("CTX_DOWNLOAD_BASE_URL")
        .unwrap_or_else(|_| "https://api.ctx.rs/functions/v1".to_string())
}

pub(super) fn expand_updater_endpoint_template(raw: &str, channel: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.replace("{channel}", channel))
}

pub(super) fn linux_installer_target_suffix(
    appimage_env: Option<&str>,
    current_exe: Option<&std::path::Path>,
) -> &'static str {
    if appimage_env.is_some_and(|value| !value.trim().is_empty()) {
        return "appimage";
    }
    if current_exe.is_some_and(|path| {
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("AppImage"))
    }) {
        return "appimage";
    }
    "appimage"
}

pub(super) fn desktop_platform_key_for(
    os: &str,
    arch: &str,
    linux_installer_suffix: &str,
) -> Result<String, String> {
    let linux_suffix = linux_installer_suffix.trim();
    match (os, arch) {
        ("linux", "x86_64") if !linux_suffix.is_empty() => Ok(format!("linux-x64-{linux_suffix}")),
        ("linux", "aarch64") if !linux_suffix.is_empty() => {
            Ok(format!("linux-arm64-{linux_suffix}"))
        }
        ("macos", "x86_64") => Ok("macos-x64".to_string()),
        ("macos", "aarch64") => Ok("macos-arm64".to_string()),
        ("windows", "x86_64") => Ok("windows-x64".to_string()),
        _ => Err(format!(
            "unsupported platform for desktop updater: {os}/{arch}"
        )),
    }
}

pub(super) fn desktop_platform_key() -> Result<String, String> {
    let installer_suffix = linux_installer_target_suffix(
        std::env::var("APPIMAGE").ok().as_deref(),
        std::env::current_exe().ok().as_deref(),
    );
    desktop_platform_key_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        installer_suffix,
    )
}
