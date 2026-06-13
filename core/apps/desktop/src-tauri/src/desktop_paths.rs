use super::*;

const DEBUG_DESKTOP_LOCAL_ROOT_DIR: &str = ".ctx-dev";

fn sanitized_path_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

fn compute_desktop_local_data_root(
    override_raw: Option<&str>,
    debug_build: bool,
    dev_instance_id: &str,
    shared_root: &Path,
) -> Result<PathBuf> {
    if let Some(raw) = override_raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if !path.is_absolute() {
                anyhow::bail!("{DESKTOP_DAEMON_DATA_DIR_ENV} must be an absolute path");
            }
            return Ok(path);
        }
    }

    if debug_build {
        let base = shared_root
            .parent()
            .context("resolving ctx home parent directory")?;
        let instance = sanitized_path_component(dev_instance_id);
        return Ok(base.join(DEBUG_DESKTOP_LOCAL_ROOT_DIR).join(instance));
    }

    Ok(shared_root.to_path_buf())
}

pub(super) fn desktop_local_data_root() -> Result<PathBuf> {
    let shared_root = ctx_fs::paths::default_ctx_home().context("resolving ctx home")?;
    compute_desktop_local_data_root(
        std::env::var(DESKTOP_DAEMON_DATA_DIR_ENV).ok().as_deref(),
        cfg!(debug_assertions),
        desktop_dev_instance_id(),
        &shared_root,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_override_wins() {
        let shared_root = Path::new("/tmp/release-root");
        let root = compute_desktop_local_data_root(
            Some(" /tmp/custom-root "),
            true,
            "dev-abcd",
            shared_root,
        )
        .expect("absolute override should resolve");
        assert_eq!(root, PathBuf::from("/tmp/custom-root"));
    }

    #[test]
    fn relative_override_is_rejected() {
        let shared_root = Path::new("/tmp/release-root");
        let err =
            compute_desktop_local_data_root(Some("relative/path"), true, "dev-abcd", shared_root)
                .expect_err("relative override must fail");
        assert!(format!("{err:#}").contains(DESKTOP_DAEMON_DATA_DIR_ENV));
    }

    #[test]
    fn debug_build_uses_instance_scoped_local_data_root() {
        let shared_root = Path::new("/tmp/release-root");
        let root = compute_desktop_local_data_root(None, true, "dev/demo:worktree", shared_root)
            .expect("debug root should resolve");
        assert_eq!(root, PathBuf::from("/tmp/.ctx-dev/dev_demo_worktree"));
    }

    #[test]
    fn release_build_uses_shared_ctx_root() {
        let shared_root = Path::new("/tmp/release-root");
        let root = compute_desktop_local_data_root(None, false, "dev-abcd", shared_root)
            .expect("release root should resolve");
        assert_eq!(root, PathBuf::from("/tmp/release-root"));
    }
}
