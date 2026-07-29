use std::{
    env,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};
#[cfg(not(unix))]
use std::sync::mpsc;

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::CaptureError;
use uuid::Uuid;

use super::HistorySourcePluginSource;

const MAX_PLUGIN_STDOUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PLUGIN_STDERR_BYTES: usize = 256 * 1024;
const MAX_PLUGIN_STDERR_SNIPPET_BYTES: usize = 4096;
const MAX_INLINE_CURSOR_ENV_BYTES: usize = 8192;
const SAFE_PLUGIN_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TEMP",
    "TMP",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];

#[derive(Debug, Clone)]
pub(crate) struct HistorySourcePluginRun {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: String,
}

#[derive(Debug, Clone)]
pub(crate) struct HistorySourcePluginRunOptions<'a> {
    pub(crate) data_root: &'a Path,
    pub(crate) machine_id: &'a str,
    pub(crate) cursor: Option<&'a str>,
    pub(crate) cursor_stream: &'a str,
    pub(crate) full_rescan: bool,
}

pub(crate) fn run_history_source_plugin(
    source: &HistorySourcePluginSource,
    options: HistorySourcePluginRunOptions<'_>,
) -> Result<HistorySourcePluginRun> {
    let (program, args) = source.command.split_first().ok_or_else(|| {
        anyhow!(
            "history source plugin {} has an empty command",
            source.label()
        )
    })?;
    let mut command = Command::new(program);
    command.env_clear();
    inherit_safe_plugin_env(&mut command);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if let Some(working_dir) = &source.working_dir {
        command.current_dir(resolve_manifest_path(&source.manifest_dir, working_dir));
    }
    for (key, value) in &source.env {
        command.env(key, value);
    }
    command.env("CTX_DATA_ROOT", options.data_root);
    command.env("CTX_HISTORY_PLUGIN", "1");
    command.env("CTX_HISTORY_PLUGIN_NAME", &source.plugin_name);
    command.env("CTX_HISTORY_PLUGIN_MANIFEST", &source.manifest_path);
    command.env("CTX_HISTORY_SOURCE", source.label());
    command.env("CTX_HISTORY_SOURCE_ID", &source.source_id);
    command.env("CTX_HISTORY_PROVIDER_KEY", &source.provider_key);
    command.env("CTX_HISTORY_SOURCE_FORMAT", &source.source_format);
    command.env("CTX_HISTORY_CURSOR_STREAM", options.cursor_stream);
    command.env("CTX_HISTORY_MACHINE_ID", options.machine_id);
    command.env(
        "CTX_HISTORY_FULL_RESCAN",
        if options.full_rescan { "1" } else { "0" },
    );
    let cursor_file = if let Some(cursor) = options.cursor {
        let path = write_private_temp_file("ctx-history-cursor", cursor).with_context(|| {
            format!("write history source plugin {} cursor file", source.label())
        })?;
        if cursor.len() <= MAX_INLINE_CURSOR_ENV_BYTES {
            command.env("CTX_HISTORY_CURSOR", cursor);
        } else {
            command.env_remove("CTX_HISTORY_CURSOR");
        }
        command.env("CTX_HISTORY_CURSOR_FILE", &path);
        Some(path)
    } else {
        command.env_remove("CTX_HISTORY_CURSOR");
        command.env_remove("CTX_HISTORY_CURSOR_FILE");
        None
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            cleanup_cursor_file(cursor_file.as_ref())?;
            return Err(error).with_context(|| {
                format!(
                    "spawn history source plugin {} command {}",
                    source.label(),
                    shell_like_command(&source.command)
                )
            });
        }
    };
    let stdout = child.stdout.take().ok_or_else(|| {
        anyhow::Error::new(CaptureError::SystemInvariant(
            "history source plugin stdout was not piped",
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        anyhow::Error::new(CaptureError::SystemInvariant(
            "history source plugin stderr was not piped",
        ))
    })?;
    let run_result = collect_child_output_with_timeout(
        &mut child,
        stdout,
        stderr,
        source.timeout,
        &source.label(),
    );
    cleanup_cursor_file(cursor_file.as_ref())?;
    let (status, stdout, stderr) = run_result?;
    let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
    if !status.success() {
        let detail = if stderr.is_empty() {
            format!("exit status {status}")
        } else {
            format!("exit status {status}: {}", stderr_snippet(&stderr))
        };
        return Err(anyhow!(
            "history source plugin {} failed: {detail}",
            source.label()
        ));
    }
    Ok(HistorySourcePluginRun { stdout, stderr })
}

#[cfg(unix)]
fn collect_child_output_with_timeout(
    child: &mut Child,
    mut stdout: ChildStdout,
    mut stderr: ChildStderr,
    timeout: Duration,
    source_label: &str,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    set_nonblocking(stdout.as_raw_fd())?;
    set_nonblocking(stderr.as_raw_fd())?;

    let started = Instant::now();
    let mut status = None;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    loop {
        if stdout_open {
            read_available_with_limit(
                &mut stdout,
                &mut stdout_bytes,
                &mut stdout_open,
                MAX_PLUGIN_STDOUT_BYTES,
                "stdout",
                source_label,
            )
            .inspect_err(|_| {
                let _ = child.kill();
                let _ = child.wait();
            })?;
        }
        if stderr_open {
            read_available_with_limit(
                &mut stderr,
                &mut stderr_bytes,
                &mut stderr_open,
                MAX_PLUGIN_STDERR_BYTES,
                "stderr",
                source_label,
            )
            .inspect_err(|_| {
                let _ = child.kill();
                let _ = child.wait();
            })?;
        }
        if status.is_none() {
            status = child.try_wait().map_err(|source| CaptureError::SystemIo {
                operation: "poll history source plugin process",
                source,
            })?;
        }
        if let Some(status) = status {
            if !stdout_open && !stderr_open {
                return Ok((status, stdout_bytes, stderr_bytes));
            }
        }
        if started.elapsed() >= timeout {
            if status.is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(anyhow!(
                "history source plugin {source_label} timed out after {}s",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(unix))]
fn collect_child_output_with_timeout(
    child: &mut Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    timeout: Duration,
    source_label: &str,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    #[derive(Clone, Copy)]
    enum PipeKind {
        Stdout,
        Stderr,
    }

    let (tx, rx) = mpsc::channel();
    let stdout_source = source_label.to_owned();
    let stdout_tx = tx.clone();
    let stdout_handle = thread::spawn(move || {
        let _ = stdout_tx.send((
            PipeKind::Stdout,
            read_pipe_with_limit(stdout, MAX_PLUGIN_STDOUT_BYTES, "stdout", &stdout_source),
        ));
    });
    let stderr_source = source_label.to_owned();
    let stderr_tx = tx;
    let stderr_handle = thread::spawn(move || {
        let _ = stderr_tx.send((
            PipeKind::Stderr,
            read_pipe_with_limit(stderr, MAX_PLUGIN_STDERR_BYTES, "stderr", &stderr_source),
        ));
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|source| CaptureError::SystemIo {
            operation: "poll history source plugin process",
            source,
        })? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "history source plugin {source_label} timed out after {}s",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };

    let mut stdout = None;
    let mut stderr = None;
    while stdout.is_none() || stderr.is_none() {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(anyhow!(
                "history source plugin {source_label} timed out after {}s",
                timeout.as_secs()
            ));
        };
        if remaining == Duration::ZERO {
            return Err(anyhow!(
                "history source plugin {source_label} timed out after {}s",
                timeout.as_secs()
            ));
        }
        match rx.recv_timeout(remaining) {
            Ok((PipeKind::Stdout, result)) => stdout = Some(result?),
            Ok((PipeKind::Stderr, result)) => stderr = Some(result?),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(anyhow!(
                    "history source plugin {source_label} timed out after {}s",
                    timeout.as_secs()
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow::Error::new(CaptureError::SystemInvariant(
                    "history source plugin output reader stopped before pipes were drained",
                )));
            }
        }
    }

    if stdout_handle.join().is_err() {
        return Err(anyhow::Error::new(CaptureError::WorkerPanicked(
            "history source plugin stdout reader",
        )));
    }
    if stderr_handle.join().is_err() {
        return Err(anyhow::Error::new(CaptureError::WorkerPanicked(
            "history source plugin stderr reader",
        )));
    }
    let stdout = stdout.ok_or_else(|| {
        anyhow::Error::new(CaptureError::SystemInvariant(
            "history source plugin stdout reader returned no result",
        ))
    })?;
    let stderr = stderr.ok_or_else(|| {
        anyhow::Error::new(CaptureError::SystemInvariant(
            "history source plugin stderr reader returned no result",
        ))
    })?;
    Ok((status, stdout, stderr))
}

#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(CaptureError::SystemIo {
            operation: "read history source plugin pipe flags",
            source: std::io::Error::last_os_error(),
        }
        .into());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(CaptureError::SystemIo {
            operation: "set history source plugin pipe nonblocking",
            source: std::io::Error::last_os_error(),
        }
        .into());
    }
    Ok(())
}

