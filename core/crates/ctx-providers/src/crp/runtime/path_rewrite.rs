use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

enum BundledLinuxRewrite {
    NotBundledPath,
    AlreadyLinux,
    Candidate(String),
}

fn bundled_linux_candidate_for_marker(path: &str, marker: &str) -> BundledLinuxRewrite {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return BundledLinuxRewrite::NotBundledPath;
    }
    let sep = if trimmed.contains('\\') { '\\' } else { '/' };
    let needle = format!("{sep}{marker}{sep}");
    let Some(idx) = trimmed.find(&needle) else {
        return BundledLinuxRewrite::NotBundledPath;
    };
    let prefix = &trimmed[..idx];
    let bundles_segment = format!("{sep}bundles");
    let prefix_path = Path::new(prefix);
    let looks_like_bundle_root = prefix == "bundles"
        || prefix.ends_with(&bundles_segment)
        || prefix_path.join("manifest.json").is_file()
        || prefix_path.join("runtime_lock.v2.json").is_file();
    if !looks_like_bundle_root {
        return BundledLinuxRewrite::NotBundledPath;
    }
    let rest = &trimmed[idx + needle.len()..];
    let mut parts = rest.split(sep);
    let Some(id) = parts.next() else {
        return BundledLinuxRewrite::NotBundledPath;
    };
    let Some(os) = parts.next() else {
        return BundledLinuxRewrite::NotBundledPath;
    };
    let Some(arch) = parts.next() else {
        return BundledLinuxRewrite::NotBundledPath;
    };
    if os == "linux" {
        return BundledLinuxRewrite::AlreadyLinux;
    }
    let tail: String = parts.collect::<Vec<_>>().join(&sep.to_string());
    let candidate = format!("{prefix}{needle}{id}{sep}linux{sep}{arch}{sep}{tail}");
    BundledLinuxRewrite::Candidate(candidate)
}

fn bundled_linux_candidate(path: &str) -> BundledLinuxRewrite {
    let providers = bundled_linux_candidate_for_marker(path, "providers");
    if !matches!(providers, BundledLinuxRewrite::NotBundledPath) {
        return providers;
    }
    bundled_linux_candidate_for_marker(path, "runtimes")
}

#[derive(Debug, Deserialize)]
struct BundledManifestRuntimeEntry {
    id: String,
    os: String,
    arch: String,
    root: String,
    bin: String,
}

#[derive(Debug, Deserialize)]
struct BundledManifestForRuntimeRewrite {
    #[serde(default)]
    runtimes: Vec<BundledManifestRuntimeEntry>,
}

fn bundles_root_for_path(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        let looks_like_bundle_root = ancestor
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new("bundles"))
            || ancestor.join("manifest.json").is_file()
            || ancestor.join("runtime_lock.v2.json").is_file();
        if looks_like_bundle_root {
            Some(ancestor.to_path_buf())
        } else {
            None
        }
    })
}

fn resolve_runtime_linux_path_from_manifest(path: &str) -> Option<String> {
    let source_path = Path::new(path);
    let bundles_root = bundles_root_for_path(source_path)?;
    let rel = source_path.strip_prefix(&bundles_root).ok()?;
    let mut parts = rel.components();
    if parts.next()?.as_os_str() != std::ffi::OsStr::new("runtimes") {
        return None;
    }
    let runtime_id = parts.next()?.as_os_str().to_string_lossy().to_string();
    let _host_os = parts.next()?;
    let arch = parts.next()?.as_os_str().to_string_lossy().to_string();
    let source_bin_name = source_path.file_name()?.to_string_lossy().to_string();

    let manifest_path = bundles_root.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path).ok()?;
    let manifest: BundledManifestForRuntimeRewrite = serde_json::from_str(&raw).ok()?;
    let runtime = manifest
        .runtimes
        .iter()
        .find(|entry| entry.id == runtime_id && entry.os == "linux" && entry.arch == arch)?;
    let root = Path::new(&runtime.root);
    let runtime_root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        bundles_root.join(root)
    };
    let candidate = runtime_root.join(&runtime.bin);
    if !candidate.exists() {
        return None;
    }
    if candidate
        .file_name()
        .is_some_and(|name| name.to_string_lossy() == source_bin_name)
    {
        return Some(candidate.to_string_lossy().to_string());
    }
    None
}

