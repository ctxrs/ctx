mod command;
mod install;
mod metadata;
mod path;
mod state;

pub use command::{maybe_spawn_auto_upgrade, run, UpgradeArgs};

use std::env;

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
struct UpgradePlan {
    pub(super) current_version: String,
    pub(super) latest_version: String,
    pub(super) channel: String,
    pub(super) platform: String,
    pub(super) metadata_url: String,
    pub(super) artifact_url: String,
    pub(super) artifact_sha256: String,
    pub(super) install_path: std::path::PathBuf,
    pub(super) update_available: bool,
    pub(super) managed: bool,
    pub(super) warnings: Vec<String>,
    pub(super) path: path::PathDiagnostics,
    pub(super) metadata: metadata::ReleaseMetadata,
}

fn platform_key() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x64"),
        ("windows", "x86_64") => Ok("windows-x64"),
        ("freebsd", "x86_64") => Ok("freebsd-x64"),
        (os, arch) => Err(anyhow!("unsupported ctx upgrade platform: {os}-{arch}")),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn env_flag(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

fn version_gt(left: &str, right: &str) -> bool {
    let left = version_parts(left);
    let right = version_parts(right);
    left > right
}

fn version_parts(value: &str) -> Vec<u64> {
    value
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .take(4)
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}
