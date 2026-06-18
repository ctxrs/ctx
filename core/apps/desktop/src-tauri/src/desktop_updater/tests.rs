use super::*;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn remote_bootstrap_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("remote bootstrap env lock poisoned")
}

fn temp_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!(
        "ctx-desktop-updater-{label}-{}-{now}.json",
        std::process::id()
    ));
    path
}

fn staged_paths(label: &str) -> (PathBuf, PathBuf) {
    let meta_path = temp_path(&format!("{label}-meta"));
    let mut bytes_path = temp_path(&format!("{label}-bytes"));
    bytes_path.set_extension("bin");
    (meta_path, bytes_path)
}

fn staged_meta(version: &str) -> DesktopStagedUpdateMeta {
    DesktopStagedUpdateMeta {
        version: version.to_string(),
        target: "macos-arm64".to_string(),
        endpoint: "https://example.test/releases/stable/latest-tauri.json".to_string(),
        channel: "stable".to_string(),
        download_url: "https://example.test/download/stable/1.2.4/ctx.app.tar.gz".to_string(),
        signature: "sig".to_string(),
        sha256: String::new(),
        downloaded_at_ms: 1,
        size_bytes: 7,
    }
}

fn staged_download_url() -> &'static str {
    "https://example.test/download/stable/1.2.4/ctx.app.tar.gz"
}

fn staged_config() -> DesktopNativeUpdaterConfig {
    DesktopNativeUpdaterConfig {
        target: "macos-arm64".to_string(),
        endpoint: "https://example.test/releases/stable/latest-tauri.json".to_string(),
        pubkey: Some("pubkey".to_string()),
    }
}

#[test]
fn desktop_platform_key_is_known_for_current_target() {
    let key = support::desktop_platform_key();
    assert!(
        key.is_ok(),
        "current target should map to known update platform key: {key:?}"
    );
}

#[test]
fn linux_installer_target_suffix_prefers_appimage_env() {
    assert_eq!(
        support::linux_installer_target_suffix(Some("/tmp/ctx.AppImage"), None),
        "appimage"
    );
}

#[test]
fn linux_installer_target_suffix_uses_appimage_extension_when_env_missing() {
    assert_eq!(
        support::linux_installer_target_suffix(
            None,
            Some(PathBuf::from("/tmp/ctx.AppImage").as_path())
        ),
        "appimage"
    );
}

#[test]
fn linux_installer_target_suffix_defaults_to_appimage_without_extra_signal() {
    assert_eq!(
        support::linux_installer_target_suffix(None, None),
        "appimage"
    );
}

#[test]
fn desktop_platform_key_for_linux_targets_includes_installer_suffix() {
    assert_eq!(
        support::desktop_platform_key_for("linux", "x86_64", "appimage")
            .expect("linux x64 appimage target should resolve"),
        "linux-x64-appimage"
    );
    assert_eq!(
        support::desktop_platform_key_for("linux", "aarch64", "appimage")
            .expect("linux arm64 appimage target should resolve"),
        "linux-arm64-appimage"
    );
}

#[test]
fn resolve_native_updater_config_uses_base_defaults() {
    let cfg = support::resolve_native_updater_config("stable").expect("config should resolve");
    assert!(
        cfg.endpoint.ends_with("/releases/stable/latest-tauri.json"),
        "unexpected endpoint: {}",
        cfg.endpoint
    );
    assert!(
        cfg.pubkey.is_some(),
        "desktop updater config should embed the production updater pubkey"
    );
}

#[test]
fn expand_updater_endpoint_template_replaces_channel_placeholder() {
    let cfg = support::expand_updater_endpoint_template(
        "https://example.test/releases/{channel}/latest-tauri.json",
        "rc-2026.02.17",
    )
    .expect("template should expand");
    assert_eq!(
        cfg,
        "https://example.test/releases/rc-2026.02.17/latest-tauri.json"
    );
}

