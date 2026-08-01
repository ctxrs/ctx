use std::{
    fs::{self, File},
    io::Read as _,
    path::{Component, Path},
};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{ControlManifest, MAX_ERROR_MESSAGE_BYTES, MAX_URL_BYTES};

pub(super) fn sort_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, sort_json(value));
            }
            Value::Object(sorted)
        }
        value => value,
    }
}

pub(super) fn expected_operation(manifest: &ControlManifest) -> Option<&'static str> {
    [
        (manifest.lifecycle.setup.is_some(), "lifecycle.setup"),
        (manifest.lifecycle.manage.is_some(), "lifecycle.manage"),
        (manifest.referral.create.is_some(), "referral.create"),
        (manifest.referral.status.is_some(), "referral.status"),
        (manifest.referral.payout.is_some(), "referral.payout"),
    ]
    .into_iter()
    .find_map(|(present, operation)| present.then_some(operation))
}

pub(super) fn validate_identifier(value: &str, maximum: usize, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        bail!("invalid_request: Pro test {label} is invalid");
    }
    Ok(())
}

pub(super) fn validate_error(code: &str, message: &str) -> Result<()> {
    const ALLOWED: &[&str] = &[
        "anonymous_trial_already_consumed",
        "anonymous_trial_identity_ambiguous",
        "anonymous_trial_installation_limit",
        "artifact_download_failed",
        "authentication_denied",
        "authentication_expired",
        "authentication_failed",
        "authentication_required",
        "checkout_expired",
        "checkout_timeout",
        "commercial_access_locked",
        "commercial_identity_conflict",
        "commercial_unavailable",
        "entitlement_expired",
        "helper_crashed",
        "helper_timeout",
        "invalid_request",
        "invalid_response",
        "key_store_locked",
        "key_store_unavailable",
        "pro_not_installed",
        "protocol_mismatch",
        "rate_limited",
        "referral_codename_conflict",
        "referral_not_eligible",
        "referral_not_found",
        "referral_payout_unavailable",
        "referral_unavailable",
        "service_unavailable",
    ];
    if !ALLOWED.contains(&code) {
        bail!("invalid_request: Pro test script uses an unknown error code");
    }
    if message.len() > MAX_ERROR_MESSAGE_BYTES
        || message
            .bytes()
            .any(|byte| !(byte == b'\t' || (b' '..=b'~').contains(&byte)))
    {
        bail!("invalid_request: Pro test error message is invalid");
    }
    Ok(())
}

pub(super) fn validate_fixture_url(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_URL_BYTES {
        bail!("invalid_request: Pro test {label} is outside allowed bounds");
    }
    let parsed = url::Url::parse(value)
        .with_context(|| format!("invalid_request: Pro test {label} is invalid"))?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https"
        || host.is_empty()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !(host.ends_with(".example.test") || host.ends_with(".invalid"))
    {
        bail!("invalid_request: Pro test {label} must use a non-live HTTPS fixture host");
    }
    Ok(())
}

pub(super) fn validate_receipt_name(value: &str) -> Result<()> {
    validate_relative_path(value, "receipt path")?;
    if Path::new(value).components().count() != 1 || !value.ends_with(".json") {
        bail!("invalid_request: Pro test receipt must be one JSON file in the observer root");
    }
    Ok(())
}

pub(super) fn validate_relative_path(value: &str, label: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > MAX_URL_BYTES
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("invalid_request: Pro test {label} is not a safe relative path");
    }
    Ok(())
}

pub(super) fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid_request: Pro test {label} must be lowercase hexadecimal");
    }
    Ok(())
}

pub(super) fn verify_bounded_file_in_root(
    root: &Path,
    path: &Path,
    maximum: u64,
    expected_sha256: &str,
    executable: bool,
) -> Result<()> {
    if path.parent().is_none() || !path.starts_with(root) {
        bail!("invalid_request: Pro test file escaped its observer root");
    }
    let relative = path
        .strip_prefix(root)
        .context("invalid_request: resolve Pro test file path")?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("invalid_request: Pro test file path is invalid");
        };
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).context("invalid_request: inspect Pro test file")?;
        if metadata.file_type().is_symlink() {
            bail!("invalid_request: Pro test file path contains a symlink");
        }
        if current != path && !metadata.is_dir() {
            bail!("invalid_request: Pro test file parent is not a directory");
        }
    }
    let metadata = fs::symlink_metadata(path).context("invalid_request: inspect Pro test file")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum {
        bail!("invalid_request: Pro test file size or type is invalid");
    }
    if executable {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o111 == 0 {
                bail!("invalid_request: Pro test helper is not executable");
            }
        }
    }
    let mut file = File::open(path).context("invalid_request: open Pro test file")?;
    let mut digest = Sha256::new();
    let copied = std::io::copy(
        &mut std::io::Read::by_ref(&mut file).take(maximum.saturating_add(1)),
        &mut digest,
    )
    .context("invalid_request: hash Pro test file")?;
    if copied != metadata.len() || format!("{:x}", digest.finalize()) != expected_sha256 {
        bail!("invalid_request: Pro test file digest does not match its manifest");
    }
    Ok(())
}
