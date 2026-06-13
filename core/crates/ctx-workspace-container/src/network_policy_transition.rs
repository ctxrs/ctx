use std::borrow::Cow;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_core::ids::WorkspaceId;
use ctx_sandbox_container_runtime::{
    command_output_message, command_output_with_timeout, sandbox_container_command,
    SandboxCommandMode,
};
use ctx_sandbox_contract::{ContainerExecutionSettings, ContainerNetworkMode};
use serde::Serialize;
use tokio::{fs, io::AsyncWriteExt};

use crate::allowlist;
use crate::container::container_data_root;
use crate::SANDBOX_OP_TIMEOUT;

const EGRESS_PROXY_BINARY: &str = "ctx-egress-proxy";
const EGRESS_PROXY_CONFIG_NAME: &str = "egress-proxy.json";
const EGRESS_PROXY_CONTAINER_PATH: &str = "/usr/local/bin/ctx-egress-proxy";
const TRANSPARENT_PROXY_PORT: u16 = 15001;
const EGRESS_PROXY_BYPASS_UID: u32 = 43_558;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedContainerNetworkPolicy {
    pub egress_guard: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TransparentProxyConfig {
    listen: String,
    mode: ContainerNetworkMode,
    allowlist: Vec<String>,
    max_peek_bytes: usize,
    bypass_uid: u32,
}

pub async fn apply_container_network_policy(
    data_root: &Path,
    mode: &SandboxCommandMode,
    workspace_id: WorkspaceId,
    name: &str,
    settings: &ContainerExecutionSettings,
    daemon_host: &str,
    daemon_port: u16,
) -> Result<AppliedContainerNetworkPolicy> {
    if matches!(settings.network_mode, ContainerNetworkMode::All) {
        return transition_to_unrestricted_network(data_root, mode, name).await;
    }

    transition_to_restricted_network(
        data_root,
        mode,
        workspace_id,
        name,
        settings,
        daemon_host,
        daemon_port,
    )
    .await
}

pub fn transparent_proxy_policy(
    settings: &ContainerExecutionSettings,
) -> (ContainerNetworkMode, Vec<String>) {
    match settings.network_mode {
        ContainerNetworkMode::LlmOnly => {
            let mut entries: Vec<String> = allowlist::LLM_ALLOWLIST
                .iter()
                .filter_map(|entry| allowlist::normalize_allowlist_entry(entry))
                .collect();
            entries.sort();
            entries.dedup();
            (ContainerNetworkMode::Allowlist, entries)
        }
        ContainerNetworkMode::Allowlist => {
            (ContainerNetworkMode::Allowlist, settings.allowlist.clone())
        }
        ContainerNetworkMode::All => (ContainerNetworkMode::All, Vec::new()),
    }
}

fn transparent_proxy_pid_file() -> Cow<'static, str> {
    std::env::var("CTX_EGRESS_PROXY_PID_FILE")
        .map(Cow::Owned)
        .unwrap_or_else(|_| Cow::Borrowed("/tmp/ctx-egress-proxy.pid"))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

fn daemon_ip_resolution_script(daemon_host: &str) -> String {
    match daemon_host.parse::<IpAddr>() {
        Ok(IpAddr::V4(_)) => format!("daemon_ip={}", shell_single_quote(daemon_host)),
        Ok(IpAddr::V6(_)) => "exit 44".to_string(),
        Err(_) => format!(
            r#"daemon_ip="$(getent hosts {daemon_host} | awk '$1 ~ /^[0-9.]+$/ {{ print $1; exit }}' || true)"
if [ -z "$daemon_ip" ]; then
  exit 44
fi"#,
            daemon_host = shell_single_quote(daemon_host),
        ),
    }
}

async fn transition_to_unrestricted_network(
    data_root: &Path,
    mode: &SandboxCommandMode,
    name: &str,
) -> Result<AppliedContainerNetworkPolicy> {
    let stop_err = stop_transparent_proxy(data_root, mode, name).await.err();
    let clear_err = clear_egress_guard(data_root, mode, name).await.err();
    let mut failures = Vec::new();
    if let Some(err) = stop_err {
        failures.push(format!("stop transparent proxy: {err:#}"));
    }
    if let Some(err) = clear_err {
        failures.push(format!("clear egress guard: {err:#}"));
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "failed to tear down restricted container network policy: {}",
            failures.join("; ")
        );
    }
    Ok(AppliedContainerNetworkPolicy {
        egress_guard: false,
    })
}