#[cfg(unix)]
fn read_available_with_limit<R: Read>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
    open: &mut bool,
    max_bytes: usize,
    name: &str,
    source_label: &str,
) -> Result<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *open = false;
                return Ok(());
            }
            Ok(count) => {
                if bytes.len().saturating_add(count) > max_bytes {
                    return Err(anyhow!(
                        "history source plugin {source_label} {name} exceeded {max_bytes} byte limit"
                    ));
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(anyhow::Error::new(CaptureError::SystemIo {
                    operation: "read history source plugin output pipe",
                    source: error,
                }))
                .with_context(|| format!("read history source plugin {source_label} {name}"));
            }
        }
    }
}

#[cfg(any(test, not(unix)))]
fn read_pipe_with_limit<R: Read>(
    mut reader: R,
    max_bytes: usize,
    name: &str,
    source_label: &str,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|source| CaptureError::SystemIo {
                operation: "read history source plugin output pipe",
                source,
            })?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > max_bytes {
            return Err(anyhow!(
                "history source plugin {source_label} {name} exceeded {max_bytes} byte limit"
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn inherit_safe_plugin_env(command: &mut Command) {
    for key in SAFE_PLUGIN_ENV {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn write_private_temp_file(prefix: &str, contents: &str) -> Result<PathBuf> {
    for _ in 0..16 {
        let path = env::temp_dir().join(format!("{prefix}-{}.cursor", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())
                    .map_err(|source| CaptureError::SystemIo {
                        operation: "write history source plugin cursor file",
                        source,
                    })?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(anyhow::Error::new(CaptureError::SystemIo {
                    operation: "create history source plugin cursor file",
                    source: error,
                }))
                .with_context(|| format!("create private temp file {}", path.display()));
            }
        }
    }
    Err(anyhow::Error::new(CaptureError::SystemInvariant(
        "failed to allocate unique history source plugin cursor file",
    )))
}

fn cleanup_cursor_file(path: Option<&PathBuf>) -> Result<()> {
    if let Some(path) = path {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CaptureError::SystemIo {
                    operation: "remove history source plugin cursor file",
                    source,
                }
                .into());
            }
        }
    }
    Ok(())
}

fn resolve_manifest_path(manifest_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        manifest_dir.join(path)
    }
}

fn shell_like_command(command: &[String]) -> String {
    command.join(" ")
}

fn stderr_snippet(value: &str) -> String {
    let mut snippet = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(12)
        .collect::<Vec<_>>()
        .join(" | ");
    if snippet.len() > MAX_PLUGIN_STDERR_SNIPPET_BYTES {
        snippet.truncate(MAX_PLUGIN_STDERR_SNIPPET_BYTES);
        snippet.push_str("...");
    }
    snippet
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn bounded_pipe_reader_accepts_exact_limit() {
        let bytes = read_pipe_with_limit(Cursor::new(b"abcd"), 4, "stdout", "plugin/default")
            .expect("output at limit should pass");
        assert_eq!(bytes, b"abcd");
    }

    #[test]
    fn bounded_pipe_reader_rejects_over_limit() {
        let error = read_pipe_with_limit(Cursor::new(b"abcde"), 4, "stdout", "plugin/default")
            .expect_err("output over limit should fail");
        assert!(
            error
                .to_string()
                .contains("history source plugin plugin/default stdout exceeded 4 byte limit"),
            "{error}"
        );
    }
}
