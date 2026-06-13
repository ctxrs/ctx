use super::*;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use tauri_plugin_updater::{verify_signature, UpdaterExt};

static STAGED_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn clear_staged_update_files(meta_path: &Path, bytes_path: &Path) -> Result<(), String> {
    if meta_path.exists() {
        std::fs::remove_file(meta_path).map_err(|e| {
            format!(
                "clearing staged update metadata '{}': {e}",
                meta_path.display()
            )
        })?;
    }
    if bytes_path.exists() {
        std::fs::remove_file(bytes_path).map_err(|e| {
            format!(
                "clearing staged update bytes '{}': {e}",
                bytes_path.display()
            )
        })?;
    }
    Ok(())
}

fn staged_update_meta_is_valid(meta: &DesktopStagedUpdateMeta) -> bool {
    !meta.version.trim().is_empty()
        && !meta.target.trim().is_empty()
        && !meta.endpoint.trim().is_empty()
        && !meta.channel.trim().is_empty()
        && !meta.download_url.trim().is_empty()
        && !meta.signature.trim().is_empty()
        && is_valid_sha256(&meta.sha256)
        && meta.size_bytes > 0
}

fn is_valid_sha256(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn temp_path_for(path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_else(|| "desktop_update_staged".into());
    let sequence = STAGED_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    file_name.push(format!(".tmp-{}-{sequence}-{suffix}", std::process::id()));
    path.with_file_name(file_name)
}

fn write_file_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "creating staged update directory '{}': {e}",
                parent.display()
            )
        })?;
    }
    let tmp_path = temp_path_for(path, "write");
    {
        let mut file = std::fs::File::create(&tmp_path).map_err(|e| {
            format!(
                "creating staged update temp file '{}': {e}",
                tmp_path.display()
            )
        })?;
        file.write_all(bytes).map_err(|e| {
            format!(
                "writing staged update temp file '{}': {e}",
                tmp_path.display()
            )
        })?;
        file.sync_all().map_err(|e| {
            format!(
                "syncing staged update temp file '{}': {e}",
                tmp_path.display()
            )
        })?;
    }
    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!(
            "committing staged update file '{}' -> '{}': {e}",
            tmp_path.display(),
            path.display()
        )
    })
}

pub(super) fn read_staged_update_meta(
    meta_path: &Path,
    bytes_path: &Path,
) -> Result<Option<DesktopStagedUpdateMeta>, String> {
    if !meta_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(meta_path).map_err(|e| {
        format!(
            "reading staged update metadata '{}': {e}",
            meta_path.display()
        )
    })?;
    let parsed = match serde_json::from_str::<DesktopStagedUpdateMeta>(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!(
                "warn: clearing corrupt staged update metadata '{}': {err}",
                meta_path.display()
            );
            clear_staged_update_files(meta_path, bytes_path).map_err(|clear_err| {
                format!(
                    "parsing staged update metadata '{}': {err}; clearing corrupt staged update state: {clear_err}",
                    meta_path.display()
                )
            })?;
            return Ok(None);
        }
    };
    if !staged_update_meta_is_valid(&parsed) {
        clear_staged_update_files(meta_path, bytes_path)?;
        return Ok(None);
    }
    Ok(Some(parsed))
}

pub(super) fn write_staged_update_files(
    meta_path: &Path,
    bytes_path: &Path,
    meta: &DesktopStagedUpdateMeta,
    bytes: &[u8],
) -> Result<(), String> {
    let mut meta = meta.clone();
    meta.size_bytes = bytes.len();
    meta.sha256 = sha256_hex(bytes);
    write_file_atomically(bytes_path, bytes)?;
    let encoded = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("encoding staged update metadata: {e}"))?;
    write_file_atomically(meta_path, format!("{encoded}\n").as_bytes())
}

pub(super) fn read_staged_update_meta_for_app(
    app: &tauri::AppHandle,
) -> Result<Option<DesktopStagedUpdateMeta>, String> {
    let path = staged_meta_path_for_app(app)?;
    let bytes_path = staged_bytes_path_for_app(app)?;
    read_staged_update_meta(&path, &bytes_path)
}

