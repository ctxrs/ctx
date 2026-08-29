#[cfg(any(test, unix, windows))]
use std::path::PathBuf;
use std::{collections::BTreeMap, env, ffi::OsString, path::Path};

use anyhow::{anyhow, Result};
use ctx_daemon_runtime::{NormalizedLaunch, SupervisorIdentity, SupervisorSpec};
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    compact_json, DaemonApplicationHost, SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV,
    SEMANTIC_EMBEDDING_TOKEN_ENV,
};

const SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST: &[&str] = &[
    "ALL_PROXY",
    "CURL_CA_BUNDLE",
    "CTX_ANALYTICS_ENABLED",
    "CTX_DAEMON_ENABLED",
    "CTX_DAEMON_MODE",
    "CTX_LOCAL_USAGE_ENABLED",
    "CTX_SEARCH_SEMANTIC",
    SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV,
    SEMANTIC_EMBEDDING_TOKEN_ENV,
    "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_CHANNEL",
    "CTX_UPGRADE_INTERVAL_SECONDS",
    "DBUS_SESSION_BUS_ADDRESS",
    "HOMEDRIVE",
    "HOMEPATH",
    "HOME",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "LANG",
    "LC_ALL",
    "LOCALAPPDATA",
    "MIMOCODE_CONFIG_DIR",
    "NO_PROXY",
    "REQUESTS_CA_BUNDLE",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SystemRoot",
    "TEMP",
    "TMP",
    "TMPDIR",
    "TZ",
    "USERPROFILE",
    "WINDIR",
    "XDG_RUNTIME_DIR",
    "all_proxy",
    "https_proxy",
    "http_proxy",
    "no_proxy",
];
const DAEMON_LOOP_INTERVAL_ENV: &str = "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS";
pub(super) const HOSTED_INSTALLER_SETUP_ENV: &str = "CTX_HOSTED_INSTALLER_SETUP";
const HOSTED_INSTALLER_TRANSIENT_POLICY_ENV: &[&str] =
    &["CTX_SEARCH_SEMANTIC", "CTX_UPGRADE_CHANNEL"];
#[cfg(any(test, target_os = "linux"))]
pub(super) const SYSTEMD_UNIT_NAME: &str = "ctx.service";
#[cfg(any(test, target_os = "macos"))]
pub(super) const LAUNCH_AGENT_LABEL: &str = "rs.ctx.daemon";
pub(super) const SUPERVISOR_DESCRIPTION: &str = "ctx persistent history daemon";
const SUPERVISOR_DAEMON_FIXED_PATH: &str = if cfg!(windows) {
    r"C:\Windows\System32;C:\Windows"
} else {
    "/usr/local/bin:/usr/bin:/bin"
};

