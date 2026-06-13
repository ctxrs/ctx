#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::process::Command;

use ctx_resource_utilization::resource_governance::{apply_limits, EffectiveResourceLimits};
use ctx_settings_model::ResourceGovernanceStatusState;

const SCOPE_UNIT: &str = "ctx-daemon.scope";
const REEXEC_ENV: &str = "CTX_SYSTEMD_E2E_REEXEC";

async fn systemd_user_available() -> bool {
    if which::which("systemctl").is_err() || which::which("systemd-run").is_err() {
        return false;
    }

    Command::new("systemctl")
        .arg("--user")
        .arg("show-environment")
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn scope_is_active() -> Result<bool> {
    let status = Command::new("systemctl")
        .arg("--user")
        .arg("is-active")
        .arg("--quiet")
        .arg(SCOPE_UNIT)
        .status()
        .await
        .context("systemctl is-active failed")?;

    if status.success() {
        return Ok(true);
    }

    match status.code() {
        Some(3) | Some(4) => Ok(false),
        _ => anyhow::bail!("unexpected systemctl is-active exit: {:?}", status.code()),
    }
}

fn is_in_scope() -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .map(|s| s.lines().any(|line| line.contains(SCOPE_UNIT)))
        .unwrap_or(false)
}

async fn systemd_run_supports_pid() -> bool {
    let output = Command::new("systemd-run").arg("--help").output().await;
    let Ok(output) = output else {
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

async fn systemctl_show(unit: &str, prop: &str) -> Result<String> {
    let output = Command::new("systemctl")
        .arg("--user")
        .arg("show")
        .arg(unit)
        .arg("-p")
        .arg(prop)
        .output()
        .await
        .with_context(|| format!("systemctl show {unit}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "systemctl show {unit} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix(&format!("{prop}=")) {
            return Ok(value.trim().to_string());
        }
    }

    anyhow::bail!("missing {prop} in systemctl show output")
}

fn cgroup_v2_enabled() -> bool {
    Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
}

fn read_trimmed(path: &Path) -> Result<String> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(contents.trim().to_string())
}

fn read_mem_total_mb() -> Result<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").context("reading /proc/meminfo")?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = rest
                .split_whitespace()
                .next()
                .context("missing MemTotal value")?
                .parse::<u64>()
                .context("parsing MemTotal")?;
            return Ok(kb / 1024);
        }
    }
    anyhow::bail!("MemTotal not found in /proc/meminfo")
}

fn read_cpu_stat(cgroup_path: &Path) -> Result<(u64, u64)> {
    let stat_path = cgroup_path.join("cpu.stat");
    let contents = read_trimmed(&stat_path)?;
    let mut throttled = 0;
    let mut throttled_usec = 0;
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or_default();
        let value = parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0);
        match key {
            "nr_throttled" => throttled = value,
            "throttled_usec" => throttled_usec = value,
            _ => {}
        }
    }
    Ok((throttled, throttled_usec))
}

fn burn_cpu(duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        std::hint::spin_loop();
    }
}

#[tokio::test]
#[ignore]
async fn resource_governance_systemd_limits_apply_and_throttle() -> Result<()> {
    if !systemd_user_available().await {
        eprintln!("skipping: no systemd user session or systemd tools");
        return Ok(());
    }

    if !cgroup_v2_enabled() {
        eprintln!("skipping: cgroup v2 not available");
        return Ok(());
    }

    match scope_is_active().await {
        Ok(true) => {
            eprintln!("skipping: {SCOPE_UNIT} already active");
            return Ok(());
        }
        Ok(false) => {}
        Err(err) => {
            eprintln!("skipping: could not inspect scope status ({err:#})");
            return Ok(());
        }
    }

    if !is_in_scope() && std::env::var(REEXEC_ENV).is_err() && !systemd_run_supports_pid().await {
        let exe = std::env::current_exe().context("locating test binary")?;
        let status = Command::new("systemd-run")
            .arg("--user")
            .arg("--scope")
            .arg("--unit")
            .arg("ctx-daemon")
            .arg("--same-dir")
            .arg(exe)
            .arg("--exact")
            .arg("resource_governance_systemd_limits_apply_and_throttle")
            .arg("--ignored")
            .env(REEXEC_ENV, "1")
            .status()
            .await
            .context("reexec under systemd-run")?;
        assert!(status.success(), "systemd-run reexec failed with {status}");
        return Ok(());
    }

    let mem_total_mb = read_mem_total_mb().unwrap_or(1024).max(512);
    let mut memory_max_mb = (mem_total_mb * 3 / 4).max(256);
    if memory_max_mb > mem_total_mb {
        memory_max_mb = mem_total_mb;
    }
    let mut memory_high_mb = (memory_max_mb * 9 / 10).max(128);
    if memory_high_mb > memory_max_mb {
        memory_high_mb = memory_max_mb;
    }

    let limits = EffectiveResourceLimits {
        cpu_quota_pct: 50,
        memory_high_mb: u32::try_from(memory_high_mb).unwrap_or(512),
        memory_max_mb: u32::try_from(memory_max_mb).unwrap_or(1024),
    };

    let runtime = apply_limits(std::process::id(), &limits, false).await;
    assert_eq!(
        runtime.last_state,
        ResourceGovernanceStatusState::Applied,
        "expected Applied, got {:?}: {}",
        runtime.last_state,
        runtime
            .last_message
            .unwrap_or_else(|| "unknown reason".to_string())
    );

    tokio::time::sleep(Duration::from_millis(200)).await;

    let control_group = systemctl_show(SCOPE_UNIT, "ControlGroup").await?;
    let cgroup_rel = control_group.trim_start_matches('/');
    let cgroup_path = PathBuf::from("/sys/fs/cgroup").join(cgroup_rel);
    assert!(
        cgroup_path.exists(),
        "cgroup path missing: {}",
        cgroup_path.display()
    );

    let self_cgroup = read_trimmed(Path::new("/proc/self/cgroup"))?;
    assert!(
        self_cgroup.contains(SCOPE_UNIT),
        "process not attached to scope: {self_cgroup}"
    );

    let cpu_max = read_trimmed(&cgroup_path.join("cpu.max"))?;
    let cpu_quota = cpu_max.split_whitespace().next().unwrap_or("max");
    assert_ne!(cpu_quota, "max", "cpu.max is unlimited");

    let expected_high_bytes = u64::from(limits.memory_high_mb) * 1024 * 1024;
    let expected_max_bytes = u64::from(limits.memory_max_mb) * 1024 * 1024;
    let memory_high = read_trimmed(&cgroup_path.join("memory.high"))?;
    let memory_max = read_trimmed(&cgroup_path.join("memory.max"))?;
    let memory_high_bytes = memory_high.parse::<u64>().context("parse memory.high")?;
    let memory_max_bytes = memory_max.parse::<u64>().context("parse memory.max")?;
    assert_eq!(memory_high_bytes, expected_high_bytes);
    assert_eq!(memory_max_bytes, expected_max_bytes);

    let (before_throttled, before_throttled_usec) = read_cpu_stat(&cgroup_path)?;
    let mut handles = Vec::new();
    for _ in 0..2 {
        handles.push(std::thread::spawn(|| burn_cpu(Duration::from_millis(1200))));
    }
    for handle in handles {
        let _ = handle.join();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (after_throttled, after_throttled_usec) = read_cpu_stat(&cgroup_path)?;

    assert!(
        after_throttled > before_throttled || after_throttled_usec > before_throttled_usec,
        "expected CPU throttling but stats did not change"
    );

    Ok(())
}
