#[cfg(target_os = "linux")]
use anyhow::Context;
#[cfg(target_os = "linux")]
use anyhow::Result;
use chrono::{DateTime, Utc};
use ctx_settings_model::{
    PublicResourceGovernanceLimits, PublicResourceGovernanceSettings,
    PublicResourceGovernanceStatus, ResourceGovernanceMode, ResourceGovernanceSettings,
    ResourceGovernanceStatusState,
};
#[cfg(target_os = "linux")]
use tokio::process::Command;
#[cfg(target_os = "linux")]
use tokio::time::timeout;

use crate::SystemSnapshot;

#[cfg(target_os = "linux")]
const SYSTEMD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
#[cfg(target_os = "linux")]
const SCOPE_UNIT_BASE: &str = "ctx-daemon";
#[cfg(target_os = "linux")]
const SCOPE_UNIT: &str = "ctx-daemon.scope";

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveResourceLimits {
    pub cpu_quota_pct: u32,
    pub memory_high_mb: u32,
    pub memory_max_mb: u32,
}

#[derive(Debug, Clone)]
pub struct ResourceGovernanceRuntime {
    pub last_applied: Option<EffectiveResourceLimits>,
    pub last_state: ResourceGovernanceStatusState,
    pub last_message: Option<String>,
    pub last_applied_at: Option<DateTime<Utc>>,
    pub requires_restart: bool,
}

impl Default for ResourceGovernanceRuntime {
    fn default() -> Self {
        Self {
            last_applied: None,
            last_state: ResourceGovernanceStatusState::Disabled,
            last_message: None,
            last_applied_at: None,
            requires_restart: false,
        }
    }
}

pub fn compute_effective_limits(
    settings: &ResourceGovernanceSettings,
    system: &SystemSnapshot,
    cpu_count: usize,
) -> Option<EffectiveResourceLimits> {
    if !settings.enabled {
        return None;
    }

    let cores = cpu_count.max(1) as u32;
    let total_mb = (system.memory_total_bytes / (1024 * 1024)).max(1);
    let reserve_mb = if total_mb >= 16384 {
        4096
    } else if total_mb >= 8192 {
        2048
    } else {
        1024
    };
    let auto_max_mb = total_mb.saturating_sub(reserve_mb).max(512);
    let auto_high_mb = ((auto_max_mb as f64) * 0.9).round() as u64;
    let auto_high_mb = auto_high_mb.min(auto_max_mb);
    let auto_cpu_pct = if cores > 2 {
        (cores - 1) * 100
    } else {
        cores * 100
    };

    let mut cpu_quota_pct = match settings.mode {
        ResourceGovernanceMode::Auto => auto_cpu_pct,
        ResourceGovernanceMode::Custom => settings.cpu_quota_pct.unwrap_or(auto_cpu_pct),
    };
    let mut memory_high_mb = match settings.mode {
        ResourceGovernanceMode::Auto => auto_high_mb as u32,
        ResourceGovernanceMode::Custom => settings.memory_high_mb.unwrap_or(auto_high_mb as u32),
    };
    let mut memory_max_mb = match settings.mode {
        ResourceGovernanceMode::Auto => auto_max_mb as u32,
        ResourceGovernanceMode::Custom => settings.memory_max_mb.unwrap_or(auto_max_mb as u32),
    };

    let max_cpu_pct = cores.saturating_mul(100).max(100);
    cpu_quota_pct = cpu_quota_pct.clamp(50, max_cpu_pct);

    if memory_max_mb > total_mb as u32 {
        memory_max_mb = total_mb as u32;
    }
    if memory_high_mb > memory_max_mb {
        memory_high_mb = memory_max_mb;
    }
    if memory_high_mb == 0 {
        memory_high_mb = 256;
    }
    if memory_max_mb == 0 {
        memory_max_mb = 512;
    }

    Some(EffectiveResourceLimits {
        cpu_quota_pct,
        memory_high_mb,
        memory_max_mb,
    })
}

