use std::{
    io::Read,
    process::{Child, ChildStderr, ChildStdout, ExitStatus},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(not(unix))]
use std::sync::mpsc;

use anyhow::{anyhow, Context, Result};

const MAX_PLUGIN_STDOUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PLUGIN_STDERR_BYTES: usize = 256 * 1024;

#[cfg(unix)]
pub(super) fn collect_child_output_with_timeout(
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
            status = child.try_wait()?;
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
pub(super) fn collect_child_output_with_timeout(
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
        if let Some(status) = child.try_wait()? {
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
            Ok((PipeKind::Stdout, result)) => {
                stdout = Some(result?);
            }
            Ok((PipeKind::Stderr, result)) => {
                stderr = Some(result?);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(anyhow!(
                    "history source plugin {source_label} timed out after {}s",
                    timeout.as_secs()
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow!(
                    "history source plugin {source_label} output reader stopped before pipes were drained"
                ));
            }
        }
    }

    if stdout_handle.join().is_err() {
        return Err(anyhow!("history source plugin stdout reader panicked"));
    }
    if stderr_handle.join().is_err() {
        return Err(anyhow!("history source plugin stderr reader panicked"));
    }
    let stdout = stdout.expect("stdout reader result");
    let stderr = stderr.expect("stderr reader result");
    Ok((status, stdout, stderr))
}

#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("read plugin pipe flags");
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("set plugin pipe nonblocking");
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
    let mut buffer = [0u8; 8192];
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
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("read history source plugin {source_label} {name}"))
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
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_pipe_with_limit_accepts_output_at_limit() {
        let bytes = read_pipe_with_limit(Cursor::new(b"abcd"), 4, "stdout", "plugin/default")
            .expect("output at limit should pass");
        assert_eq!(bytes, b"abcd");
    }

    #[test]
    fn read_pipe_with_limit_rejects_output_over_limit() {
        let err = read_pipe_with_limit(Cursor::new(b"abcde"), 4, "stdout", "plugin/default")
            .expect_err("output over limit should fail");
        assert!(
            err.to_string()
                .contains("history source plugin plugin/default stdout exceeded 4 byte limit"),
            "{err}"
        );
    }
}
