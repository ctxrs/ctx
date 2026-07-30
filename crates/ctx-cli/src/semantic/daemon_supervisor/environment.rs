use std::{collections::BTreeMap, env, ffi::OsString};

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use ctx_history_core::utc_now;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::compact_json;

const SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST: &[&str] = &[
    "ALL_PROXY",
    "CURL_CA_BUNDLE",
    "CTX_ANALYTICS_ENABLED",
    "CTX_DAEMON_ENABLED",
    "CTX_DAEMON_MODE",
    "CTX_LOCAL_USAGE_ENABLED",
    "CTX_SEARCH_SEMANTIC",
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
const SUPERVISOR_DAEMON_FIXED_PATH: &str = if cfg!(windows) {
    r"C:\Windows\System32;C:\Windows"
} else {
    "/usr/local/bin:/usr/bin:/bin"
};

#[derive(Debug, Clone)]
pub(super) struct SupervisorEnvironmentSnapshot {
    pub(super) values: Vec<(String, String)>,
    captured_at_ms: i64,
    sha256: String,
}

impl SupervisorEnvironmentSnapshot {
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
            "sha256": self.sha256,
            "values_exposed": false,
            "error": Value::Null,
        }))
    }
}

pub(super) fn supervisor_environment_snapshot() -> Result<SupervisorEnvironmentSnapshot> {
    let mut values = BTreeMap::new();
    for name in ctx_history_capture::provider_sources::DISCOVERY_ENV_ALLOWLIST
        .iter()
        .chain(SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST)
    {
        let Some(value) = env::var_os(name) else {
            continue;
        };
        values.insert(
            (*name).to_owned(),
            validated_supervisor_environment_value(name, value)?,
        );
    }
    values.insert("PATH".to_owned(), SUPERVISOR_DAEMON_FIXED_PATH.to_owned());
    #[cfg(unix)]
    if !values.contains_key("HOME") {
        if let Some(home) = crate::identity::home_dir() {
            let home = validated_supervisor_fallback_home(home)?;
            values.insert("HOME".to_owned(), home);
        }
    }

    let values = values.into_iter().collect::<Vec<_>>();
    let mut digest = Sha256::new();
    for (name, value) in &values {
        digest.update(u64::try_from(name.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(value.as_bytes());
    }
    Ok(SupervisorEnvironmentSnapshot {
        values,
        captured_at_ms: utc_now().timestamp_millis(),
        sha256: format!("{:x}", digest.finalize()),
    })
}

pub(super) fn supervisor_environment_contract_report() -> Value {
    match supervisor_environment_snapshot() {
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

#[cfg(any(test, target_os = "linux"))]
pub(super) fn linux_systemd_unit(executable: &Path, data_root: &Path) -> Result<String> {
    let snapshot = supervisor_environment_snapshot()?;
    linux_systemd_unit_with_environment(executable, data_root, &snapshot)
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn linux_systemd_unit_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let executable = validated_supervisor_artifact_path("ctx executable", executable)?;
    let data_root = validated_supervisor_artifact_path("ctx data root", data_root)?;
    let environment = snapshot
        .values
        .iter()
        .map(|(name, value)| systemd_quote_text(&format!("{name}={value}")))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!(
        "[Unit]\nDescription=ctx persistent history daemon\n\n[Service]\nType=simple\nExecStart=/usr/bin/env -i {} {} --data-root {} daemon run --format=json\nRestart=on-failure\nRestartSec=2\nStandardOutput=null\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n",
        environment,
        systemd_quote_text(executable),
        systemd_quote_text(data_root),
    ))
}

#[cfg(any(test, target_os = "linux"))]
fn systemd_quote_text(value: &str) -> String {
    let value = value.replace('%', "%%");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn launch_agent_plist(executable: &Path, data_root: &Path) -> Result<String> {
    let snapshot = supervisor_environment_snapshot()?;
    launch_agent_plist_with_environment(executable, data_root, &snapshot)
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn launch_agent_plist_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let executable = validated_supervisor_artifact_path("ctx executable", executable)?;
    let data_root = validated_supervisor_artifact_path("ctx data root", data_root)?;
    let environment = snapshot
        .values
        .iter()
        .map(|(name, value)| {
            format!(
                "<string>{}</string>",
                xml_escape(&format!("{name}={value}"))
            )
        })
        .collect::<Vec<_>>()
        .join("");
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>rs.ctx.daemon</string>\n<key>ProgramArguments</key><array><string>/usr/bin/env</string><string>-i</string>{}<string>{}</string><string>--data-root</string><string>{}</string><string>daemon</string><string>run</string><string>--format=json</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ProcessType</key><string>Background</string>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n",
        environment,
        xml_escape(executable),
        xml_escape(data_root),
    ))
}

#[cfg(any(test, target_os = "macos", windows))]
pub(super) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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

pub(super) fn validated_supervisor_artifact_path<'a>(
    label: &str,
    path: &'a Path,
) -> Result<&'a str> {
    let value = path.to_str().ok_or_else(|| {
        anyhow!("supervisor {label} is not Unicode and cannot be persisted safely")
    })?;
    validated_supervisor_artifact_text(label, value)
}

pub(super) fn validated_supervisor_artifact_text<'a>(
    label: &str,
    value: &'a str,
) -> Result<&'a str> {
    if value.chars().any(char::is_control) {
        return Err(anyhow!(
            "supervisor {label} contains control characters and cannot be persisted safely"
        ));
    }
    Ok(value)
}

fn supervisor_environment_allowlist_names() -> Vec<&'static str> {
    let mut names = ctx_history_capture::provider_sources::DISCOVERY_ENV_ALLOWLIST.to_vec();
    names.extend_from_slice(SUPERVISOR_DAEMON_POLICY_ENV_ALLOWLIST);
    names.push("PATH");
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_is_narrow_nonsecret_and_rejects_controls() {
        let allowlist = supervisor_environment_allowlist_names();
        for required in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "COPILOT_HOME",
            "XDG_CONFIG_HOME",
            "CTX_LOCAL_USAGE_ENABLED",
            "CTX_ANALYTICS_ENABLED",
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
            "CTX_PRO_HELPER",
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
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
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
}