async fn transition_to_restricted_network(
    data_root: &Path,
    mode: &SandboxCommandMode,
    workspace_id: WorkspaceId,
    name: &str,
    settings: &ContainerExecutionSettings,
    daemon_host: &str,
    daemon_port: u16,
) -> Result<AppliedContainerNetworkPolicy> {
    let proxy_bin = match ensure_egress_proxy_available(data_root, mode, name).await {
        Ok(()) => EGRESS_PROXY_CONTAINER_PATH.to_string(),
        Err(img_err) => {
            if std::env::var("CTX_EGRESS_PROXY_PATH").ok().is_some() {
                let host_bin = ensure_egress_proxy_binary(data_root).await?;
                host_bin.to_string_lossy().to_string()
            } else {
                return Err(img_err).context(
                    "restricted container networking requires ctx-egress-proxy in the container image",
                );
            }
        }
    };
    let (proxy_mode, proxy_allowlist) = transparent_proxy_policy(settings);
    let proxy_config = TransparentProxyConfig {
        listen: format!("127.0.0.1:{TRANSPARENT_PROXY_PORT}"),
        mode: proxy_mode,
        allowlist: proxy_allowlist,
        max_peek_bytes: 16 * 1024,
        bypass_uid: EGRESS_PROXY_BYPASS_UID,
    };
    let config_path =
        write_transparent_proxy_config(&container_data_root(data_root, workspace_id), proxy_config)
            .await?;
    start_transparent_proxy(
        data_root,
        mode,
        name,
        &PathBuf::from(proxy_bin),
        &config_path,
        &config_path.with_file_name("ctx-egress-proxy.log"),
    )
    .await?;
    let egress_guard = configure_transparent_egress_guard(
        data_root,
        mode,
        name,
        TRANSPARENT_PROXY_PORT,
        daemon_host,
        daemon_port,
    )
    .await?;
    Ok(AppliedContainerNetworkPolicy { egress_guard })
}

fn proxy_runtime_root(data_root: &Path) -> PathBuf {
    data_root.join("runtimes").join(EGRESS_PROXY_BINARY)
}

fn proxy_runtime_path(data_root: &Path) -> PathBuf {
    proxy_runtime_root(data_root).join(EGRESS_PROXY_BINARY)
}

async fn ensure_egress_proxy_binary(data_root: &Path) -> Result<PathBuf> {
    let runtime_root = proxy_runtime_root(data_root);
    fs::create_dir_all(&runtime_root).await?;
    let dest = proxy_runtime_path(data_root);
    let src = match std::env::var("CTX_EGRESS_PROXY_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            anyhow::bail!(
                "missing CTX_EGRESS_PROXY_PATH; host-injected egress proxy requires an explicit Linux binary path"
            )
        }
    };
    if !src.exists() {
        anyhow::bail!("missing {EGRESS_PROXY_BINARY} binary at {}", src.display());
    }
    if src != dest {
        fs::copy(&src, &dest).await?;
    }
    let mut perms = fs::metadata(&dest).await?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&dest, perms).await?;
    Ok(dest)
}

async fn ensure_egress_proxy_available(
    data_root: &Path,
    mode: &SandboxCommandMode,
    container_name: &str,
) -> Result<()> {
    let script = format!(
        "set -e; command -v iptables >/dev/null 2>&1; test -x '{EGRESS_PROXY_CONTAINER_PATH}'"
    );
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg(container_name)
        .arg("sh")
        .arg("-c")
        .arg(script);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "container missing required egress tooling (iptables and/or {EGRESS_PROXY_CONTAINER_PATH}); status {}",
        output.status
    );
}

async fn write_transparent_proxy_config(
    root: &Path,
    config: TransparentProxyConfig,
) -> Result<PathBuf> {
    fs::create_dir_all(root).await?;
    let path = root.join(EGRESS_PROXY_CONFIG_NAME);
    let raw = serde_json::to_string_pretty(&config)?;
    let mut file = fs::File::create(&path).await?;
    file.write_all(raw.as_bytes()).await?;
    Ok(path)
}

async fn start_transparent_proxy(
    data_root: &Path,
    mode: &SandboxCommandMode,
    name: &str,
    bin_path: &Path,
    config_path: &Path,
    log_path: &Path,
) -> Result<()> {
    let bin = bin_path.to_string_lossy();
    let config = config_path.to_string_lossy();
    let log = log_path.to_string_lossy();
    let pid_file = transparent_proxy_pid_file();
    let script = transparent_proxy_start_script(&bin, &config, &log, &pid_file);
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg(name)
        .arg("sh")
        .arg("-c")
        .arg(script);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "failed to start transparent proxy (status: {})",
            output.status
        );
    }
}