#[test]
fn endpoint_with_download_id_appends_query_param() {
    let url = support::endpoint_with_download_id(
        "https://example.test/releases/stable/latest-tauri.json",
        Some("abc-123"),
    )
    .expect("endpoint should parse");
    assert_eq!(
        url.as_str(),
        "https://example.test/releases/stable/latest-tauri.json?ctx_download_id=abc-123"
    );
}

#[test]
fn normalize_download_id_rejects_invalid_chars() {
    let value = support::normalize_download_id(Some("abc def"));
    assert!(value.is_none());
}

#[test]
fn resolve_updater_pubkey_prefers_runtime_value() {
    let runtime_raw =
        "untrusted comment: minisign public key: 0D503F73CDD77B9C\nRWSce9fNcz9QDfv7dghgOH/dIA0Txkgk8rB86J5s6I15e+NkpWjU3CFs\n";
    let build_raw =
        "untrusted comment: minisign public key: ABCDEF0123456789\nRWSce9fNcz9QAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";
    let key = support::resolve_updater_pubkey(Some(runtime_raw.to_string()), Some(build_raw))
        .expect("resolved key");
    let decoded = String::from_utf8(
        BASE64_STANDARD
            .decode(key.as_bytes())
            .expect("runtime key should decode as base64"),
    )
    .expect("runtime key should decode as utf8");
    assert_eq!(decoded, runtime_raw);
}

#[test]
fn resolve_updater_pubkey_decodes_base64_minisign_key() {
    let raw =
        "untrusted comment: minisign public key: 0D503F73CDD77B9C\nRWSce9fNcz9QDfv7dghgOH/dIA0Txkgk8rB86J5s6I15e+NkpWjU3CFs\n";
    let encoded = BASE64_STANDARD.encode(raw.as_bytes());
    let key = support::resolve_updater_pubkey(Some(encoded), None).expect("resolved key");
    assert_eq!(key, BASE64_STANDARD.encode(raw.as_bytes()));
}

#[test]
fn resolve_updater_pubkey_falls_back_to_build_value_when_runtime_invalid() {
    let build_raw =
        "untrusted comment: minisign public key: 0D503F73CDD77B9C\nRWSce9fNcz9QDfv7dghgOH/dIA0Txkgk8rB86J5s6I15e+NkpWjU3CFs\n";
    let key = support::resolve_updater_pubkey(Some("not-a-valid-key".to_string()), Some(build_raw))
        .expect("resolved key");
    assert_eq!(key, BASE64_STANDARD.encode(build_raw.as_bytes()));
}

#[test]
fn resolve_updater_pubkey_returns_none_when_both_sources_empty() {
    assert!(support::resolve_updater_pubkey(Some("".to_string()), Some("  ")).is_none());
}

#[test]
fn remote_bootstrap_insecure_loopback_override_allows_only_explicit_local_http() {
    assert!(
        support::should_allow_remote_bootstrap_insecure_loopback_updater(
            true,
            "http://127.0.0.1:43123/releases/stable/latest-tauri.json"
        )
    );
    assert!(
        support::should_allow_remote_bootstrap_insecure_loopback_updater(
            true,
            "http://localhost:43123/releases/stable/latest-tauri.json"
        )
    );
    assert!(
        !support::should_allow_remote_bootstrap_insecure_loopback_updater(
            false,
            "http://127.0.0.1:43123/releases/stable/latest-tauri.json"
        )
    );
    assert!(
        !support::should_allow_remote_bootstrap_insecure_loopback_updater(
            true,
            "https://127.0.0.1:43123/releases/stable/latest-tauri.json"
        )
    );
    assert!(
        !support::should_allow_remote_bootstrap_insecure_loopback_updater(
            true,
            "http://example.test/releases/stable/latest-tauri.json"
        )
    );
}

struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }

    fn unset(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.original.as_ref() {
            unsafe {
                std::env::set_var(self.key, value);
            }
        } else {
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[test]
fn remote_bootstrap_insecure_loopback_override_defaults_to_automation_builds() {
    let _env_lock = remote_bootstrap_env_lock();
    let _guard = EnvVarGuard::unset(support::REMOTE_BOOTSTRAP_INSECURE_LOOPBACK_UPDATER_ENV);
    assert_eq!(
        support::remote_bootstrap_insecure_loopback_override_enabled(),
        cfg!(feature = "automation")
    );
}

#[test]
fn remote_bootstrap_insecure_loopback_override_honors_explicit_env_in_debug_builds() {
    let _env_lock = remote_bootstrap_env_lock();
    let _guard = EnvVarGuard::set(support::REMOTE_BOOTSTRAP_INSECURE_LOOPBACK_UPDATER_ENV, "1");
    assert_eq!(
        support::remote_bootstrap_insecure_loopback_override_enabled(),
        cfg!(feature = "automation") || cfg!(debug_assertions)
    );
}

#[test]
fn remote_bootstrap_insecure_loopback_override_ignores_env_for_release_like_builds() {
    let _env_lock = remote_bootstrap_env_lock();
    let _guard = EnvVarGuard::set(support::REMOTE_BOOTSTRAP_INSECURE_LOOPBACK_UPDATER_ENV, "1");
    assert!(!support::remote_bootstrap_insecure_loopback_override_enabled_for_build(false, false));
    assert!(support::remote_bootstrap_insecure_loopback_override_enabled_for_build(false, true));
    assert!(support::remote_bootstrap_insecure_loopback_override_enabled_for_build(true, false));
}

#[test]
fn remote_bootstrap_freshness_check_bypasses_loopback_release_fixture_before_native_updater() {
    let _env_lock = remote_bootstrap_env_lock();
    let _override_guard =
        EnvVarGuard::set(support::REMOTE_BOOTSTRAP_INSECURE_LOOPBACK_UPDATER_ENV, "1");
    let _endpoint_guard = EnvVarGuard::set(
        "CTX_DESKTOP_UPDATER_ENDPOINT",
        "http://127.0.0.1:43123/releases/{channel}/latest-tauri.json",
    );
    assert!(recovery::should_bypass_remote_bootstrap_freshness_check(
        "stable"
    ));
}

#[test]
fn resolve_updater_pubkey_rejects_invalid_values() {
    assert!(support::resolve_updater_pubkey(Some("invalid".to_string()), None).is_none());
    assert!(support::resolve_updater_pubkey(Some("   ".to_string()), None).is_none());
}

#[test]
fn tauri_manifest_parser_accepts_absolute_updater_urls() {
    let manifest = r#"{
      "version":"1.2.3",
      "notes":"ctx 1.2.3",
      "pub_date":"2026-03-03T00:00:00Z",
      "platforms":{
        "macos-arm64":{
          "url":"https://api.ctx.rs/functions/v1/download/stable/1.2.3/ctx_1.2.3_macos-arm64_updater.app.tar.gz",
          "signature":"sig"
        }
      }
    }"#;
    let parsed = serde_json::from_str::<tauri_plugin_updater::RemoteRelease>(manifest);
    assert!(
        parsed.is_ok(),
        "absolute updater URLs should parse: {parsed:?}"
    );
}

#[test]
fn tauri_manifest_parser_rejects_relative_updater_urls() {
    let manifest = r#"{
      "version":"1.2.3",
      "notes":"ctx 1.2.3",
      "pub_date":"2026-03-03T00:00:00Z",
      "platforms":{
        "macos-arm64":{
          "url":"/download/stable/1.2.3/ctx_1.2.3_macos-arm64_updater.app.tar.gz",
          "signature":"sig"
        }
      }
    }"#;
    let parsed = serde_json::from_str::<tauri_plugin_updater::RemoteRelease>(manifest);
    assert!(parsed.is_err(), "relative updater URLs must be rejected");
}