// This launch-only subset intentionally lives here so lifecycle policy does
// not acquire a history-capture or provider-discovery dependency.
const DISCOVERY_ENV_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "ASTRBOT_ROOT",
    "CLAUDE_CONFIG_DIR",
    "CLINE_DATA_DIR",
    "CLINE_DB_DATA_DIR",
    "CLINE_DIR",
    "CLINE_SANDBOX",
    "CLINE_SANDBOX_DATA_DIR",
    "CLINE_SESSION_DATA_DIR",
    "CODEBUDDY_CONFIG_DIR",
    "CODEX_HOME",
    "CONTINUE_GLOBAL_DIR",
    "COPILOT_HOME",
    "CRUSH_GLOBAL_CONFIG",
    "CRUSH_GLOBAL_DATA",
    "CURSOR_DATA_DIR",
    "DSH_HOME",
    "FILE_STORE",
    "FILE_STORE_PATH",
    "FLATPAK_XDG_DATA_HOME",
    "FORGE_CONFIG",
    "GEMINI_CLI_HOME",
    "GOOSE_PATH_ROOT",
    "GROK_HOME",
    "HERMES_HOME",
    "JUNIE_HOME",
    "KILO_DB",
    "KIMI_CODE_HOME",
    "KIRO_HOME",
    "MIMOCODE_DB",
    "MIMOCODE_HOME",
    "MUX_ROOT",
    "NODE_ENV",
    "OH_PERSISTENCE_DIR",
    "OPENCLAW_HOME",
    "OPENCLAW_STATE_DIR",
    "OPENHANDS_CONVERSATIONS_DIR",
    "OPENHANDS_PERSISTENCE_DIR",
    "OPENHANDS_USER_ID",
    "OPENCODE_DB",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "QODER_CONFIG_DIR",
    "QWEN_CODE_SYSTEM_DEFAULTS_PATH",
    "QWEN_CODE_SYSTEM_SETTINGS_PATH",
    "QWEN_CODE_TRUSTED_FOLDERS_PATH",
    "QWEN_HOME",
    "QWEN_RUNTIME_DIR",
    "SHARED_EVENT_STORAGE_PROVIDER",
    "VIBE_HOME",
    "VIBE_SESSION_LOGGING",
    "VIBE_SESSION_LOGGING__SAVE_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "ZED_STATELESS",
];

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SupervisorEnvironmentSnapshot {
    pub(super) values: Vec<(String, String)>,
    loop_interval_seconds: Option<u64>,
    captured_at_ms: i64,
    sha256: String,
}

impl SupervisorEnvironmentSnapshot {
    pub(super) fn loop_interval_seconds(&self) -> Option<u64> {
        self.loop_interval_seconds
    }

    #[cfg(test)]
    pub(super) fn identity_sha256(&self) -> &str {
        &self.sha256
    }

    pub(super) fn with_loop_interval_seconds(
        mut self,
        loop_interval_seconds: Option<u64>,
    ) -> Result<Self> {
        if loop_interval_seconds.is_some_and(|value| value == 0 || value > 3_600) {
            return Err(anyhow!(
                "daemon supervisor loop interval must be between 1 and 3600 seconds"
            ));
        }
        self.loop_interval_seconds = loop_interval_seconds;
        self.sha256 = supervisor_environment_sha256(&self.values, loop_interval_seconds);
        Ok(self)
    }

    pub(super) fn without_semantic_embedding_auth(mut self) -> Self {
        self.values.retain(|(name, _)| {
            name != SEMANTIC_EMBEDDING_TOKEN_ENV && name != SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV
        });
        self.sha256 = supervisor_environment_sha256(&self.values, self.loop_interval_seconds);
        self
    }

    pub(super) fn contract_report(&self) -> Value {
        compact_json(json!({
            "schema_version": 1,
            "captured_at_ms": self.captured_at_ms,
            "allowlist": supervisor_environment_allowlist_names(),
            "captured_names": self
                .values
                .iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            "loop_interval_seconds": self.loop_interval_seconds,
            "sha256": self.sha256,
            "values_exposed": false,
            "error": Value::Null,
        }))
    }
}

