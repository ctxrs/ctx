use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

mod dev_instance_identity {
    include!("../../build-support/dev_instance_identity.rs");
}

fn main() {
    emit_build_identity();
}

fn emit_build_identity() {
    println!("cargo:rerun-if-env-changed=CTX_BUILD_ID");
    println!("cargo:rerun-if-env-changed=CTX_COMPATIBILITY_TOKEN");
    println!("cargo:rerun-if-env-changed=CTX_DEV_INSTANCE_ID");
    println!("cargo:rerun-if-env-changed=CTX_DEV_INSTANCE_ROOT");
    println!("cargo:rerun-if-env-changed=CTX_RELEASE_EFFECTIVE_VERSION");
    println!("cargo:rerun-if-env-changed=RELEASE_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    emit_git_rerun_hints(&manifest_dir);

    let exact_version = explicit_env_value("CTX_RELEASE_EFFECTIVE_VERSION")
        .or_else(|| explicit_env_value("RELEASE_VERSION"))
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string()));
    println!("cargo:rustc-env=CTX_RELEASE_EFFECTIVE_VERSION={exact_version}");

    let compatibility_token = explicit_env_value("CTX_COMPATIBILITY_TOKEN")
        .or_else(|| explicit_env_value("CTX_DEV_INSTANCE_ID"))
        .unwrap_or_else(|| dev_instance_identity::resolve_dev_instance_id(&manifest_dir));
    println!("cargo:rustc-env=CTX_COMPATIBILITY_TOKEN={compatibility_token}");
    println!("cargo:rustc-env=CTX_DEV_INSTANCE_ID={compatibility_token}");

    if let Ok(explicit) = env::var("CTX_BUILD_ID") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            println!("cargo:rustc-env=CTX_BUILD_ID={trimmed}");
            return;
        }
    }
    let build_id = git_head_build_id(&manifest_dir)
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string()));
    println!("cargo:rustc-env=CTX_BUILD_ID={build_id}");
}

fn explicit_env_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn git_head_build_id(cwd: &Path) -> Option<String> {
    let head = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !head.status.success() {
        return None;
    }
    let mut id = String::from_utf8(head.stdout).ok()?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let dirty = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .map(|out| out.status.success() && !String::from_utf8_lossy(&out.stdout).trim().is_empty())
        .unwrap_or(false);
    if dirty {
        id.push_str("-dirty");
    }
    Some(id)
}

fn emit_git_rerun_hints(cwd: &Path) {
    let git_dir_out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .output();
    let Ok(git_dir_out) = git_dir_out else {
        return;
    };
    if !git_dir_out.status.success() {
        return;
    }
    let git_dir_raw = String::from_utf8_lossy(&git_dir_out.stdout)
        .trim()
        .to_string();
    if git_dir_raw.is_empty() {
        return;
    }
    let git_dir = {
        let p = PathBuf::from(&git_dir_raw);
        if p.is_absolute() {
            p
        } else {
            cwd.join(p)
        }
    };
    let head_path = git_dir.join("HEAD");
    if head_path.exists() {
        println!("cargo:rerun-if-changed={}", head_path.display());
    }
    let packed_refs_path = git_dir.join("packed-refs");
    if packed_refs_path.exists() {
        println!("cargo:rerun-if-changed={}", packed_refs_path.display());
    }
}
