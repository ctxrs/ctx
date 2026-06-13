use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) const DAEMON_PATH_SENTINEL_BEGIN: &str = "__CTX_DAEMON_PATH_BEGIN__";
pub(super) const DAEMON_PATH_SENTINEL_END: &str = "__CTX_DAEMON_PATH_END__";
pub(super) const LOCAL_DAEMON_PATH_PROBE_START: &str = "__CTX_DAEMON_PATH_START__";
pub(super) const LOCAL_DAEMON_PATH_PROBE_END: &str = "__CTX_DAEMON_PATH_END__";

fn append_unique_path_entry(
    entries: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    entry: PathBuf,
) {
    if entry.as_os_str().is_empty() {
        return;
    }
    if seen.insert(entry.clone()) {
        entries.push(entry);
    }
}

fn append_path_entries_from_raw(
    entries: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    raw: &OsStr,
) {
    for entry in std::env::split_paths(raw) {
        append_unique_path_entry(entries, seen, entry);
    }
}

fn append_common_tool_dirs(
    entries: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    home_dir: Option<&Path>,
) {
    if let Some(home) = home_dir {
        append_unique_path_entry(entries, seen, home.join(".local").join("bin"));
        append_unique_path_entry(entries, seen, home.join("bin"));
    }
    for raw in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        append_unique_path_entry(entries, seen, PathBuf::from(raw));
    }
}

pub(super) fn build_effective_daemon_path(
    current_path: Option<&OsStr>,
    shell_path: Option<&OsStr>,
    home_dir: Option<&Path>,
) -> Option<OsString> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    if let Some(raw) = current_path {
        append_path_entries_from_raw(&mut entries, &mut seen, raw);
    }
    if let Some(raw) = shell_path {
        append_path_entries_from_raw(&mut entries, &mut seen, raw);
    }
    append_common_tool_dirs(&mut entries, &mut seen, home_dir);
    if entries.is_empty() {
        return None;
    }
    std::env::join_paths(entries).ok()
}

pub(super) fn extract_shell_path(stdout: &[u8]) -> Option<OsString> {
    let output = String::from_utf8_lossy(stdout);
    let start = output.find(DAEMON_PATH_SENTINEL_BEGIN)?;
    let tail = &output[start + DAEMON_PATH_SENTINEL_BEGIN.len()..];
    let end = tail.find(DAEMON_PATH_SENTINEL_END)?;
    let path = tail[..end].trim();
    (!path.is_empty()).then_some(OsString::from(path))
}

pub(super) fn read_login_shell_path(shell_path: &Path) -> Option<OsString> {
    if !shell_path.is_absolute() || !shell_path.exists() {
        return None;
    }
    let output = Command::new(shell_path)
        .arg("-lc")
        .arg(format!(
            "printf '{DAEMON_PATH_SENTINEL_BEGIN}%s{DAEMON_PATH_SENTINEL_END}' \"$PATH\""
        ))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    extract_shell_path(&output.stdout)
}

fn candidate_login_shell_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(shell) = std::env::var_os("SHELL").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(shell);
        if path.is_absolute() {
            candidates.push(path);
        }
    }
    for raw in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        let path = PathBuf::from(raw);
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    candidates
}

pub(super) fn resolve_daemon_path_env() -> Option<OsString> {
    let shell_path = candidate_login_shell_paths()
        .into_iter()
        .find_map(|shell| read_login_shell_path(&shell));
    let home_dir = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    build_effective_daemon_path(
        std::env::var_os("PATH").as_deref(),
        shell_path.as_deref(),
        home_dir.as_deref(),
    )
}

fn trim_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn local_daemon_login_shell_path() -> Option<PathBuf> {
    let shell_from_env = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.exists());
    shell_from_env.or_else(|| {
        if cfg!(target_os = "macos") {
            let fallback = PathBuf::from("/bin/zsh");
            if fallback.exists() {
                return Some(fallback);
            }
        }
        None
    })
}

pub(super) fn parse_local_daemon_path_probe_output(stdout: &str) -> Option<String> {
    let start = stdout.find(LOCAL_DAEMON_PATH_PROBE_START)?;
    let after_start = start + LOCAL_DAEMON_PATH_PROBE_START.len();
    let end_rel = stdout[after_start..].find(LOCAL_DAEMON_PATH_PROBE_END)?;
    trim_non_empty(&stdout[after_start..after_start + end_rel])
}

pub(super) fn probe_local_daemon_path_via_shell(shell_path: &Path) -> Option<String> {
    let probe_command = format!(
        "printf '%s' '{start}'; printenv PATH; printf '%s' '{end}'",
        start = LOCAL_DAEMON_PATH_PROBE_START,
        end = LOCAL_DAEMON_PATH_PROBE_END,
    );
    let output = Command::new(shell_path)
        .arg("-l")
        .arg("-c")
        .arg(&probe_command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_local_daemon_path_probe_output(&String::from_utf8_lossy(&output.stdout))
}

pub(super) fn resolve_local_daemon_path_env() -> Option<String> {
    // Finder-launched macOS apps often miss user shell PATH entries like ~/.local/bin. Probe the
    // login shell once at desktop daemon spawn so downstream CLI discovery resolves the same tools
    // a user expects in Terminal, then pass that PATH explicitly to the daemon process.
    if cfg!(target_os = "macos") {
        if let Some(shell_path) = local_daemon_login_shell_path() {
            if let Some(shell_path_env) = probe_local_daemon_path_via_shell(&shell_path) {
                return Some(shell_path_env);
            }
        }
    }
    std::env::var("PATH")
        .ok()
        .and_then(|value| trim_non_empty(&value))
}
