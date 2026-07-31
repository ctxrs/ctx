use super::*;

use crate::semantic::paths_status::{pid_lock_uses_advisory_protocol, process_executable_path};

pub(super) fn verify_legacy_v025_identity(
    data_root: &Path,
    expected_executable: &Path,
    recorded_binary: &Path,
    pid: u32,
    value: &Value,
) -> Result<()> {
    if value.get("binary_sha256").is_some() {
        verify_recorded_digest_identity(pid, value)?;
    }
    if !pid_lock_uses_advisory_protocol(value) {
        return Err(anyhow!(
            "legacy ctx daemon lock does not use the v0.25 advisory protocol"
        ));
    }
    if value
        .get("owner_id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(anyhow!("legacy ctx daemon lock has no owner identity"));
    }
    if value.get("released").and_then(Value::as_bool) != Some(false) {
        return Err(anyhow!("legacy ctx daemon lock is not live owner metadata"));
    }
    if value
        .get("started_at_ms")
        .and_then(Value::as_i64)
        .is_none_or(|started_at_ms| started_at_ms <= 0)
    {
        return Err(anyhow!(
            "legacy ctx daemon lock has no process-start identity"
        ));
    }

    verify_legacy_lock_file_ownership(data_root)?;
    let process_executable = process_executable_path(pid).ok_or_else(|| {
        anyhow!("cannot resolve executable path for legacy ctx daemon process {pid}")
    })?;
    verify_same_unix_file(
        expected_executable,
        &process_executable,
        "legacy lock owner executable inode does not match the installed ctx executable",
    )?;
    verify_same_unix_file(
        recorded_binary,
        &process_executable,
        "legacy lock executable inode does not match its owner process",
    )?;
    let expected_sha256 = executable_sha256(expected_executable)?;
    let process_sha256 = process_executable_sha256(pid).ok_or_else(|| {
        anyhow!("cannot verify executable image for legacy ctx daemon process {pid}")
    })?;
    if process_sha256 != expected_sha256 {
        return Err(anyhow!(
            "legacy lock owner image does not match the installed ctx executable"
        ));
    }
    verify_legacy_daemon_command(pid, data_root, expected_executable)?;
    #[cfg(target_os = "linux")]
    verify_linux_advisory_lock_owner(pid, &pid_lock_guard_path(&daemon_lock_path(data_root)))?;
    Ok(())
}

fn verify_same_unix_file(left: &Path, right: &Path, mismatch: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let left_metadata = fs::metadata(left)
        .with_context(|| format!("read executable identity {}", left.display()))?;
    let right_metadata = fs::metadata(right)
        .with_context(|| format!("read process executable identity {}", right.display()))?;
    if left_metadata.dev() != right_metadata.dev()
        || left_metadata.ino() != right_metadata.ino()
        || !left_metadata.is_file()
        || !right_metadata.is_file()
    {
        return Err(anyhow!("{mismatch}"));
    }
    Ok(())
}

fn verify_legacy_lock_file_ownership(data_root: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let effective_uid = unsafe { libc::geteuid() };
    for path in [
        data_root.to_path_buf(),
        daemon_lock_path(data_root),
        pid_lock_guard_path(&daemon_lock_path(data_root)),
    ] {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("read legacy daemon ownership {}", path.display()))?;
        if metadata.uid() != effective_uid {
            return Err(anyhow!(
                "legacy daemon identity path is not owned by the upgrading user: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn verify_legacy_daemon_command(
    pid: u32,
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    use std::ffi::OsStr;

    let arguments = process_arguments(pid)?;
    let argv0 = arguments
        .first()
        .ok_or_else(|| anyhow!("legacy ctx daemon process has no argv[0] identity"))?;
    verify_same_unix_file(
        Path::new(argv0),
        expected_executable,
        "legacy daemon argv[0] is not the installed ctx executable",
    )?;

    let mut recorded_root = None;
    let mut daemon_command = false;
    let mut index = 1;
    while index < arguments.len() {
        if arguments[index] == OsStr::new("--data-root") {
            let root = arguments
                .get(index + 1)
                .ok_or_else(|| anyhow!("legacy ctx daemon has an incomplete data-root argument"))?;
            if recorded_root.replace(PathBuf::from(root)).is_some() {
                return Err(anyhow!(
                    "legacy ctx daemon has ambiguous data-root arguments"
                ));
            }
            index += 2;
            continue;
        }
        if let Some(root) = arguments[index]
            .to_str()
            .and_then(|argument| argument.strip_prefix("--data-root="))
        {
            if root.is_empty() || recorded_root.replace(PathBuf::from(root)).is_some() {
                return Err(anyhow!(
                    "legacy ctx daemon has ambiguous data-root arguments"
                ));
            }
            index += 1;
            continue;
        }
        if arguments[index] == OsStr::new("daemon") {
            daemon_command = arguments.get(index + 1).is_some_and(|arg| arg == "run");
            break;
        }
        index += 1;
    }
    if !daemon_command {
        return Err(anyhow!(
            "legacy lock owner process is not a ctx daemon run command"
        ));
    }
    let recorded_root = recorded_root
        .ok_or_else(|| anyhow!("legacy ctx daemon process has no data-root argument identity"))?;
    if fs::canonicalize(&recorded_root).ok() != fs::canonicalize(data_root).ok() {
        return Err(anyhow!(
            "legacy ctx daemon process data root does not match its held lock"
        ));
    }
    Ok(())
}

fn process_arguments(pid: u32) -> Result<Vec<std::ffi::OsString>> {
    let bytes = fs::read(format!("/proc/{pid}/cmdline"))
        .with_context(|| format!("read legacy ctx daemon process arguments for {pid}"))?;
    nul_separated_arguments(&bytes)
}

fn nul_separated_arguments(bytes: &[u8]) -> Result<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;

    let arguments = bytes
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| std::ffi::OsString::from_vec(argument.to_vec()))
        .collect::<Vec<_>>();
    if arguments.is_empty() {
        return Err(anyhow!(
            "legacy ctx daemon process has no argument identity"
        ));
    }
    Ok(arguments)
}

fn verify_linux_advisory_lock_owner(pid: u32, guard_path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::metadata(guard_path)
        .with_context(|| format!("read legacy daemon guard identity {}", guard_path.display()))?;
    let device_major = libc::major(metadata.dev());
    let device_minor = libc::minor(metadata.dev());
    let locks = fs::read_to_string("/proc/locks")
        .context("read Linux advisory-lock ownership for legacy ctx daemon")?;
    let owner_matches = locks.lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 6
            || fields[1] != "FLOCK"
            || fields[3] != "WRITE"
            || fields[4].parse::<u32>().ok() != Some(pid)
        {
            return false;
        }
        let identity = fields[5].split(':').collect::<Vec<_>>();
        identity.len() == 3
            && u64::from_str_radix(identity[0], 16).ok() == Some(u64::from(device_major))
            && u64::from_str_radix(identity[1], 16).ok() == Some(u64::from(device_minor))
            && identity[2].parse::<u64>().ok() == Some(metadata.ino())
    });
    if !owner_matches {
        return Err(anyhow!(
            "legacy ctx daemon PID does not own its advisory guard lock"
        ));
    }
    Ok(())
}