fn transparent_proxy_start_script(bin: &str, config: &str, log: &str, pid_file: &str) -> String {
    format!(
        r#"
set -e
pid_file="{pid_file}"
if [ -f "$pid_file" ]; then
  old_pid="$(cat "$pid_file" 2>/dev/null || true)"
  if [ -n "$old_pid" ]; then
    kill "$old_pid" || true
  fi
  rm -f "$pid_file"
fi
mkdir -p "$(dirname '{log}')"
if command -v nohup >/dev/null 2>&1; then
  nohup '{bin}' --config '{config}' >'{log}' 2>&1 &
elif command -v setsid >/dev/null 2>&1; then
  setsid '{bin}' --config '{config}' >'{log}' 2>&1 &
else
  '{bin}' --config '{config}' >'{log}' 2>&1 &
fi
echo $! > "$pid_file"
exit 0
"#
    )
}

async fn stop_transparent_proxy(
    data_root: &Path,
    mode: &SandboxCommandMode,
    name: &str,
) -> Result<()> {
    let pid_file = transparent_proxy_pid_file();
    let script = format!(
        r#"
set -e
pid_file="{pid_file}"
if [ -f "$pid_file" ]; then
  old_pid="$(cat "$pid_file")"
  if [ -n "$old_pid" ]; then
    if kill -0 "$old_pid" 2>/dev/null; then
      if ! kill "$old_pid" 2>/dev/null; then
        if kill -0 "$old_pid" 2>/dev/null; then
          echo "failed to stop transparent proxy pid $old_pid" >&2
          exit 45
        fi
      fi
    fi
  fi
  rm -f "$pid_file"
fi
"#
    );
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg(name)
        .arg("sh")
        .arg("-c")
        .arg(script);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        Ok(())
    } else {
        let combined = command_output_message(&output);
        if combined.is_empty() {
            anyhow::bail!(
                "failed to stop transparent proxy (status: {})",
                output.status
            );
        }
        anyhow::bail!("failed to stop transparent proxy: {combined}");
    }
}

async fn configure_transparent_egress_guard(
    data_root: &Path,
    mode: &SandboxCommandMode,
    name: &str,
    proxy_port: u16,
    daemon_host: &str,
    daemon_port: u16,
) -> Result<bool> {
    let script = transparent_egress_guard_script(proxy_port, daemon_host, daemon_port);
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg(name)
        .arg("sh")
        .arg("-c")
        .arg(script);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        return Ok(true);
    }
    if let Some(code) = output.status.code() {
        if code == 43 {
            anyhow::bail!("iptables missing in harness container");
        }
        if code == 44 {
            anyhow::bail!("daemon host not resolvable inside harness container");
        }
    }
    let combined = command_output_message(&output);
    if combined.is_empty() {
        anyhow::bail!(
            "failed to configure egress guard (status: {})",
            output.status
        );
    }
    anyhow::bail!("failed to configure egress guard: {combined}");
}

fn transparent_egress_guard_script(proxy_port: u16, daemon_host: &str, daemon_port: u16) -> String {
    let daemon_ip_resolution = daemon_ip_resolution_script(daemon_host);
    format!(
        r#"
set -e
if ! command -v iptables >/dev/null 2>&1; then
  exit 43
fi
{daemon_ip_resolution}
iptables -t nat -F OUTPUT || true
iptables -F OUTPUT || true
iptables -P OUTPUT DROP
iptables -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -d 127.0.0.1/8 -j ACCEPT
iptables -A OUTPUT -o lo -j ACCEPT
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT
iptables -A OUTPUT -d "$daemon_ip" -p tcp --dport {daemon_port} -j ACCEPT
iptables -A OUTPUT -m owner --uid-owner {bypass_uid} -j ACCEPT
iptables -t nat -A OUTPUT -m owner --uid-owner {bypass_uid} -j RETURN
iptables -t nat -A OUTPUT -p tcp --dport 80 -j REDIRECT --to-ports {proxy_port}
iptables -t nat -A OUTPUT -p tcp --dport 443 -j REDIRECT --to-ports {proxy_port}
exit 0
"#,
        bypass_uid = EGRESS_PROXY_BYPASS_UID,
    )
}