pub fn status_for(
    enabled: bool,
    effective: Option<&EffectiveResourceLimits>,
    runtime: &ResourceGovernanceRuntime,
) -> PublicResourceGovernanceStatus {
    let can_apply_now = cfg!(target_os = "linux")
        && runtime.last_state != ResourceGovernanceStatusState::Unsupported;
    if !enabled {
        return PublicResourceGovernanceStatus {
            state: ResourceGovernanceStatusState::Disabled,
            can_apply_now: false,
            requires_restart: false,
            message: None,
        };
    }

    if runtime.last_state == ResourceGovernanceStatusState::Unsupported {
        return PublicResourceGovernanceStatus {
            state: ResourceGovernanceStatusState::Unsupported,
            can_apply_now: false,
            requires_restart: false,
            message: runtime.last_message.clone(),
        };
    }

    if runtime.last_state == ResourceGovernanceStatusState::Error {
        return PublicResourceGovernanceStatus {
            state: ResourceGovernanceStatusState::Error,
            can_apply_now,
            requires_restart: false,
            message: runtime.last_message.clone(),
        };
    }

    let applied = runtime
        .last_applied
        .as_ref()
        .and_then(|last| effective.map(|cur| last == cur))
        .unwrap_or(false);

    PublicResourceGovernanceStatus {
        state: if applied {
            ResourceGovernanceStatusState::Applied
        } else {
            ResourceGovernanceStatusState::Pending
        },
        can_apply_now,
        requires_restart: runtime.requires_restart,
        message: runtime.last_message.clone(),
    }
}

pub fn public_settings(
    settings: &ResourceGovernanceSettings,
    effective: Option<&EffectiveResourceLimits>,
    status: PublicResourceGovernanceStatus,
) -> PublicResourceGovernanceSettings {
    PublicResourceGovernanceSettings {
        enabled: settings.enabled,
        mode: settings.mode.clone(),
        cpu_quota_pct: settings.cpu_quota_pct,
        memory_high_mb: settings.memory_high_mb,
        memory_max_mb: settings.memory_max_mb,
        effective: effective.map(|lim| PublicResourceGovernanceLimits {
            cpu_quota_pct: lim.cpu_quota_pct,
            memory_high_mb: lim.memory_high_mb,
            memory_max_mb: lim.memory_max_mb,
        }),
        status: Some(status),
    }
}

#[cfg(target_os = "linux")]
fn is_in_scope(scope: &str) -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .map(|s| s.lines().any(|line| line.contains(scope)))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
async fn run_command(cmd: &mut Command, label: &str) -> Result<()> {
    let output = timeout(SYSTEMD_TIMEOUT, cmd.output())
        .await
        .context("command timed out")?
        .with_context(|| format!("running {label}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::bail!(
        "{label} failed ({}): {}{}",
        output.status,
        stderr,
        if stdout.trim().is_empty() {
            String::new()
        } else {
            format!(" ({stdout})")
        }
    );
}

#[cfg(target_os = "linux")]
async fn systemd_run_supports_pid() -> bool {
    let mut cmd = Command::new("systemd-run");
    cmd.arg("--help");
    let output = timeout(SYSTEMD_TIMEOUT, cmd.output()).await;
    let Ok(Ok(output)) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let help = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_lowercase();
    help.contains("--pid")
}

#[cfg(target_os = "linux")]
async fn attach_scope(pid: u32, limits: &EffectiveResourceLimits) -> Result<()> {
    if !systemd_run_supports_pid().await {
        anyhow::bail!(
            "systemd-run lacks --pid; start the daemon via systemd-run or upgrade systemd (>= 256)"
        );
    }

    let mut cmd = Command::new("systemd-run");
    cmd.arg("--user")
        .arg("--scope")
        .arg("--unit")
        .arg(SCOPE_UNIT_BASE)
        .arg("--property")
        .arg(format!("CPUQuota={}%", limits.cpu_quota_pct))
        .arg("--property")
        .arg(format!("MemoryHigh={}M", limits.memory_high_mb))
        .arg("--property")
        .arg(format!("MemoryMax={}M", limits.memory_max_mb))
        .arg("--pid")
        .arg(pid.to_string());
    run_command(&mut cmd, "systemd-run").await
}

#[cfg(target_os = "linux")]
async fn set_scope_properties(limits: &EffectiveResourceLimits) -> Result<()> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("--user")
        .arg("set-property")
        .arg("--runtime")
        .arg(SCOPE_UNIT)
        .arg(format!("CPUQuota={}%", limits.cpu_quota_pct))
        .arg(format!("MemoryHigh={}M", limits.memory_high_mb))
        .arg(format!("MemoryMax={}M", limits.memory_max_mb));
    run_command(&mut cmd, "systemctl set-property").await
}