#[test]
fn version_is_strictly_newer_respects_semver_ordering() {
    assert!(support::version_is_strictly_newer("1.2.0", "1.1.9"));
    assert!(!support::version_is_strictly_newer("1.2.0", "1.2.0"));
    assert!(!support::version_is_strictly_newer("1.2.0", "1.2.1"));
}

#[test]
fn normalize_latest_version_ignores_equal_or_older_candidates() {
    assert_eq!(
        support::normalize_latest_version("1.2.3", Some("1.2.3"), None),
        None
    );
    assert_eq!(
        support::normalize_latest_version("1.2.3", Some("1.2.2"), None),
        None
    );
    assert_eq!(
        support::normalize_latest_version("1.2.3", Some("1.2.4"), None),
        Some("1.2.4".to_string())
    );
}

#[test]
fn normalize_latest_version_prefers_pending_restart_version() {
    assert_eq!(
        support::normalize_latest_version("1.2.3", Some("1.3.0"), Some("1.2.9")),
        Some("1.2.9".to_string())
    );
}

#[test]
fn reconcile_restart_marker_clears_when_current_is_new_enough() {
    let path = temp_path("marker-clear");
    restart::write_restart_marker(&path, "2.0.0").expect("write marker");
    let marker = restart::reconcile_restart_marker(&path, "2.0.0").expect("reconcile");
    assert!(marker.is_none());
    assert!(!path.exists(), "marker file should be removed");
}