pub(crate) fn rewrite_bundled_path_for_linux(path: &str) -> Result<String> {
    match bundled_linux_candidate(path) {
        BundledLinuxRewrite::NotBundledPath | BundledLinuxRewrite::AlreadyLinux => {
            Ok(path.to_string())
        }
        BundledLinuxRewrite::Candidate(candidate) => {
            if std::path::Path::new(&candidate).exists() {
                Ok(candidate)
            } else if let Some(runtime_candidate) = resolve_runtime_linux_path_from_manifest(path) {
                Ok(runtime_candidate)
            } else {
                anyhow::bail!(
                    "missing linux bundled path for container execution: source='{path}' expected='{candidate}'"
                );
            }
        }
    }
}

fn rewrite_bundled_paths_in_shell_command(
    raw: &str,
    env: &HashMap<String, String>,
) -> Result<String> {
    let tokens = shlex::split(raw).ok_or_else(|| {
        anyhow::anyhow!("invalid shell command in --acp-command: unmatched quote")
    })?;
    if tokens.is_empty() {
        return Ok(raw.to_string());
    }

    let mut rewritten = Vec::with_capacity(tokens.len());
    for token in tokens {
        rewritten.push(rewrite_bundled_path_for_linux(&token)?);
    }

    let first_is_js_entrypoint = rewritten
        .first()
        .and_then(|command| std::path::Path::new(command).extension())
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("js"));
    if first_is_js_entrypoint {
        let node = resolve_node_binary_from_env(env)
            .ok_or_else(|| anyhow::anyhow!("could not resolve node binary for JS ACP command"))?;
        let rewritten_node = rewrite_bundled_path_for_linux(&node)?;
        rewritten.insert(0, rewritten_node);
    }

    shlex::try_join(rewritten.iter().map(String::as_str))
        .map_err(|err| anyhow::anyhow!("failed to quote --acp-command after rewrite: {err}"))
}

pub(super) fn rewrite_container_args_for_linux(
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--acp-command" {
            out.push(arg.clone());
            if let Some(acp_command) = args.get(idx + 1) {
                out.push(rewrite_bundled_paths_in_shell_command(acp_command, env)?);
                idx += 2;
                continue;
            }
            idx += 1;
            continue;
        }
        out.push(rewrite_bundled_path_for_linux(arg)?);
        idx += 1;
    }
    Ok(out)
}

fn resolve_node_binary_from_env(env: &HashMap<String, String>) -> Option<String> {
    let path_value = env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())?;
    let executable_names: &[&str] = if cfg!(windows) {
        &["node.exe", "node"]
    } else {
        &["node"]
    };
    for dir in std::env::split_paths(std::ffi::OsStr::new(&path_value)) {
        for name in executable_names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

pub(crate) fn rewrite_container_command_for_linux(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<(String, Vec<String>)> {
    let rewritten_command = rewrite_bundled_path_for_linux(command)?;
    let rewritten_args = rewrite_container_args_for_linux(args, env)?;
    let is_js_entrypoint = std::path::Path::new(&rewritten_command)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("js"));
    if !is_js_entrypoint {
        return Ok((rewritten_command, rewritten_args));
    }
    let Some(node_binary) = resolve_node_binary_from_env(env) else {
        return Ok((rewritten_command, rewritten_args));
    };
    let rewritten_node = rewrite_bundled_path_for_linux(&node_binary)?;
    let mut final_args = Vec::with_capacity(rewritten_args.len() + 1);
    final_args.push(rewritten_command);
    final_args.extend(rewritten_args);
    Ok((rewritten_node, final_args))
}

pub(crate) fn resolve_explicit_command_path(command: &str) -> Option<PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = Path::new(trimmed);
    if p.is_absolute() || trimmed.contains('/') || trimmed.contains('\\') {
        return if p.exists() {
            Some(p.to_path_buf())
        } else {
            None
        };
    }
    None
}