pub async fn apply_limits(
    _pid: u32,
    limits: &EffectiveResourceLimits,
    _has_running_children: bool,
) -> ResourceGovernanceRuntime {
    let mut runtime = ResourceGovernanceRuntime {
        last_applied: Some(limits.clone()),
        ..ResourceGovernanceRuntime::default()
    };

    #[cfg(not(target_os = "linux"))]
    {
        runtime.last_state = ResourceGovernanceStatusState::Unsupported;
        runtime.last_message =
            Some("Resource governance is not supported on this OS yet.".to_string());
        runtime
    }

    #[cfg(target_os = "linux")]
    {
        let was_in_scope = is_in_scope(SCOPE_UNIT);
        if !was_in_scope {
            if let Err(err) = attach_scope(_pid, limits).await {
                if !is_in_scope(SCOPE_UNIT) {
                    runtime.last_state = ResourceGovernanceStatusState::Unsupported;
                    runtime.last_message = Some(format!("{err:#}"));
                    return runtime;
                }
            }
        }

        if let Err(err) = set_scope_properties(limits).await {
            runtime.last_state = ResourceGovernanceStatusState::Error;
            runtime.last_message = Some(format!("{err:#}"));
            return runtime;
        }

        runtime.last_state = ResourceGovernanceStatusState::Applied;
        runtime.last_applied_at = Some(Utc::now());
        runtime.requires_restart = !was_in_scope && _has_running_children;
        if runtime.requires_restart {
            runtime.last_message =
                Some("Restart to apply limits to existing processes.".to_string());
        }
        runtime
    }
}

#[cfg(test)]
mod tests {
    use ctx_settings_model::{
        ResourceGovernanceMode, ResourceGovernanceSettings, ResourceGovernanceStatusState,
    };

    use super::{
        compute_effective_limits, status_for, EffectiveResourceLimits, ResourceGovernanceRuntime,
    };
    use crate::SystemSnapshot;

    fn system(memory_total_bytes: u64) -> SystemSnapshot {
        SystemSnapshot {
            cpu_pct: 0.0,
            memory_total_bytes,
            memory_used_bytes: 0,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
        }
    }

    #[test]
    fn compute_effective_limits_uses_auto_reserve_and_cpu_policy() {
        let settings = ResourceGovernanceSettings {
            enabled: true,
            mode: ResourceGovernanceMode::Auto,
            cpu_quota_pct: None,
            memory_high_mb: None,
            memory_max_mb: None,
        };

        let limits = compute_effective_limits(&settings, &system(32 * 1024 * 1024 * 1024), 8)
            .expect("limits");

        assert_eq!(limits.cpu_quota_pct, 700);
        assert_eq!(limits.memory_max_mb, 28 * 1024);
        assert_eq!(limits.memory_high_mb, 25_805);
    }

    #[test]
    fn compute_effective_limits_clamps_custom_values() {
        let settings = ResourceGovernanceSettings {
            enabled: true,
            mode: ResourceGovernanceMode::Custom,
            cpu_quota_pct: Some(10),
            memory_high_mb: Some(10_000),
            memory_max_mb: Some(20_000),
        };

        let limits = compute_effective_limits(&settings, &system(4 * 1024 * 1024 * 1024), 1)
            .expect("limits");

        assert_eq!(limits.cpu_quota_pct, 50);
        assert_eq!(limits.memory_max_mb, 4096);
        assert_eq!(limits.memory_high_mb, 4096);
    }

    #[test]
    fn status_for_reports_applied_only_when_limits_match() {
        let limits = EffectiveResourceLimits {
            cpu_quota_pct: 100,
            memory_high_mb: 512,
            memory_max_mb: 768,
        };
        let runtime = ResourceGovernanceRuntime {
            last_applied: Some(limits.clone()),
            last_state: ResourceGovernanceStatusState::Applied,
            last_message: None,
            last_applied_at: None,
            requires_restart: true,
        };

        let status = status_for(true, Some(&limits), &runtime);
        assert_eq!(status.state, ResourceGovernanceStatusState::Applied);
        assert!(status.requires_restart);

        let different = EffectiveResourceLimits {
            memory_max_mb: 1024,
            ..limits
        };
        let status = status_for(true, Some(&different), &runtime);
        assert_eq!(status.state, ResourceGovernanceStatusState::Pending);
    }
}
