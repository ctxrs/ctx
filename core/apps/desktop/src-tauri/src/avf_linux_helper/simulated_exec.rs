use super::*;

#[derive(Debug)]
pub(super) struct SimulatedExecRequest {
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: PathBuf,
    pub(super) env: HashMap<String, String>,
}

pub(super) fn resolve_simulated_exec_request(
    data_root: &Path,
    request: &AvfLinuxExecRequest,
) -> Result<SimulatedExecRequest> {
    let cwd = resolve_host_path_for_guest_path(data_root, Path::new(&request.cwd))?;
    let command = rewrite_bundled_path_for_host_simulation(&request.command);
    let args = request
        .args
        .iter()
        .map(|arg| rewrite_bundled_path_for_host_simulation(arg))
        .collect::<Vec<_>>();
    let env = request
        .env
        .iter()
        .map(|(key, value)| {
            let rewritten = if key == "PATH" {
                rewrite_path_list_for_host_simulation(value)
            } else if key.ends_with("_PATH") || key.ends_with("_ROOT") || key.ends_with("_DIR") {
                rewrite_bundled_path_for_host_simulation(value)
            } else {
                value.to_string()
            };
            (key.clone(), rewritten)
        })
        .collect::<HashMap<_, _>>();
    Ok(SimulatedExecRequest {
        command,
        args,
        cwd,
        env,
    })
}

pub(super) fn resolve_host_path_for_guest_path(
    data_root: &Path,
    guest_path: &Path,
) -> Result<PathBuf> {
    if guest_path == Path::new("/") {
        return Ok(shared_vm_root(data_root));
    }
    let worktrees_root = shared_vm_worktrees_root(data_root);
    if !worktrees_root.exists() {
        bail!(
            "shared VM worktree metadata root is missing at {}",
            worktrees_root.display()
        );
    }
    for workspace_dir in fs::read_dir(&worktrees_root)
        .with_context(|| format!("reading {}", worktrees_root.display()))?
    {
        let workspace_dir = workspace_dir?.path();
        if !workspace_dir.is_dir() {
            continue;
        }
        for worktree_dir in fs::read_dir(&workspace_dir)
            .with_context(|| format!("reading {}", workspace_dir.display()))?
        {
            let worktree_dir = worktree_dir?.path();
            let metadata_path = worktree_dir.join(GUEST_WORKTREE_METADATA_FILE);
            let Some(metadata) = load_guest_worktree_state(&metadata_path)? else {
                continue;
            };
            let guest_root = metadata.guest_root;
            if guest_path == guest_root {
                return Ok(metadata.host_shadow_root);
            }
            if guest_path.starts_with(&guest_root) {
                let relative = guest_path
                    .strip_prefix(&guest_root)
                    .with_context(|| format!("mapping {}", guest_path.display()))?;
                return Ok(join_relative_path(&metadata.host_shadow_root, relative));
            }
        }
    }
    bail!(
        "no helper-staged worktree metadata found for guest path {}",
        guest_path.display()
    );
}

pub(super) fn join_relative_path(root: &Path, relative: &Path) -> PathBuf {
    let mut out = root.to_path_buf();
    if relative != Path::new("") {
        out.push(relative);
    }
    out
}

pub(super) fn rewrite_path_list_for_host_simulation(value: &str) -> String {
    if value.trim().is_empty() {
        return value.to_string();
    }
    let rewritten = std::env::split_paths(std::ffi::OsStr::new(value))
        .map(|entry| {
            PathBuf::from(rewrite_bundled_path_for_host_simulation(
                entry.to_string_lossy().as_ref(),
            ))
        })
        .collect::<Vec<_>>();
    std::env::join_paths(rewritten)
        .map(|joined| joined.to_string_lossy().to_string())
        .unwrap_or_else(|_| value.to_string())
}

pub(super) fn rewrite_bundled_path_for_host_simulation(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return value.to_string();
    }
    let sep = if trimmed.contains('\\') { '\\' } else { '/' };
    for marker in ["providers", "runtimes"] {
        let needle = format!("{sep}{marker}{sep}");
        let Some(idx) = trimmed.find(&needle) else {
            continue;
        };
        let rest = &trimmed[idx + needle.len()..];
        let mut parts = rest.split(sep);
        let Some(id) = parts.next() else {
            continue;
        };
        let Some(os) = parts.next() else {
            continue;
        };
        let Some(_arch) = parts.next() else {
            continue;
        };
        if os != "linux" {
            return value.to_string();
        }
        let tail = parts.collect::<Vec<_>>().join(&sep.to_string());
        let prefix = &trimmed[..idx];
        let candidate = format!(
            "{prefix}{needle}{id}{sep}macos{sep}{}{sep}{tail}",
            std::env::consts::ARCH
        );
        if Path::new(&candidate).exists() {
            return candidate;
        }
    }
    value.to_string()
}