fn write_staged_update_for_app(
    app: &tauri::AppHandle,
    meta: &DesktopStagedUpdateMeta,
    bytes: &[u8],
) -> Result<(), String> {
    let bytes_path = staged_bytes_path_for_app(app)?;
    let meta_path = staged_meta_path_for_app(app)?;
    write_staged_update_files(&meta_path, &bytes_path, meta, bytes)
}

pub(super) fn clear_staged_update_for_app(app: &tauri::AppHandle) -> Result<(), String> {
    let meta_path = staged_meta_path_for_app(app)?;
    let bytes_path = staged_bytes_path_for_app(app)?;
    clear_staged_update_files(&meta_path, &bytes_path)
}

pub(super) fn clear_staged_update_if_current_version_is_new_enough(
    app: &tauri::AppHandle,
    current_version: &str,
) -> Result<(), String> {
    let Some(meta) = read_staged_update_meta_for_app(app)? else {
        return Ok(());
    };
    if support::version_is_at_or_above(current_version, &meta.version) {
        clear_staged_update_for_app(app)?;
    }
    Ok(())
}

pub(super) fn has_matching_staged_update(
    app: &tauri::AppHandle,
    channel: &str,
    expected_version: &str,
    config: &DesktopNativeUpdaterConfig,
    expected_signature: &str,
    expected_download_url: &str,
) -> Result<bool, String> {
    let meta_path = staged_meta_path_for_app(app)?;
    let bytes_path = staged_bytes_path_for_app(app)?;
    has_matching_staged_update_paths(
        &meta_path,
        &bytes_path,
        channel,
        expected_version,
        config,
        expected_signature,
        expected_download_url,
    )
}

pub(super) fn has_matching_staged_update_paths(
    meta_path: &Path,
    bytes_path: &Path,
    channel: &str,
    expected_version: &str,
    config: &DesktopNativeUpdaterConfig,
    expected_signature: &str,
    expected_download_url: &str,
) -> Result<bool, String> {
    read_verified_staged_update_bytes_if_matching_paths(
        meta_path,
        bytes_path,
        channel,
        expected_version,
        config,
        expected_signature,
        expected_download_url,
        None,
    )
    .map(|bytes| bytes.is_some())
}

pub(super) fn read_verified_staged_update_bytes_if_matching(
    app: &tauri::AppHandle,
    channel: &str,
    expected_version: &str,
    config: &DesktopNativeUpdaterConfig,
    expected_signature: &str,
    expected_download_url: &str,
    pubkey: &str,
) -> Result<Option<Vec<u8>>, String> {
    let meta_path = staged_meta_path_for_app(app)?;
    let bytes_path = staged_bytes_path_for_app(app)?;
    read_verified_staged_update_bytes_if_matching_paths(
        &meta_path,
        &bytes_path,
        channel,
        expected_version,
        config,
        expected_signature,
        expected_download_url,
        Some(pubkey),
    )
}

pub(super) fn read_verified_staged_update_bytes_if_matching_paths(
    meta_path: &Path,
    bytes_path: &Path,
    channel: &str,
    expected_version: &str,
    config: &DesktopNativeUpdaterConfig,
    expected_signature: &str,
    expected_download_url: &str,
    pubkey: Option<&str>,
) -> Result<Option<Vec<u8>>, String> {
    let Some(meta) = read_staged_update_meta(meta_path, bytes_path)? else {
        return Ok(None);
    };
    let matches_expected = meta.version.trim() == expected_version.trim()
        && meta.target.trim() == config.target.trim()
        && meta.endpoint.trim() == config.endpoint.trim()
        && meta.channel.trim() == channel.trim()
        && meta.signature.trim() == expected_signature.trim()
        && meta.download_url.trim() == expected_download_url.trim();
    if !matches_expected {
        clear_staged_update_files(meta_path, bytes_path)?;
        return Ok(None);
    }
    if !bytes_path.exists() {
        clear_staged_update_files(meta_path, bytes_path)?;
        return Ok(None);
    }
    let bytes = std::fs::read(bytes_path).map_err(|e| {
        format!(
            "reading staged update bytes '{}': {e}",
            bytes_path.display()
        )
    })?;
    if bytes.is_empty() || bytes.len() != meta.size_bytes {
        clear_staged_update_files(meta_path, bytes_path)?;
        return Ok(None);
    }
    let digest = sha256_hex(&bytes);
    if !digest.eq_ignore_ascii_case(meta.sha256.trim()) {
        clear_staged_update_files(meta_path, bytes_path)?;
        return Ok(None);
    }
    if let Some(pubkey) = pubkey {
        if let Err(err) = verify_signature(&bytes, &meta.signature, pubkey) {
            clear_staged_update_files(meta_path, bytes_path)?;
            return Err(format!(
                "staged update signature verification failed for '{}': {err}",
                bytes_path.display()
            ));
        }
    }
    Ok(Some(bytes))
}

