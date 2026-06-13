use super::*;

fn current_arch_token() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        std::env::consts::ARCH
    }
}

fn resource_bin(app: &tauri::AppHandle, name: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(res) = app.path().resource_dir().ok() {
        let bin_ext = if cfg!(target_os = "windows") {
            ".exe"
        } else {
            ""
        };
        let arch = current_arch_token();
        let prefix = format!("{name}-{arch}");
        for base in [res.join("bin"), res.clone()] {
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };
            let mut paths: Vec<PathBuf> =
                entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            paths.sort();
            for p in paths {
                if !p.is_file() {
                    continue;
                }
                let Some(file_name) = p.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !file_name.starts_with(&prefix) {
                    continue;
                }
                if !bin_ext.is_empty() && !file_name.ends_with(bin_ext) {
                    continue;
                }
                candidates.push(p);
            }
        }

        // Fall back to generic names only after we try arch-specific candidates.
        candidates.push(res.join("bin").join(format!("{name}{bin_ext}")));
        candidates.push(res.join(format!("{name}{bin_ext}")));
    }

    for c in candidates {
        if c.exists() && path_matches_current_platform_binary(&c) {
            return Some(c);
        }
    }
    None
}

fn dev_bin(name: &str) -> Option<PathBuf> {
    let bin_ext = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    if let Ok(raw) = std::env::var("CTX_DESKTOP_DEV_BIN_DIR") {
        let raw = raw.trim();
        if !raw.is_empty() {
            let candidate = PathBuf::from(raw).join(format!("{name}{bin_ext}"));
            if candidate.exists() && path_matches_current_platform_binary(&candidate) {
                return Some(candidate);
            }
        }
    }
    if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
        let candidate = PathBuf::from(target_dir)
            .join("debug")
            .join(format!("{name}{bin_ext}"));
        if candidate.exists() && path_matches_current_platform_binary(&candidate) {
            return Some(candidate);
        }
    }
    // Desktop prep syncs host binaries into src-tauri/bin; prefer this before generic core/target.
    let synced_bin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join(format!("{name}{bin_ext}"));
    if synced_bin.exists() && path_matches_current_platform_binary(&synced_bin) {
        return Some(synced_bin);
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())?
        .to_path_buf();
    let candidate = root
        .join("target")
        .join("debug")
        .join(format!("{name}{bin_ext}"));
    if candidate.exists() && path_matches_current_platform_binary(&candidate) {
        return Some(candidate);
    }
    None
}

pub(super) fn resolve_daemon_bin(app: &tauri::AppHandle) -> Result<PathBuf> {
    if cfg!(debug_assertions) {
        return dev_bin(DESKTOP_DAEMON_BIN_NAME).with_context(|| format!(
            "missing development binary for `{DESKTOP_DAEMON_BIN_NAME}` at expected path (run `pnpm -C core desktop:prep` or rerun desktop_sync_resources after building `ctx-http`)"
        ));
    }
    resource_bin(app, DESKTOP_DAEMON_BIN_NAME).with_context(|| {
        format!("missing bundled binary for `{DESKTOP_DAEMON_BIN_NAME}` in application resources")
    })
}

pub(super) fn select_optional_bin_path(
    name: &str,
    bundled: Option<PathBuf>,
    dev: Option<PathBuf>,
    debug_build: bool,
    is_macos: bool,
) -> Option<PathBuf> {
    if !debug_build {
        return bundled;
    }
    if is_macos && name == "ctx-avf-linux-helper" {
        return bundled.or(dev);
    }
    dev.or(bundled)
}

pub(super) fn resolve_optional_bin(app: &tauri::AppHandle, name: &str) -> Option<PathBuf> {
    let bundled = resource_bin(app, name);
    let dev = if cfg!(debug_assertions) {
        dev_bin(name)
    } else {
        None
    };
    select_optional_bin_path(
        name,
        bundled,
        dev,
        cfg!(debug_assertions),
        cfg!(target_os = "macos"),
    )
}

fn path_matches_current_platform_binary(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 4];
    if file.read_exact(&mut header).is_err() {
        return false;
    }

    if cfg!(target_os = "linux") {
        header == [0x7f, b'E', b'L', b'F']
    } else if cfg!(target_os = "windows") {
        header[0..2] == [b'M', b'Z']
    } else if cfg!(target_os = "macos") {
        matches!(
            header,
            [0xFE, 0xED, 0xFA, 0xCE]
                | [0xCE, 0xFA, 0xED, 0xFE]
                | [0xFE, 0xED, 0xFA, 0xCF]
                | [0xCF, 0xFA, 0xED, 0xFE]
                | [0xCA, 0xFE, 0xBA, 0xBE]
                | [0xBE, 0xBA, 0xFE, 0xCA]
        )
    } else {
        true
    }
}

pub(super) fn dev_web_dist() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())?
        .join("web")
        .join("dist");
    if dir.exists() {
        Some(dir)
    } else {
        None
    }
}

pub(super) fn dev_bundle_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundles");
    if dir.exists() {
        Some(dir)
    } else {
        None
    }
}

pub(super) fn configured_bundle_dir() -> Option<PathBuf> {
    let raw = std::env::var(DESKTOP_BUNDLE_DIR_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

pub(super) fn select_bundle_dir_path(
    configured: Option<PathBuf>,
    bundled: Option<PathBuf>,
    dev: Option<PathBuf>,
) -> Option<PathBuf> {
    configured.or(bundled).or(dev)
}

pub(crate) fn desktop_bundle_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|p| p.join("bundles"))
        .filter(|p| p.exists());
    select_bundle_dir_path(configured_bundle_dir(), bundled, dev_bundle_dir())
}

pub(in super::super) fn daemon_data_dir(_app: &tauri::AppHandle) -> Result<PathBuf> {
    // Debug desktop builds must not share the release app's local daemon state by default.
    let root = desktop_local_data_root()?;
    std::fs::create_dir_all(&root)
        .with_context(|| format!("creating ctx home {}", root.display()))?;
    Ok(root)
}
