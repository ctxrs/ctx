use std::net::TcpListener;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub(super) fn to_err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

pub(super) fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::Prefix(p) => out.push(p.as_os_str()),
            std::path::Component::RootDir => out.push(std::path::MAIN_SEPARATOR_STR),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Normal(x) => out.push(x),
        }
    }
    out
}

pub(super) fn expand_tilde(raw: &str) -> Option<PathBuf> {
    if raw == "~" || raw.starts_with("~/") {
        let base = directories::BaseDirs::new()?;
        let home = base.home_dir();
        if raw == "~" {
            Some(home.to_path_buf())
        } else {
            Some(home.join(raw.trim_start_matches("~/")))
        }
    } else {
        None
    }
}

pub(super) fn pick_unused_local_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding ephemeral port")?;
    let port = listener.local_addr().context("reading local addr")?.port();
    Ok(port)
}

pub(super) const SSH_TUNNEL_HEALTH_RETRIES: usize = 12;
pub(super) const SSH_TUNNEL_HEALTH_BASE_DELAY_MS: u64 = 150;
pub(super) const LOCAL_DAEMON_HEALTH_RETRIES: usize = 20;
pub(super) const LOCAL_DAEMON_HEALTH_BASE_DELAY_MS: u64 = 100;
pub(super) const DESKTOP_DAEMON_DATA_DIR_ENV: &str = "CTX_DESKTOP_DAEMON_DATA_DIR";
pub(super) const DAEMON_ENV_PASSTHROUGH: &[&str] = &[
    "CTX_AVF_LINUX_HELPER_PATH",
    "CTX_AVF_LINUX_GUEST_RUNTIME_DIR",
    "CTX_HARNESS_SANDBOX_CLI_PATH",
    "CTX_SANDBOX_PREFETCH",
    "CTX_BUNDLE_MANIFEST",
];
