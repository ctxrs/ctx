use std::sync::Arc;

use tokio::sync::broadcast;

#[async_trait::async_trait]
pub trait ProviderChildReclassifierHost: Send + Sync + 'static {
    fn subscribe_shutdown(&self) -> broadcast::Receiver<()>;
    async fn provider_process_pids(&self) -> Vec<u32>;
    fn tool_slice_unit(&self) -> &'static str;
}

// Best-effort background maintenance. On non-Linux hosts we don't have the cgroup primitives
// needed to reclassify provider child processes, so this is a no-op.
pub fn spawn_provider_child_reclassifier<H>(state: Arc<H>)
where
    H: ProviderChildReclassifierHost,
{
    #[cfg(not(target_os = "linux"))]
    {
        let _ = state;
    }

    #[cfg(target_os = "linux")]
    linux::spawn_provider_child_reclassifier(state);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context, Result};
    use sysinfo::{Pid, System};
    use tokio::process::Command;
    use tokio::time::timeout;
    use tokio::time::MissedTickBehavior;

    use crate::provider_child_reclassifier::ProviderChildReclassifierHost;

    const DEFAULT_INTERVAL_MS: u64 = 2_000;
    const DEFAULT_CHILD_LIMIT: usize = 2_000;
    const SYSTEMD_TIMEOUT: Duration = Duration::from_secs(3);

    #[derive(Debug, Clone)]
    struct ReclassifierConfig {
        interval: Duration,
        child_limit: usize,
    }

    impl ReclassifierConfig {
        fn from_env() -> Self {
            let interval_ms =
                env_u64("CTX_TOOL_RECLASSIFIER_INTERVAL_MS").unwrap_or(DEFAULT_INTERVAL_MS);
            let child_limit = env_u64("CTX_TOOL_RECLASSIFIER_CHILD_LIMIT")
                .map(|v| v as usize)
                .unwrap_or(DEFAULT_CHILD_LIMIT);
            Self {
                interval: Duration::from_millis(interval_ms),
                child_limit,
            }
        }

        fn enabled(&self) -> bool {
            !self.interval.is_zero()
        }
    }

    #[derive(Debug)]
    enum Backend {
        SystemdRun,
        CgroupProcs { tool_slice: PathBuf },
        Disabled { reason: String },
    }

    pub(super) fn spawn_provider_child_reclassifier<H>(state: Arc<H>)
    where
        H: ProviderChildReclassifierHost,
    {
        let cfg = ReclassifierConfig::from_env();
        if !cfg.enabled() {
            return;
        }

        let mut shutdown_rx = state.subscribe_shutdown();
        tokio::spawn(async move {
            let mut system = System::new_all();
            let mut classified: HashSet<u32> = HashSet::new();
            let mut backend = detect_backend().await;

            if let Backend::Disabled { reason } = &backend {
                tracing::info!("provider child reclassifier disabled: {reason}");
                return;
            }

            let mut ticker = tokio::time::interval(cfg.interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            if let Err(err) =
                reclassify_once(&state, &mut system, &cfg, &mut classified, &mut backend).await
            {
                tracing::warn!("provider child reclassifier tick failed: {err:#}");
            }

            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => break,
                    _ = ticker.tick() => {
                        if let Err(err) =
                            reclassify_once(&state, &mut system, &cfg, &mut classified, &mut backend).await
                        {
                            tracing::warn!("provider child reclassifier tick failed: {err:#}");
                        }
                    }
                }
            }
        });
    }

    async fn reclassify_once<H>(
        state: &Arc<H>,
        system: &mut System,
        cfg: &ReclassifierConfig,
        classified: &mut HashSet<u32>,
        backend: &mut Backend,
    ) -> Result<()>
    where
        H: ProviderChildReclassifierHost,
    {
        if matches!(backend, Backend::Disabled { .. }) {
            return Ok(());
        }

        let provider_pids = state.provider_process_pids().await;
        if provider_pids.is_empty() {
            classified.clear();
            return Ok(());
        }

        system.refresh_processes();
        let provider_roots: HashSet<u32> = provider_pids.iter().copied().collect();
        let mut child_pids = collect_child_pids(system, &provider_pids, cfg.child_limit);
        child_pids.retain(|pid| !provider_roots.contains(pid));

        classified.retain(|pid| child_pids.contains(pid));

        for pid in child_pids.iter() {
            if classified.contains(pid) {
                continue;
            }
            if pid_in_tool_slice(*pid, state.tool_slice_unit()) {
                classified.insert(*pid);
                continue;
            }
            if let Err(err) = classify_pid(*pid, backend).await {
                tracing::warn!(
                    pid = *pid,
                    "failed to reclassify provider child pid: {err:#}"
                );
                if matches!(backend, Backend::Disabled { .. }) {
                    break;
                }
            } else {
                classified.insert(*pid);
            }
        }

        Ok(())
    }

    fn collect_child_pids(system: &System, provider_pids: &[u32], limit: usize) -> HashSet<u32> {
        if provider_pids.is_empty() || limit == 0 {
            return HashSet::new();
        }

        let mut task_pids = HashSet::new();
        for (pid, process) in system.processes() {
            if let Some(tasks) = process.tasks() {
                for task_pid in tasks {
                    if task_pid != pid {
                        task_pids.insert(*task_pid);
                    }
                }
            }
        }

        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in system.processes() {
            if task_pids.contains(pid) {
                continue;
            }
            if let Some(parent) = process.parent() {
                if task_pids.contains(&parent) {
                    continue;
                }
                children.entry(parent).or_default().push(*pid);
            }
        }

        let mut output = HashSet::new();
        let mut stack: Vec<Pid> = provider_pids
            .iter()
            .map(|pid| Pid::from_u32(*pid))
            .collect();
        while let Some(parent) = stack.pop() {
            if let Some(kids) = children.get(&parent) {
                for child in kids {
                    let child_pid = child.as_u32();
                    if output.insert(child_pid) {
                        if output.len() >= limit {
                            return output;
                        }
                        stack.push(*child);
                    }
                }
            }
        }
        output
    }

    fn pid_in_tool_slice(pid: u32, tool_slice_unit: &str) -> bool {
        let path = format!("/proc/{pid}/cgroup");
        std::fs::read_to_string(path)
            .ok()
            .map(|contents| contents.lines().any(|line| line.contains(tool_slice_unit)))
            .unwrap_or(false)
    }

    async fn classify_pid(pid: u32, backend: &mut Backend) -> Result<()> {
        match backend {
            Backend::SystemdRun => classify_pid_systemd_run(pid).await,
            Backend::CgroupProcs { tool_slice } => classify_pid_cgroup_procs(pid, tool_slice).await,
            Backend::Disabled { .. } => Ok(()),
        }
    }

    async fn detect_backend() -> Backend {
        if systemd_user_available().await && systemd_run_supports_pid().await {
            return Backend::SystemdRun;
        }
        if let Some(tool_slice) = tool_slice_path().await {
            return Backend::CgroupProcs { tool_slice };
        }
        Backend::Disabled {
            reason: "no supported backend".to_string(),
        }
    }

    async fn classify_pid_systemd_run(pid: u32) -> Result<()> {
        if !systemd_run_supports_pid().await {
            anyhow::bail!("systemd-run lacks --pid");
        }

        let mut cmd = Command::new("systemd-run");
        cmd.arg("--user")
            .arg("--quiet")
            .arg("--scope")
            .arg("--slice=app-ctx-tools.slice")
            .arg(format!("--pid={pid}"));
        let status = timeout(SYSTEMD_TIMEOUT, cmd.status())
            .await
            .context("timed out running systemd-run")??;
        if !status.success() {
            anyhow::bail!("systemd-run exited with status {status}");
        }
        Ok(())
    }

    async fn classify_pid_cgroup_procs(pid: u32, tool_slice: &Path) -> Result<()> {
        let cgroup_procs = tool_slice.join("cgroup.procs");
        tokio::fs::write(&cgroup_procs, format!("{pid}\n"))
            .await
            .with_context(|| format!("write {}", cgroup_procs.display()))?;
        Ok(())
    }

    async fn systemd_user_available() -> bool {
        let mut cmd = Command::new("systemctl");
        cmd.arg("--user").arg("show-environment");
        matches!(
            timeout(Duration::from_secs(2), cmd.output()).await,
            Ok(Ok(output)) if output.status.success()
        )
    }

    async fn systemd_run_supports_pid() -> bool {
        let mut cmd = Command::new("systemd-run");
        cmd.arg("--help");
        match timeout(Duration::from_secs(2), cmd.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).contains("--pid")
            }
            _ => false,
        }
    }

    async fn tool_slice_path() -> Option<PathBuf> {
        let base = Path::new("/sys/fs/cgroup/user.slice");
        if !base.exists() {
            return None;
        }

        let uid = current_uid()?;
        let user_slice = base.join(format!("user-{uid}.slice"));
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
        let session_path = runtime_dir.and_then(|dir| {
            Path::new(&dir)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|session| user_slice.join(format!("session-{session}.scope")))
        });

        let candidates = [
            session_path
                .as_ref()
                .map(|scope| scope.join("app-ctx-tools.slice")),
            Some(user_slice.join("app.slice").join("app-ctx-tools.slice")),
            Some(user_slice.join("app-ctx-tools.slice")),
        ];

        candidates
            .into_iter()
            .flatten()
            .find(|candidate| candidate.join("cgroup.procs").exists())
    }

    fn current_uid() -> Option<u32> {
        parse_uid_from_runtime_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
            .or_else(|| parse_uid_from_env(std::env::var("UID").ok().as_deref()))
            .or_else(|| parse_uid_from_status(&std::fs::read_to_string("/proc/self/status").ok()?))
    }

    fn parse_uid_from_runtime_dir(runtime_dir: Option<&str>) -> Option<u32> {
        runtime_dir
            .and_then(|dir| dir.strip_prefix("/run/user/"))
            .and_then(|suffix| suffix.split('/').next())
            .and_then(|value| value.parse::<u32>().ok())
    }

    fn parse_uid_from_env(value: Option<&str>) -> Option<u32> {
        value.and_then(|raw| raw.trim().parse::<u32>().ok())
    }

    fn parse_uid_from_status(status: &str) -> Option<u32> {
        status.lines().find_map(|line| {
            let rest = line.strip_prefix("Uid:")?;
            rest.split_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok())
        })
    }

    fn env_u64(key: &str) -> Option<u64> {
        std::env::var(key)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
    }

    #[cfg(test)]
    mod tests {
        use super::{parse_uid_from_env, parse_uid_from_runtime_dir, parse_uid_from_status};

        #[test]
        fn parses_uid_from_runtime_dir() {
            assert_eq!(parse_uid_from_runtime_dir(Some("/run/user/501")), Some(501));
            assert_eq!(
                parse_uid_from_runtime_dir(Some("/run/user/1000/systemd")),
                Some(1000)
            );
            assert_eq!(parse_uid_from_runtime_dir(Some("/tmp/runtime")), None);
        }

        #[test]
        fn parses_uid_from_env_value() {
            assert_eq!(parse_uid_from_env(Some("1001")), Some(1001));
            assert_eq!(parse_uid_from_env(Some(" 1002 ")), Some(1002));
            assert_eq!(parse_uid_from_env(Some("nope")), None);
        }

        #[test]
        fn parses_uid_from_proc_status() {
            let status = "Name:\tctx\nUid:\t501\t501\t501\t501\nGid:\t20\t20\t20\t20\n";
            assert_eq!(parse_uid_from_status(status), Some(501));
            assert_eq!(parse_uid_from_status("Name:\tctx\n"), None);
        }
    }
}
