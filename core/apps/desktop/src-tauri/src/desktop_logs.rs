use super::*;

fn desktop_logs_dir() -> Result<PathBuf> {
    Ok(desktop_local_data_root()?.join("logs"))
}

fn desktop_log_path() -> Result<PathBuf> {
    Ok(desktop_logs_dir()?.join("desktop.log"))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn append_native_desktop_log_line(message: &str, level: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let path = desktop_log_path()?;
    append_native_desktop_log_line_at_path(&path, message, level)
}

fn append_native_desktop_log_line_at_path(path: &Path, message: &str, level: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("desktop log path missing parent: {}", path.display()))?;
    ctx_fs::permissions::ensure_private_dir_sync(parent)
        .with_context(|| format!("creating desktop log dir at {}", parent.display()))?;
    let mut file = ctx_fs::permissions::open_private_append_sync(path)
        .with_context(|| format!("opening desktop log at {}", path.display()))?;
    writeln!(file, "{} [{}] {}", now_ms(), level, message)
        .with_context(|| format!("writing desktop log at {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flushing desktop log at {}", path.display()))?;
    Ok(())
}

fn log_native_desktop_line(level: &str, message: &str) {
    if let Err(err) = append_native_desktop_log_line(message, level) {
        eprintln!("desktop_startup: log_write_failed level={level} error={err:#}");
    }
    eprintln!("{message}");
}

pub(super) fn log_desktop_startup(message: &str) {
    log_native_desktop_line("info", message);
}

pub(super) fn log_desktop_startup_error(message: &str) {
    log_native_desktop_line("error", message);
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::append_native_desktop_log_line_at_path;

    #[test]
    fn desktop_log_append_creates_private_file() {
        let dir = std::env::temp_dir().join(format!("ctx-desktop-log-{}", uuid::Uuid::new_v4()));
        let path = dir.join("logs").join("desktop.log");

        append_native_desktop_log_line_at_path(&path, "hello", "info").unwrap();

        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn desktop_log_append_rejects_symlinked_log_file() {
        let dir = std::env::temp_dir().join(format!("ctx-desktop-log-{}", uuid::Uuid::new_v4()));
        let outside =
            std::env::temp_dir().join(format!("ctx-desktop-log-outside-{}", uuid::Uuid::new_v4()));
        let path = dir.join("logs").join("desktop.log");
        let outside_path = outside.join("outside.log");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(&outside_path, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside_path, &path).unwrap();

        let err = append_native_desktop_log_line_at_path(&path, "hello", "info").unwrap_err();

        assert!(format!("{err:#}").contains("must not be a symlink"));
        assert_eq!(std::fs::read(&outside_path).unwrap(), b"outside");
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }
}