pub(super) async fn stage_update_in_background(
    app: tauri::AppHandle,
    channel: &str,
) -> Result<(), String> {
    let current_version = app.package_info().version.to_string();
    let mut attempt = attempts::begin_update_attempt(channel, &current_version);

    let config = support::resolve_native_updater_config(channel)?;
    let Some(pubkey) = config.pubkey.as_deref() else {
        clear_staged_update_for_app(&app)?;
        attempts::persist_attempt_success_best_effort(&app, &mut attempt);
        return Ok(());
    };
    let endpoint_url = support::endpoint_with_download_id(&config.endpoint, None)?;
    let build_stage = attempts::begin_attempt_stage(&mut attempt, "build");
    let updater = app
        .updater_builder()
        .target(config.target.clone())
        .pubkey(pubkey)
        .endpoints(vec![endpoint_url])
        .map_err(|e| {
            let err = support::updater_stage_error("build", e);
            attempts::fail_attempt_stage(&mut attempt, build_stage, "build", &err);
            attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
        })?
        .build()
        .map_err(|e| {
            let err = support::updater_stage_error("build", e);
            attempts::fail_attempt_stage(&mut attempt, build_stage, "build", &err);
            attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
        })?;
    attempts::complete_attempt_stage(&mut attempt, build_stage);

    let check_stage = attempts::begin_attempt_stage(&mut attempt, "check");
    let Some(update) = updater.check().await.map_err(|e| {
        let err = support::updater_stage_error("check", e);
        attempts::fail_attempt_stage(&mut attempt, check_stage, "check", &err);
        attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
    })?
    else {
        attempts::complete_attempt_stage(&mut attempt, check_stage);
        clear_staged_update_for_app(&app)?;
        attempts::persist_attempt_success_best_effort(&app, &mut attempt);
        return Ok(());
    };
    attempts::complete_attempt_stage(&mut attempt, check_stage);
    let latest_version = update.version.clone();
    attempt.target_version = Some(latest_version.clone());

    let verify_stage = attempts::begin_attempt_stage(&mut attempt, "verify");
    if !support::version_is_strictly_newer(&latest_version, &current_version) {
        attempts::complete_attempt_stage(&mut attempt, verify_stage);
        clear_staged_update_for_app(&app)?;
        attempts::persist_attempt_success_best_effort(&app, &mut attempt);
        return Ok(());
    }
    attempts::complete_attempt_stage(&mut attempt, verify_stage);

    let download_stage = attempts::begin_attempt_stage(&mut attempt, "download");
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| {
        let err = support::updater_stage_error("download", e);
        attempts::fail_attempt_stage(&mut attempt, download_stage, "download", &err);
        attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
    })?;
    attempts::complete_attempt_stage(&mut attempt, download_stage);

    let marker_stage = attempts::begin_attempt_stage(&mut attempt, "marker");
    let meta = DesktopStagedUpdateMeta {
        version: latest_version,
        target: config.target,
        endpoint: config.endpoint,
        download_url: update.download_url.to_string(),
        signature: update.signature.clone(),
        sha256: String::new(),
        channel: channel.to_string(),
        downloaded_at_ms: now_ms(),
        size_bytes: bytes.len(),
    };
    write_staged_update_for_app(&app, &meta, &bytes).map_err(|e| {
        let err = support::updater_stage_error("marker_write", e);
        attempts::fail_attempt_stage(&mut attempt, marker_stage, "marker_write", &err);
        attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
    })?;
    attempts::complete_attempt_stage(&mut attempt, marker_stage);
    attempts::persist_attempt_success_best_effort(&app, &mut attempt);
    Ok(())
}