pub(super) fn supervisor_environment_snapshot(
    _host: &dyn DaemonApplicationHost,
) -> Result<SupervisorEnvironmentSnapshot> {
    let mut values = BTreeMap::new();
    let hosted_installer_setup =
        env::var_os(HOSTED_INSTALLER_SETUP_ENV).as_deref() == Some(std::ffi::OsStr::new("1"));
    for name in DISCOVERY_ENV_ALLOWLIST
        .iter()
        .chain(SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST)
    {
        if hosted_installer_setup && HOSTED_INSTALLER_TRANSIENT_POLICY_ENV.contains(name) {
            continue;
        }
        let Some(value) = env::var_os(name) else {
            continue;
        };
        values.insert(
            (*name).to_owned(),
            validated_supervisor_environment_value(name, value)?,
        );
    }
    if !values.contains_key(SEMANTIC_EMBEDDING_TOKEN_ENV)
        || !values.contains_key(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV)
    {
        values.remove(SEMANTIC_EMBEDDING_TOKEN_ENV);
        values.remove(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV);
    }
    values.insert("PATH".to_owned(), SUPERVISOR_DAEMON_FIXED_PATH.to_owned());
    #[cfg(unix)]
    if !values.contains_key("HOME") {
        if let Some(home) = _host.home_dir() {
            let home = validated_supervisor_fallback_home(home)?;
            values.insert("HOME".to_owned(), home);
        }
    }

    let values = values.into_iter().collect::<Vec<_>>();
    let loop_interval_seconds = env::var(DAEMON_LOOP_INTERVAL_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(3_600));
    let sha256 = supervisor_environment_sha256(&values, loop_interval_seconds);
    Ok(SupervisorEnvironmentSnapshot {
        values,
        loop_interval_seconds,
        captured_at_ms: utc_now().timestamp_millis(),
        sha256,
    })
}