async fn clear_egress_guard(data_root: &Path, mode: &SandboxCommandMode, name: &str) -> Result<()> {
    let script = r#"
set -e
if ! command -v iptables >/dev/null 2>&1; then
  exit 0
fi
iptables -t nat -F OUTPUT
iptables -F OUTPUT
iptables -P OUTPUT ACCEPT
"#;
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg(name)
        .arg("sh")
        .arg("-c")
        .arg(script);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        Ok(())
    } else {
        let combined = command_output_message(&output);
        if combined.is_empty() {
            anyhow::bail!("failed to clear egress guard (status: {})", output.status);
        }
        anyhow::bail!("failed to clear egress guard: {combined}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_sandbox_contract::ContainerRuntimeKind;

    #[test]
    fn daemon_ip_resolution_uses_literal_ip_without_getent() {
        let script = daemon_ip_resolution_script("192.168.64.1");
        assert_eq!(script, "daemon_ip='192.168.64.1'");
        assert!(!script.contains("getent hosts"));
    }

    #[test]
    fn daemon_ip_resolution_uses_getent_for_hostnames() {
        let script = daemon_ip_resolution_script("host.containers.internal");
        assert!(script.contains("getent hosts 'host.containers.internal'"));
        assert!(script.contains(r#"$1 ~ /^[0-9.]+$/ { print $1; exit }"#));
        assert!(script.contains("exit 44"));
    }

    #[test]
    fn daemon_ip_resolution_rejects_ipv6_literals() {
        let script = daemon_ip_resolution_script("::1");
        assert_eq!(script, "exit 44");
    }

    #[test]
    fn transparent_proxy_policy_maps_llm_only_to_explicit_allowlist_entries() {
        let settings = ContainerExecutionSettings::default();
        let (mode, allowlist) = transparent_proxy_policy(&settings);
        assert_eq!(mode, ContainerNetworkMode::Allowlist);
        assert!(allowlist.iter().any(|entry| entry == "openrouter.ai"));
        assert!(allowlist.iter().any(|entry| entry == "api.openai.com"));
    }

    #[test]
    fn transparent_proxy_policy_preserves_custom_allowlist_mode() {
        let settings = ContainerExecutionSettings {
            network_mode: ContainerNetworkMode::Allowlist,
            allowlist: vec!["example.com".to_string(), "api.example.com".to_string()],
            runtime: ContainerRuntimeKind::NativeContainer,
            ..Default::default()
        };
        let (mode, allowlist) = transparent_proxy_policy(&settings);
        assert_eq!(mode, ContainerNetworkMode::Allowlist);
        assert_eq!(allowlist, settings.allowlist);
    }

    #[test]
    fn transparent_proxy_start_script_writes_log_next_to_config() {
        let script = transparent_proxy_start_script(
            "/usr/local/bin/ctx-egress-proxy",
            "/data/egress-proxy.json",
            "/data/ctx-egress-proxy.log",
            "/tmp/ctx-egress-proxy.pid",
        );

        assert!(script.contains("mkdir -p \"$(dirname '/data/ctx-egress-proxy.log')\""));
        assert!(script.contains(
            "nohup '/usr/local/bin/ctx-egress-proxy' --config '/data/egress-proxy.json' >'/data/ctx-egress-proxy.log' 2>&1 &"
        ));
        assert!(!script.contains(">/tmp/ctx-egress-proxy.log"));
    }

    #[test]
    fn restricted_egress_guard_does_not_grant_uid0_network_bypass() {
        let script = transparent_egress_guard_script(
            TRANSPARENT_PROXY_PORT,
            "host.containers.internal",
            4310,
        );

        assert!(!script.contains("--uid-owner 0"));
        assert!(!script.contains("--uid-owner root"));
    }

    #[test]
    fn restricted_egress_guard_uses_dedicated_proxy_uid_bypass_and_redirects() {
        let script = transparent_egress_guard_script(
            TRANSPARENT_PROXY_PORT,
            "host.containers.internal",
            4310,
        );
        assert_ne!(EGRESS_PROXY_BYPASS_UID, 0);

        assert!(script.contains(&format!(
            "iptables -A OUTPUT -m owner --uid-owner {EGRESS_PROXY_BYPASS_UID} -j ACCEPT"
        )));
        assert!(script.contains(&format!(
            "iptables -t nat -A OUTPUT -m owner --uid-owner {EGRESS_PROXY_BYPASS_UID} -j RETURN"
        )));
        assert!(script.contains(&format!(
            "iptables -t nat -A OUTPUT -p tcp --dport 80 -j REDIRECT --to-ports {TRANSPARENT_PROXY_PORT}"
        )));
        assert!(script.contains(&format!(
            "iptables -t nat -A OUTPUT -p tcp --dport 443 -j REDIRECT --to-ports {TRANSPARENT_PROXY_PORT}"
        )));
    }
}
