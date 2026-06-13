use super::*;

#[cfg(target_os = "linux")]
fn systemd_run_available() -> bool {
    match Command::new("systemd-run").arg("--version").status() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn systemd_user_available() -> bool {
    match Command::new("systemctl")
        .arg("--user")
        .arg("show-environment")
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn should_use_systemd_scope() -> bool {
    systemd_run_available() && systemd_user_available()
}

#[cfg(not(target_os = "linux"))]
pub(super) fn should_use_systemd_scope() -> bool {
    false
}

pub(in super::super) fn stop_systemd_scope(_unit: &str) {
    #[cfg(target_os = "linux")]
    {
        let scope = if _unit.ends_with(".scope") {
            _unit.to_string()
        } else {
            format!("{_unit}.scope")
        };
        let _ = Command::new("systemctl")
            .arg("--user")
            .arg("stop")
            .arg(&scope)
            .status();
        let _ = Command::new("systemctl")
            .arg("--user")
            .arg("reset-failed")
            .arg(&scope)
            .status();
    }
}

pub(in super::super) fn systemd_scope_for_local_daemon_url(base_url: &str) -> Option<String> {
    let url = Url::parse(base_url).ok()?;
    let port = url.port()?;
    Some(format!("ctx-daemon-{port}"))
}