#[test]
fn reconcile_restart_marker_keeps_pending_when_current_is_older() {
    let path = temp_path("marker-pending");
    restart::write_restart_marker(&path, "2.0.0").expect("write marker");
    let marker = restart::reconcile_restart_marker(&path, "1.9.9").expect("reconcile");
    assert_eq!(marker.as_deref(), Some("2.0.0"));
    assert!(
        path.exists(),
        "marker file should remain while restart is pending"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn read_restart_marker_clears_blank_version_marker() {
    let path = temp_path("marker-blank");
    std::fs::write(&path, "{\n  \"version\": \"   \"\n}\n").expect("write marker");
    let marker = restart::read_restart_marker(&path).expect("read marker");
    assert!(marker.is_none(), "blank marker should be ignored");
    assert!(!path.exists(), "blank marker file should be removed");
}

#[test]
fn read_restart_marker_clears_corrupt_marker() {
    let path = temp_path("marker-corrupt");
    std::fs::write(&path, "{\n").expect("write corrupt marker");
    let marker = restart::read_restart_marker(&path).expect("read marker");
    assert!(marker.is_none(), "corrupt marker should be ignored");
    assert!(!path.exists(), "corrupt marker file should be removed");
}

#[test]
fn read_staged_update_meta_clears_corrupt_metadata() {
    let (meta_path, bytes_path) = staged_paths("corrupt-stage");
    std::fs::write(&meta_path, "{\n").expect("write corrupt staged metadata");
    std::fs::write(&bytes_path, b"payload").expect("write staged bytes");

    let meta =
        staged::read_staged_update_meta(&meta_path, &bytes_path).expect("read staged metadata");

    assert!(meta.is_none(), "corrupt staged metadata should be ignored");
    assert!(
        !meta_path.exists(),
        "corrupt staged metadata should be removed after detection"
    );
    assert!(
        !bytes_path.exists(),
        "paired staged bytes should be removed with corrupt metadata"
    );
}

#[test]
fn read_staged_update_meta_clears_invalid_required_fields() {
    let (meta_path, bytes_path) = staged_paths("invalid-stage");
    std::fs::write(
        &meta_path,
        "{\n  \"version\": \"   \",\n  \"target\": \"macos-arm64\",\n  \"endpoint\": \"https://example.test/releases/stable/latest-tauri.json\",\n  \"channel\": \"stable\",\n  \"downloaded_at_ms\": 1,\n  \"size_bytes\": 3\n}\n",
    )
    .expect("write invalid staged metadata");
    std::fs::write(&bytes_path, b"payload").expect("write staged bytes");

    let meta =
        staged::read_staged_update_meta(&meta_path, &bytes_path).expect("read staged metadata");

    assert!(
        meta.is_none(),
        "invalid staged metadata should be discarded"
    );
    assert!(
        !meta_path.exists(),
        "invalid metadata file should be removed"
    );
    assert!(
        !bytes_path.exists(),
        "invalid metadata should clear paired bytes"
    );
}

#[test]
fn has_matching_staged_update_clears_orphaned_metadata_when_bytes_are_missing() {
    let (meta_path, bytes_path) = staged_paths("orphaned-stage");
    let meta = staged_meta("1.2.4");
    staged::write_staged_update_files(&meta_path, &bytes_path, &meta, b"payload")
        .expect("write staged update");
    std::fs::remove_file(&bytes_path).expect("remove staged bytes");

    let has_match = staged::has_matching_staged_update_paths(
        &meta_path,
        &bytes_path,
        "stable",
        "1.2.4",
        &staged_config(),
        "sig",
        staged_download_url(),
    )
    .expect("check staged update");

    assert!(
        !has_match,
        "orphaned staged metadata must not be treated as ready"
    );
    assert!(
        !meta_path.exists(),
        "orphaned staged metadata should be cleared after detection"
    );
    assert!(
        !bytes_path.exists(),
        "missing staged bytes should stay absent after cleanup"
    );
}

#[test]
fn has_matching_staged_update_clears_stale_mismatched_version() {
    let (meta_path, bytes_path) = staged_paths("stale-mismatch");
    let meta = staged_meta("1.2.3");
    staged::write_staged_update_files(&meta_path, &bytes_path, &meta, b"payload")
        .expect("write staged update");

    let has_match = staged::has_matching_staged_update_paths(
        &meta_path,
        &bytes_path,
        "stable",
        "1.2.4",
        &staged_config(),
        "sig",
        staged_download_url(),
    )
    .expect("check staged update");

    assert!(
        !has_match,
        "stale staged payload must not match a newer target"
    );
    assert!(!meta_path.exists(), "stale metadata should be removed");
    assert!(
        !bytes_path.exists(),
        "stale payload bytes should be removed"
    );
}

#[test]
fn has_matching_staged_update_clears_mismatched_signature() {
    let (meta_path, bytes_path) = staged_paths("signature-mismatch");
    let meta = staged_meta("1.2.4");
    staged::write_staged_update_files(&meta_path, &bytes_path, &meta, b"payload")
        .expect("write staged update");

    let has_match = staged::has_matching_staged_update_paths(
        &meta_path,
        &bytes_path,
        "stable",
        "1.2.4",
        &staged_config(),
        "other-signature",
        staged_download_url(),
    )
    .expect("check staged update");

    assert!(
        !has_match,
        "staged payload must be tied to the current manifest signature"
    );
    assert!(
        !meta_path.exists(),
        "signature mismatch should clear staged metadata"
    );
    assert!(
        !bytes_path.exists(),
        "signature mismatch should clear staged bytes"
    );
}

#[test]
fn has_matching_staged_update_clears_mismatched_download_url() {
    let (meta_path, bytes_path) = staged_paths("download-url-mismatch");
    let meta = staged_meta("1.2.4");
    staged::write_staged_update_files(&meta_path, &bytes_path, &meta, b"payload")
        .expect("write staged update");

    let has_match = staged::has_matching_staged_update_paths(
        &meta_path,
        &bytes_path,
        "stable",
        "1.2.4",
        &staged_config(),
        "sig",
        "https://example.test/download/stable/1.2.4/other.tar.gz",
    )
    .expect("check staged update");

    assert!(
        !has_match,
        "staged payload must be tied to the current manifest download URL"
    );
    assert!(
        !meta_path.exists(),
        "download URL mismatch should clear staged metadata"
    );
    assert!(
        !bytes_path.exists(),
        "download URL mismatch should clear staged bytes"
    );
}

#[test]
fn read_staged_update_bytes_if_matching_clears_hash_mismatch() {
    let (meta_path, bytes_path) = staged_paths("hash-mismatch");
    let meta = staged_meta("1.2.4");
    staged::write_staged_update_files(&meta_path, &bytes_path, &meta, b"payload")
        .expect("write staged update");
    std::fs::write(&bytes_path, b"tampered").expect("tamper staged bytes");

    let bytes = staged::read_verified_staged_update_bytes_if_matching_paths(
        &meta_path,
        &bytes_path,
        "stable",
        "1.2.4",
        &staged_config(),
        "sig",
        staged_download_url(),
        None,
    )
    .expect("read staged update bytes");

    assert!(
        bytes.is_none(),
        "tampered staged payload must not be accepted"
    );
    assert!(
        !meta_path.exists(),
        "hash mismatch should clear staged metadata"
    );
    assert!(
        !bytes_path.exists(),
        "hash mismatch should clear staged bytes"
    );
}

#[test]
fn read_staged_update_bytes_if_matching_clears_empty_payload() {
    let (meta_path, bytes_path) = staged_paths("empty-stage");
    let meta = staged_meta("1.2.4");
    staged::write_staged_update_files(&meta_path, &bytes_path, &meta, &[])
        .expect("write empty staged update");

    let bytes = staged::read_verified_staged_update_bytes_if_matching_paths(
        &meta_path,
        &bytes_path,
        "stable",
        "1.2.4",
        &staged_config(),
        "sig",
        staged_download_url(),
        None,
    )
    .expect("read staged update bytes");

    assert!(bytes.is_none(), "empty staged payload should be discarded");
    assert!(
        !meta_path.exists(),
        "empty staged metadata should be removed after detection"
    );
    assert!(
        !bytes_path.exists(),
        "empty staged payload should be removed after detection"
    );
}

#[test]
fn desktop_update_phase_strings_are_stable() {
    assert_eq!(DesktopAppUpdatePhase::Idle.as_str(), "idle");
    assert_eq!(DesktopAppUpdatePhase::Staging.as_str(), "staging");
    assert_eq!(DesktopAppUpdatePhase::StagedReady.as_str(), "staged_ready");
    assert_eq!(
        DesktopAppUpdatePhase::RestartRequired.as_str(),
        "restart_required"
    );
    assert_eq!(DesktopAppUpdatePhase::Failed.as_str(), "failed");
}

#[test]
fn last_failed_stage_message_reports_latest_failure() {
    let attempt = DesktopUpdateAttempt {
        attempt_id: "attempt".to_string(),
        channel: "stable".to_string(),
        current_version: "0.4.9".to_string(),
        target_version: Some("0.4.10".to_string()),
        started_at_ms: 1,
        finished_at_ms: Some(2),
        result: DesktopUpdateAttemptResult::Failed,
        stages: vec![
            DesktopUpdateAttemptStage {
                stage: "check".to_string(),
                started_at_ms: 1,
                finished_at_ms: Some(1),
                result: DesktopUpdateAttemptResult::Failed,
                error_code: Some("check".to_string()),
                error_message: Some("first".to_string()),
            },
            DesktopUpdateAttemptStage {
                stage: "install".to_string(),
                started_at_ms: 2,
                finished_at_ms: Some(2),
                result: DesktopUpdateAttemptResult::Failed,
                error_code: Some("install".to_string()),
                error_message: Some("latest".to_string()),
            },
        ],
    };
    assert_eq!(
        attempts::last_failed_stage_message(&attempt),
        Some("latest")
    );
}

#[test]
fn desktop_update_attempt_json_round_trip() {
    let attempt = DesktopUpdateAttempt {
        attempt_id: "attempt-123".to_string(),
        channel: "stable".to_string(),
        current_version: "0.4.9".to_string(),
        target_version: Some("0.4.10".to_string()),
        started_at_ms: 100,
        finished_at_ms: Some(200),
        result: DesktopUpdateAttemptResult::Succeeded,
        stages: vec![DesktopUpdateAttemptStage {
            stage: "download".to_string(),
            started_at_ms: 120,
            finished_at_ms: Some(180),
            result: DesktopUpdateAttemptResult::Succeeded,
            error_code: None,
            error_message: None,
        }],
    };
    let encoded = serde_json::to_string(&attempt).expect("encode attempt");
    let decoded: DesktopUpdateAttempt = serde_json::from_str(&encoded).expect("decode attempt");
    assert_eq!(decoded.attempt_id, "attempt-123");
    assert_eq!(decoded.target_version.as_deref(), Some("0.4.10"));
    assert_eq!(decoded.stages.len(), 1);
    assert_eq!(decoded.stages[0].stage, "download");
}

#[test]
fn transaction_phase_helpers_keep_restart_and_staging_contracts_consistent() {
    let config = staged_config();

    let restart_required = transaction::restart_required_state(
        &config,
        "1.2.3".to_string(),
        Some("1.2.4".to_string()),
        Some("attempt-1".to_string()),
        None,
    );
    assert!(restart_required.restart_required);
    assert!(restart_required.available);
    assert!(!restart_required.staged);
    assert_eq!(
        restart_required.phase,
        DesktopAppUpdatePhase::RestartRequired.as_str()
    );
    assert_eq!(
        restart_required.message.as_deref(),
        Some(RESTART_READY_MESSAGE)
    );

    let staging = transaction::staging_state(
        &config,
        "1.2.3".to_string(),
        "1.2.4".to_string(),
        Some("attempt-2".to_string()),
        Some("download failed".to_string()),
    );
    assert!(!staging.restart_required);
    assert!(!staging.available);
    assert!(!staging.staged);
    assert_eq!(staging.phase, DesktopAppUpdatePhase::Staging.as_str());
    assert_eq!(
        staging.message.as_deref(),
        Some("Downloading update in background.")
    );
    assert_eq!(staging.last_error.as_deref(), Some("download failed"));

    let failed = transaction::failed_state(
        &config,
        "1.2.3".to_string(),
        "1.2.4".to_string(),
        Some("attempt-3".to_string()),
        Some("download failed".to_string()),
    );
    assert!(!failed.restart_required);
    assert!(failed.available);
    assert!(!failed.staged);
    assert_eq!(failed.phase, DesktopAppUpdatePhase::Failed.as_str());
    assert_eq!(
        failed.message.as_deref(),
        Some("Desktop update failed while installing in background.")
    );
    assert_eq!(failed.last_error.as_deref(), Some("download failed"));
}

#[test]
fn repeated_apply_short_circuits_when_restart_is_required() {
    let state = DesktopAppUpdateStateResp {
        configured: true,
        available: true,
        restart_required: true,
        phase: DesktopAppUpdatePhase::RestartRequired.as_str().to_string(),
        staged: false,
        current_version: "1.2.3".to_string(),
        latest_version: Some("1.2.4".to_string()),
        target: "macos-arm64".to_string(),
        endpoint: "https://example.test/releases/stable/latest-tauri.json".to_string(),
        message: Some(RESTART_READY_MESSAGE.to_string()),
        last_attempt_id: Some("attempt-1".to_string()),
        last_error: None,
    };

    let response = transaction::short_circuit_apply(&state)
        .expect("short circuit result")
        .expect("restart-required response");

    assert!(!response.applied, "repeat apply should not reinstall");
    assert!(
        response.needs_restart,
        "repeat apply should require restart"
    );
    assert!(
        !response.up_to_date,
        "repeat apply should not claim up-to-date"
    );
    assert_eq!(response.latest_version.as_deref(), Some("1.2.4"));
    assert_eq!(response.message, RESTART_READY_MESSAGE);
}

#[test]
fn apply_short_circuit_rejects_unconfigured_updater() {
    let state = DesktopAppUpdateStateResp {
        configured: false,
        available: false,
        restart_required: false,
        phase: DesktopAppUpdatePhase::Idle.as_str().to_string(),
        staged: false,
        current_version: "1.2.3".to_string(),
        latest_version: None,
        target: "macos-arm64".to_string(),
        endpoint: "https://example.test/releases/stable/latest-tauri.json".to_string(),
        message: None,
        last_attempt_id: None,
        last_error: None,
    };

    let err = transaction::short_circuit_apply(&state).expect_err("unconfigured apply should fail");
    assert!(
        err.contains("missing embedded updater public key"),
        "expected embedded pubkey guidance: {err}"
    );
}

#[test]
fn staged_ready_transaction_marks_update_available_without_restart() {
    let state = transaction::staged_ready_state(
        &staged_config(),
        "1.2.3".to_string(),
        "1.2.4".to_string(),
        Some("attempt-3".to_string()),
        None,
    );
    assert!(state.configured);
    assert!(state.available);
    assert!(!state.restart_required);
    assert!(state.staged);
    assert_eq!(state.phase, DesktopAppUpdatePhase::StagedReady.as_str());
    assert_eq!(state.latest_version.as_deref(), Some("1.2.4"));
}

fn freshness_state() -> DesktopAppUpdateStateResp {
    DesktopAppUpdateStateResp {
        configured: true,
        available: false,
        restart_required: false,
        phase: DesktopAppUpdatePhase::Idle.as_str().to_string(),
        staged: false,
        current_version: "1.2.3".to_string(),
        latest_version: None,
        target: "macos-arm64".to_string(),
        endpoint: "https://example.test/releases/stable/latest-tauri.json".to_string(),
        message: None,
        last_attempt_id: None,
        last_error: None,
    }
}

#[test]
fn remote_bootstrap_freshness_allows_current_desktop() {
    let state = freshness_state();
    let result = recovery::validate_remote_bootstrap_desktop_freshness(&state, "stable");
    assert!(
        result.is_ok(),
        "current desktop should pass remote bootstrap freshness gate: {result:?}"
    );
}

#[test]
fn remote_bootstrap_freshness_rejects_stale_desktop() {
    let mut state = freshness_state();
    state.latest_version = Some("1.2.4".to_string());
    state.phase = DesktopAppUpdatePhase::Staging.as_str().to_string();
    let err = recovery::validate_remote_bootstrap_desktop_freshness(&state, "stable")
        .expect_err("stale desktop should be rejected");
    assert!(
        err.contains(REMOTE_BOOTSTRAP_UPDATE_REQUIRED_PREFIX),
        "expected stale prefix in error: {err}"
    );
    assert!(
        err.contains("1.2.3") && err.contains("1.2.4"),
        "expected current/latest versions in error: {err}"
    );
}

#[test]
fn remote_bootstrap_freshness_rejects_restart_required_desktop() {
    let mut state = freshness_state();
    state.restart_required = true;
    state.latest_version = Some("1.2.4".to_string());
    state.phase = DesktopAppUpdatePhase::RestartRequired.as_str().to_string();
    let err = recovery::validate_remote_bootstrap_desktop_freshness(&state, "stable")
        .expect_err("restart-required desktop should be rejected");
    assert!(
        err.contains("Restart ctx to finish applying desktop version `1.2.4`"),
        "expected restart guidance in error: {err}"
    );
}

#[test]
fn remote_bootstrap_freshness_rejects_unverified_desktop() {
    let mut state = freshness_state();
    state.configured = false;
    state.message = Some("Native updater is not configured.".to_string());
    let err = recovery::validate_remote_bootstrap_desktop_freshness(&state, "stable")
        .expect_err("unverified desktop freshness should be rejected");
    assert!(
        err.contains(REMOTE_BOOTSTRAP_FRESHNESS_UNVERIFIED_PREFIX),
        "expected unverified prefix in error: {err}"
    );
}
