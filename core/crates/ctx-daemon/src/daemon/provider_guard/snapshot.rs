use crate::daemon::provider_capability_hosts::ProviderLifecycleBackgroundHost;

pub(super) async fn capture_guard_snapshot(
    state: &ProviderLifecycleBackgroundHost,
    event: &ctx_provider_runtime::provider_guard::ProviderGuardEvent,
) {
    #[cfg(target_os = "linux")]
    {
        let timestamp_ms = unix_ms_now();
        let dir = state.data_root().join("logs").join("providers");
        let path = dir.join(format!(
            "provider-guard-{}-{}-{}-{}.log",
            event.sample.label, event.sample.pid, event.stage, timestamp_ms
        ));
        if tokio::fs::create_dir_all(&dir).await.is_err() {
            return;
        }

        let mut output = String::new();
        output.push_str(&format!("event={}\n", event.stage));
        output.push_str(&format!("pid={}\n", event.sample.pid));
        output.push_str(&format!("label={}\n", event.sample.label));
        output.push_str(&format!("memory_bytes={}\n", event.sample.memory_bytes));
        output.push_str(&format!("timestamp_ms={timestamp_ms}\n\n"));

        let status_path = format!("/proc/{}/status", event.sample.pid);
        match tokio::fs::read_to_string(&status_path).await {
            Ok(status) => {
                output.push_str("== /proc/pid/status ==\n");
                output.push_str(&status);
                output.push('\n');
            }
            Err(err) => {
                output.push_str("== /proc/pid/status ==\n");
                output.push_str(&format!("error={err:#}\n\n"));
            }
        }

        let smaps_path = format!("/proc/{}/smaps_rollup", event.sample.pid);
        match tokio::fs::read_to_string(&smaps_path).await {
            Ok(smaps) => {
                output.push_str("== /proc/pid/smaps_rollup ==\n");
                output.push_str(&smaps);
                output.push('\n');
            }
            Err(err) => {
                output.push_str("== /proc/pid/smaps_rollup ==\n");
                output.push_str(&format!("error={err:#}\n\n"));
            }
        }

        let cmdline_path = format!("/proc/{}/cmdline", event.sample.pid);
        match tokio::fs::read(&cmdline_path).await {
            Ok(cmdline) => {
                let printable = cmdline
                    .split(|b| *b == 0)
                    .filter_map(|part| std::str::from_utf8(part).ok())
                    .collect::<Vec<_>>()
                    .join(" ");
                output.push_str("== /proc/pid/cmdline ==\n");
                output.push_str(&printable);
                output.push('\n');
            }
            Err(err) => {
                output.push_str("== /proc/pid/cmdline ==\n");
                output.push_str(&format!("error={err:#}\n"));
            }
        }

        if tokio::fs::write(&path, output).await.is_ok() {
            tracing::info!(
                provider_id = %event.sample.label,
                pid = event.sample.pid,
                path = %path.display(),
                "captured provider guard snapshot"
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, event);
    }
}

#[cfg(target_os = "linux")]
fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
