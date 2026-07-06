use std::{
    env,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

use crate::config::AppConfig;

pub(super) fn platform_key() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x64"),
        ("windows", "x86_64") => Ok("windows-x64"),
        ("freebsd", "x86_64") => Ok("freebsd-x64"),
        (os, arch) => Err(anyhow!("unsupported ctx upgrade platform: {os}-{arch}")),
    }
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

pub(super) fn version_gt(left: &str, right: &str) -> bool {
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

pub(super) fn auto_mode_is_apply(config: &AppConfig) -> bool {
    config.upgrade.auto.eq_ignore_ascii_case("apply")
}

pub(super) fn env_flag(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

pub(super) fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
