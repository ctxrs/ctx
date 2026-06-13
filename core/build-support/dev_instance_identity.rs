use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn resolve_dev_instance_id(manifest_dir: &Path) -> String {
    if let Some(explicit) = trimmed_env("CTX_DEV_INSTANCE_ID") {
        return explicit;
    }
    stable_dev_instance_id(manifest_dir)
}

fn stable_dev_instance_id(cwd: &Path) -> String {
    let root = shared_dev_instance_root(cwd);
    let canonical = std::fs::canonicalize(&root).unwrap_or(root);
    let mut key = canonical.to_string_lossy().to_string();
    if cfg!(windows) {
        key = key.to_ascii_lowercase();
    }
    format!("dev-{:016x}", fnv1a64(key.as_bytes()))
}

fn shared_dev_instance_root(cwd: &Path) -> PathBuf {
    let default_root = git_repo_root(cwd)
        .or_else(|| find_cargo_workspace_root(cwd))
        .unwrap_or_else(|| cwd.to_path_buf());

    if let Some(raw_override) = trimmed_env("CTX_DEV_INSTANCE_ROOT") {
        let override_path = PathBuf::from(raw_override);
        if override_path.is_absolute() {
            return override_path;
        }
        return default_root.join(override_path);
    }
    default_root
}

fn find_cargo_workspace_root(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let cargo_toml = dir.join("Cargo.toml");
        let Ok(raw) = std::fs::read_to_string(&cargo_toml) else {
            continue;
        };
        if raw.contains("[workspace]") {
            return Some(dir.to_path_buf());
        }
    }
    None
}

fn git_repo_root(cwd: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

fn trimmed_env(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