fn supervisor_environment_sha256(
    values: &[(String, String)],
    loop_interval_seconds: Option<u64>,
) -> String {
    let mut digest = Sha256::new();
    for (name, value) in values {
        digest.update(u64::try_from(name.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(value.as_bytes());
    }
    if let Some(loop_interval_seconds) = loop_interval_seconds {
        digest.update(b"daemon_loop_interval_seconds");
        digest.update(loop_interval_seconds.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn supervisor_environment_contract_report(host: &dyn DaemonApplicationHost) -> Value {
    match supervisor_environment_snapshot(host) {
        Ok(snapshot) => snapshot.contract_report(),
        Err(error) => compact_json(json!({
            "schema_version": 1,
            "captured_at_ms": utc_now().timestamp_millis(),
            "allowlist": supervisor_environment_allowlist_names(),
            "captured_names": [],
            "sha256": Value::Null,
            "values_exposed": false,
            "error": format!("{error:#}"),
        })),
    }
}

#[cfg(test)]
pub(super) fn linux_systemd_unit_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let identity = supervisor_identity(SYSTEMD_UNIT_NAME, PathBuf::from(SYSTEMD_UNIT_NAME))?;
    ctx_daemon_runtime::linux_systemd_unit(&supervisor_artifact_spec(
        identity, executable, data_root, snapshot,
    )?)
}

// Called from supervisor tests on every host and from the daemon only on
// macOS; plain cargo test on this host sees no caller, so allow dead_code.
#[cfg(any(test, target_os = "macos"))]
#[allow(dead_code)]
pub(super) fn launch_agent_plist_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let identity = supervisor_identity(
        LAUNCH_AGENT_LABEL,
        PathBuf::from(format!("{LAUNCH_AGENT_LABEL}.plist")),
    )?;
    ctx_daemon_runtime::launch_agent_plist(&supervisor_artifact_spec(
        identity, executable, data_root, snapshot,
    )?)
}

#[cfg(any(test, target_os = "linux", target_os = "macos", windows))]
pub(super) fn supervisor_identity(
    name: &str,
    artifact_path: PathBuf,
) -> Result<SupervisorIdentity> {
    SupervisorIdentity::new(name, artifact_path)
}

#[cfg(any(test, windows))]
pub(super) fn windows_supervisor_identity(
    data_root: &Path,
    user_sid: &str,
) -> Result<SupervisorIdentity> {
    supervisor_identity(
        &format!(r"\ctx-daemon-{user_sid}"),
        ctx_daemon_runtime::daemon_root_path(data_root).join("windows-task.xml"),
    )
}

pub(super) fn supervisor_artifact_spec(
    identity: SupervisorIdentity,
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<SupervisorSpec> {
    let environment = snapshot
        .values
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    let mut arguments = vec![
        OsString::from("--data-root"),
        data_root.as_os_str().to_os_string(),
        OsString::from("daemon"),
        OsString::from("run"),
        OsString::from("--format=json"),
    ];
    if let Some(loop_interval_seconds) = snapshot.loop_interval_seconds {
        arguments.push(OsString::from("--loop-interval-seconds"));
        arguments.push(OsString::from(loop_interval_seconds.to_string()));
    }
    SupervisorSpec::new(
        identity,
        SUPERVISOR_DESCRIPTION,
        ctx_daemon_runtime::supervisor_environment_path(data_root),
        NormalizedLaunch::new(executable.to_path_buf(), arguments, environment),
    )
}

fn validated_supervisor_environment_value(name: &str, value: OsString) -> Result<String> {
    let value = value.into_string().map_err(|_| {
        anyhow!(
            "supervisor environment variable {name} is not Unicode; remove it or persist the path in ctx configuration"
        )
    })?;
    validated_supervisor_artifact_text(&format!("environment variable {name}"), &value)?;
    Ok(value)
}

#[cfg(unix)]
fn validated_supervisor_fallback_home(home: PathBuf) -> Result<String> {
    validated_supervisor_environment_value("HOME", home.into_os_string())
}

pub(super) fn validated_supervisor_artifact_text<'a>(
    label: &str,
    value: &'a str,
) -> Result<&'a str> {
    ctx_daemon_runtime::validated_supervisor_artifact_text(label, value)
}

pub(crate) fn supervisor_environment_allowlist_names() -> Vec<&'static str> {
    let mut names = DISCOVERY_ENV_ALLOWLIST.to_vec();
    names.extend_from_slice(SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST);
    names.push("PATH");
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestHost;

    struct RestoreEnvironment(Vec<(&'static str, Option<OsString>)>);

    impl RestoreEnvironment {
        fn capture(names: &[&'static str]) -> Self {
            Self(
                names
                    .iter()
                    .map(|name| (*name, env::var_os(name)))
                    .collect(),
            )
        }
    }

    impl Drop for RestoreEnvironment {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn contract_is_narrow_and_rejects_controls() {
        let allowlist = supervisor_environment_allowlist_names();
        for required in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "COPILOT_HOME",
            "DSH_HOME",
            "GROK_HOME",
            "XDG_CONFIG_HOME",
            "CTX_LOCAL_USAGE_ENABLED",
            "CTX_ANALYTICS_ENABLED",
            "CTX_SEARCH_SEMANTIC",
            SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV,
            SEMANTIC_EMBEDDING_TOKEN_ENV,
            "CTX_UPGRADE_AUTO",
            "CTX_UPGRADE_CHANNEL",
            "CTX_UPGRADE_INTERVAL_SECONDS",
            "HTTPS_PROXY",
            "MIMOCODE_CONFIG_DIR",
            "NO_PROXY",
            "SSL_CERT_FILE",
            "CURL_CA_BUNDLE",
        ] {
            assert!(allowlist.contains(&required), "missing {required}");
        }
        for forbidden in [
            "CTX_PRO_CHANNEL",
            "CTX_PRO_HELPER",
            "CTX_SEMANTIC_EMBEDDING_FALLBACK_TOKEN",
            "CTX_SEMANTIC_MODEL_ONNX",
            "CTX_SEMANTIC_COREML_NATIVE_COMPUTE",
            "CTX_ANALYTICS_ENDPOINT",
            "CTX_RELEASE_INHERITED_AUTHORITY",
            "CTX_RELEASE_CONFIGURED_AUTHORITY",
            "CTX_RELEASE_BASE_URL",
            "CTX_RELEASE_METADATA_URL",
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            "CTX_RELEASE_PUBLIC_KEY",
            "CTX_RELEASE_SIGNATURE",
            "CTX_RELEASE_SELF_UPGRADE_ALLOWED",
            "CTX_RELEASE_VERSION",
            "CTX_PRO_STAGING_ACCESS_CLIENT_ID",
            "CTX_PRO_STAGING_ACCESS_CLIENT_SECRET",
            "CTX_PRO_QUALIFICATION_HELPER_PATH",
            "CTX_PRO_QUALIFICATION_HELPER_SHA256",
            "CTX_PRO_QUALIFICATION_HELPER_CHANNEL",
            "AWS_SECRET_ACCESS_KEY",
            "DEEPSEEK_API_KEY",
            "GITHUB_TOKEN",
            "OPENROUTER_API_KEY",
            "XAI_API_KEY",
        ] {
            assert!(!allowlist.contains(&forbidden), "captured {forbidden}");
        }
        for hostile in [
            "line\nbreak",
            "carriage\rreturn",
            "tab\tvalue",
            "nul\0value",
        ] {
            let error =
                validated_supervisor_environment_value("CODEX_HOME", hostile.into()).unwrap_err();
            assert!(
                error.to_string().contains("control characters"),
                "{error:#}"
            );
        }
        #[cfg(unix)]
        assert!(validated_supervisor_fallback_home(PathBuf::from("/tmp/home\ninjected")).is_err());
    }

    #[test]
    fn supervisor_handoffs_endpoint_bound_semantic_auth_without_argument_exposure() -> Result<()> {
        const UNRELATED_TOKEN_ENV: &str = "CTX_SEMANTIC_EMBEDDING_FALLBACK_TOKEN";
        const TOKEN_A: &str = "semantic-bearer-token-a";
        const TOKEN_B: &str = "semantic-bearer-token-b";
        const ENDPOINT: &str = "https://embeddings.example.test/";
        const UNRELATED_VALUE: &str = "unrelated-token";
        let _env_lock = crate::test_environment_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _restore = RestoreEnvironment::capture(&[
            SEMANTIC_EMBEDDING_TOKEN_ENV,
            SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV,
            UNRELATED_TOKEN_ENV,
        ]);

        env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENV, TOKEN_A);
        env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV, ENDPOINT);
        env::set_var(UNRELATED_TOKEN_ENV, UNRELATED_VALUE);
        let snapshot = supervisor_environment_snapshot(&TestHost)?;
        let temp = tempfile::tempdir()?;
        let windows_data_root = temp.path().join("data");
        let windows_executable = Path::new(r"C:\Program Files\ctx\ctx.exe");
        let unit = linux_systemd_unit_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &snapshot,
        )?;
        let launch_agent = launch_agent_plist_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &snapshot,
        )?;
        let windows_script = crate::supervisor::windows_sanitized_daemon_script_with_environment(
            windows_executable,
            &windows_data_root,
            &snapshot,
        )?;
        let windows_identity = windows_supervisor_identity(&windows_data_root, "S-1-0-0")?;
        let windows_spec = supervisor_artifact_spec(
            windows_identity,
            windows_executable,
            &windows_data_root,
            &snapshot,
        )?;
        let windows_environment_path =
            ctx_daemon_runtime::write_supervisor_environment(&windows_spec)?;
        ctx_history_platform::platform_security::verify_private_file(&windows_environment_path)?;
        let windows_environment: Value =
            serde_json::from_slice(&std::fs::read(&windows_environment_path)?)?;

        assert!(snapshot
            .values
            .iter()
            .any(|(name, value)| { name == SEMANTIC_EMBEDDING_TOKEN_ENV && value == TOKEN_A }));
        assert!(snapshot.values.iter().any(|(name, value)| {
            name == SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV && value == ENDPOINT
        }));
        assert!(!snapshot
            .values
            .iter()
            .any(|(name, _)| name == UNRELATED_TOKEN_ENV));
        let exec_start = unit
            .lines()
            .find(|line| line.starts_with("ExecStart="))
            .expect("systemd ExecStart");
        assert!(!exec_start.contains(SEMANTIC_EMBEDDING_TOKEN_ENV));
        assert!(!exec_start.contains(TOKEN_A));
        assert!(unit.contains(ctx_daemon_runtime::SUPERVISOR_ENVIRONMENT_FILE_ENV));
        assert!(!unit.contains(TOKEN_A));
        assert!(!unit.contains(ENDPOINT));
        assert!(!unit.contains(UNRELATED_TOKEN_ENV));
        assert!(!unit.contains(UNRELATED_VALUE));
        let launch_arguments = launch_agent
            .split_once("<key>ProgramArguments</key><array>")
            .and_then(|(_, remainder)| remainder.split_once("</array>"))
            .map(|(arguments, _)| arguments)
            .expect("launchd ProgramArguments array");
        assert!(!launch_arguments.contains(SEMANTIC_EMBEDDING_TOKEN_ENV));
        assert!(!launch_arguments.contains(TOKEN_A));
        assert!(launch_agent.contains(ctx_daemon_runtime::SUPERVISOR_ENVIRONMENT_FILE_ENV));
        assert!(!launch_agent.contains(TOKEN_A));
        assert!(!launch_agent.contains(ENDPOINT));
        assert!(!launch_agent.contains(UNRELATED_TOKEN_ENV));
        assert!(!launch_agent.contains(UNRELATED_VALUE));
        assert!(windows_script.contains(ctx_daemon_runtime::SUPERVISOR_ENVIRONMENT_FILE_ENV));
        assert!(!windows_script.contains("Get-Content -LiteralPath"));
        assert!(!windows_script.contains(SEMANTIC_EMBEDDING_TOKEN_ENV));
        assert!(!windows_script.contains(TOKEN_A));
        assert!(!windows_script.contains(ENDPOINT));
        assert!(windows_environment["environment"]
            .as_array()
            .expect("Windows supervisor environment entries")
            .iter()
            .any(|entry| {
                entry["name"] == SEMANTIC_EMBEDDING_TOKEN_ENV && entry["value"] == TOKEN_A
            }));
        assert!(windows_environment["environment"]
            .as_array()
            .expect("Windows supervisor environment entries")
            .iter()
            .any(|entry| {
                entry["name"] == SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV && entry["value"] == ENDPOINT
            }));

        env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENV, TOKEN_B);
        env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV, ENDPOINT);
        let rotated = supervisor_environment_snapshot(&TestHost)?;
        let rotated_unit = linux_systemd_unit_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &rotated,
        )?;
        let rotated_launch_agent = launch_agent_plist_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &rotated,
        )?;
        let rotated_windows_script =
            crate::supervisor::windows_sanitized_daemon_script_with_environment(
                windows_executable,
                &windows_data_root,
                &rotated,
            )?;
        assert_ne!(rotated.sha256, snapshot.sha256);
        for artifact in [&rotated_unit, &rotated_launch_agent] {
            assert!(!artifact.contains(TOKEN_B));
            assert!(!artifact.contains(ENDPOINT));
            assert!(!artifact.contains(TOKEN_A));
        }
        assert!(!rotated_windows_script.contains(TOKEN_A));
        assert!(!rotated_windows_script.contains(TOKEN_B));
        assert!(!rotated_windows_script.contains(ENDPOINT));
        let rotated_windows_identity = windows_supervisor_identity(&windows_data_root, "S-1-0-0")?;
        let rotated_windows_spec = supervisor_artifact_spec(
            rotated_windows_identity,
            windows_executable,
            &windows_data_root,
            &rotated,
        )?;
        ctx_daemon_runtime::write_supervisor_environment(&rotated_windows_spec)?;
        let rotated_windows_environment = std::fs::read_to_string(&windows_environment_path)?;
        assert!(rotated_windows_environment.contains(TOKEN_B));
        assert!(rotated_windows_environment.contains(ENDPOINT));
        assert!(!rotated_windows_environment.contains(TOKEN_A));

        let scrubbed = rotated.clone().without_semantic_embedding_auth();
        assert_ne!(scrubbed.sha256, rotated.sha256);
        assert!(!scrubbed.values.iter().any(|(name, _)| {
            name == SEMANTIC_EMBEDDING_TOKEN_ENV || name == SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV
        }));
        for artifact in [
            linux_systemd_unit_with_environment(
                Path::new("/usr/local/bin/ctx"),
                Path::new("/home/user/.local/share/ctx"),
                &scrubbed,
            )?,
            launch_agent_plist_with_environment(
                Path::new("/usr/local/bin/ctx"),
                Path::new("/home/user/.local/share/ctx"),
                &scrubbed,
            )?,
        ] {
            assert!(!artifact.contains(SEMANTIC_EMBEDDING_TOKEN_ENV));
            assert!(!artifact.contains(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV));
            assert!(!artifact.contains(TOKEN_A));
            assert!(!artifact.contains(TOKEN_B));
            assert!(!artifact.contains(ENDPOINT));
        }
        let scrubbed_windows_identity = windows_supervisor_identity(&windows_data_root, "S-1-0-0")?;
        let scrubbed_windows_spec = supervisor_artifact_spec(
            scrubbed_windows_identity,
            windows_executable,
            &windows_data_root,
            &scrubbed,
        )?;
        ctx_daemon_runtime::write_supervisor_environment(&scrubbed_windows_spec)?;
        let scrubbed_windows_environment = std::fs::read_to_string(&windows_environment_path)?;
        assert!(!scrubbed_windows_environment.contains(SEMANTIC_EMBEDDING_TOKEN_ENV));
        assert!(!scrubbed_windows_environment.contains(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV));
        assert!(!scrubbed_windows_environment.contains(TOKEN_A));
        assert!(!scrubbed_windows_environment.contains(TOKEN_B));
        assert!(!scrubbed_windows_environment.contains(ENDPOINT));
        Ok(())
    }

    #[test]
    fn hosted_installer_keeps_named_environment_and_excludes_only_transient_policy() -> Result<()> {
        const NAMES: &[&str] = &[
            HOSTED_INSTALLER_SETUP_ENV,
            "CODEX_HOME",
            "CTX_SEARCH_SEMANTIC",
            "CTX_UPGRADE_CHANNEL",
            "CTX_UPGRADE_AUTO",
            "HTTP_PROXY",
            "SSL_CERT_FILE",
        ];
        let _env_lock = crate::test_environment_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _restore = RestoreEnvironment::capture(NAMES);

        env::set_var("CODEX_HOME", "/tmp/ctx-hosted-codex-home");
        env::set_var("CTX_SEARCH_SEMANTIC", "true");
        env::set_var("CTX_UPGRADE_CHANNEL", "staging");
        env::set_var("CTX_UPGRADE_AUTO", "false");
        env::set_var("HTTP_PROXY", "http://proxy.example.test:8080");
        env::set_var("SSL_CERT_FILE", "/tmp/ctx-supervisor-ca-before.pem");
        env::set_var(HOSTED_INSTALLER_SETUP_ENV, "1");
        let installer_snapshot = supervisor_environment_snapshot(&TestHost)?;
        let installer_unit = linux_systemd_unit_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &installer_snapshot,
        )?;

        env::remove_var("CTX_SEARCH_SEMANTIC");
        env::remove_var("CTX_UPGRADE_CHANNEL");
        env::remove_var(HOSTED_INSTALLER_SETUP_ENV);
        let ordinary_shell_snapshot = supervisor_environment_snapshot(&TestHost)?;
        let ordinary_shell_unit = linux_systemd_unit_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &ordinary_shell_snapshot,
        )?;

        assert_eq!(installer_snapshot.sha256, ordinary_shell_snapshot.sha256);
        assert_eq!(installer_snapshot.values, ordinary_shell_snapshot.values);
        assert_eq!(installer_unit, ordinary_shell_unit);
        for excluded in [
            HOSTED_INSTALLER_SETUP_ENV,
            "CTX_SEARCH_SEMANTIC",
            "CTX_UPGRADE_CHANNEL",
        ] {
            assert!(!installer_snapshot
                .values
                .iter()
                .any(|(name, _)| name == excluded));
        }
        for retained in [
            "CODEX_HOME",
            "CTX_UPGRADE_AUTO",
            "HTTP_PROXY",
            "SSL_CERT_FILE",
        ] {
            assert!(
                installer_snapshot
                    .values
                    .iter()
                    .any(|(name, _)| name == retained),
                "missing {retained}"
            );
        }

        env::set_var("SSL_CERT_FILE", "/tmp/ctx-supervisor-ca-after.pem");
        let ordinary_operator_snapshot = supervisor_environment_snapshot(&TestHost)?;
        assert_ne!(
            installer_snapshot.sha256, ordinary_operator_snapshot.sha256,
            "ordinary setup must retain operator-owned daemon environment overrides"
        );
        assert!(ordinary_operator_snapshot
            .values
            .iter()
            .any(|(name, value)| {
                name == "SSL_CERT_FILE" && value == "/tmp/ctx-supervisor-ca-after.pem"
            }));
        Ok(())
    }

    #[test]
    fn ordinary_operator_policy_overrides_remain_in_the_supervisor_contract() -> Result<()> {
        const NAMES: &[&str] = &[
            HOSTED_INSTALLER_SETUP_ENV,
            "CTX_SEARCH_SEMANTIC",
            "CTX_UPGRADE_CHANNEL",
        ];
        let _env_lock = crate::test_environment_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _restore = RestoreEnvironment::capture(NAMES);

        env::remove_var(HOSTED_INSTALLER_SETUP_ENV);
        env::set_var("CTX_SEARCH_SEMANTIC", "true");
        env::set_var("CTX_UPGRADE_CHANNEL", "staging");
        let snapshot = supervisor_environment_snapshot(&TestHost)?;
        let unit = linux_systemd_unit_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &snapshot,
        )?;

        assert!(snapshot
            .values
            .iter()
            .any(|(name, value)| name == "CTX_SEARCH_SEMANTIC" && value == "true"));
        assert!(snapshot
            .values
            .iter()
            .any(|(name, value)| name == "CTX_UPGRADE_CHANNEL" && value == "staging"));
        assert!(!unit.contains("CTX_SEARCH_SEMANTIC=true"));
        assert!(!unit.contains("CTX_UPGRADE_CHANNEL=staging"));
        Ok(())
    }

    #[test]
    fn explicit_loop_interval_is_part_of_the_verified_supervisor_launch() -> Result<()> {
        let _env_lock = crate::test_environment_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _restore = RestoreEnvironment::capture(&[DAEMON_LOOP_INTERVAL_ENV]);
        env::remove_var(DAEMON_LOOP_INTERVAL_ENV);

        let default_snapshot = supervisor_environment_snapshot(&TestHost)?;
        let custom_snapshot = default_snapshot
            .clone()
            .with_loop_interval_seconds(Some(23))?;
        let unit = linux_systemd_unit_with_environment(
            Path::new("/usr/local/bin/ctx"),
            Path::new("/home/user/.local/share/ctx"),
            &custom_snapshot,
        )?;

        assert_eq!(default_snapshot.loop_interval_seconds(), None);
        assert_eq!(custom_snapshot.loop_interval_seconds(), Some(23));
        assert_ne!(default_snapshot.sha256, custom_snapshot.sha256);
        assert!(unit.contains("--loop-interval-seconds 23"));
        assert!(default_snapshot
            .with_loop_interval_seconds(Some(0))
            .is_err());
        assert!(custom_snapshot
            .with_loop_interval_seconds(Some(3_601))
            .is_err());
        Ok(())
    }
}
