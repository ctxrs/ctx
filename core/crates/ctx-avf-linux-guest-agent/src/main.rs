mod process_setup;
mod protocol;
#[cfg(target_os = "linux")]
mod pty_exec;
#[cfg(test)]
mod tests;
#[cfg(target_os = "linux")]
mod vsock_server;

#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{bail, Result};

#[cfg(any(target_os = "linux", test))]
use crate::protocol::AvfLinuxExecFrame;
#[cfg(any(target_os = "linux", test))]
use crate::protocol::{read_exec_frame, write_exec_frame};
#[cfg(target_os = "linux")]
use crate::protocol::{AvfLinuxExecError, AvfLinuxExecExit};
#[cfg(target_os = "linux")]
use process_setup::{
    configure_command_process_group, configure_command_user, lookup_user, prepare_exec_request,
    terminate_exec_process_group,
};
#[cfg(target_os = "linux")]
use pty_exec::handle_pty_connection;
#[cfg(target_os = "linux")]
use vsock_server::serve;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const GUEST_VSOCK_PORT: u32 = 47001;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
#[cfg(target_os = "linux")]
const VSOCK_LISTENER_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
#[cfg(target_os = "linux")]
const GUEST_CONTROL_READY_MARKER_ENV: &str = "CTX_AVF_GUEST_CONTROL_READY_MARKER";
#[cfg(target_os = "linux")]
const DEFAULT_PTY_COLS: u16 = 80;
#[cfg(target_os = "linux")]
const DEFAULT_PTY_ROWS: u16 = 24;
// Keep guest-agent stream chunks aligned with the host helper's empirically safe
// shared-VM transport budget so streamed stdin is not truncated mid-import.
#[cfg(any(target_os = "linux", test))]
const AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD: usize = 1024;

fn main() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        serve()
    }

    #[cfg(not(target_os = "linux"))]
    {
        bail!("ctx-avf-linux-guest-agent only runs on Linux guests");
    }
}

#[cfg(target_os = "linux")]
fn handle_connection(conn: OwnedFd) -> Result<()> {
    let mut reader = File::from(conn);
    let request = match read_exec_frame(&mut reader).context("reading exec request")? {
        Some(AvfLinuxExecFrame::Request(request)) => request,
        Some(other) => {
            let writer = Arc::new(Mutex::new(
                reader
                    .try_clone()
                    .context("cloning connection for error reply")?,
            ));
            let _ = write_error_frame(
                &writer,
                "invalid_request",
                &format!("expected request frame first, received {other:?}"),
            );
            return Ok(());
        }
        None => return Ok(()),
    };
    let writer = Arc::new(Mutex::new(
        reader
            .try_clone()
            .context("cloning connection for response stream")?,
    ));

    let prepared = match prepare_exec_request(&request) {
        Ok(prepared) => prepared,
        Err(err) => {
            let _ = write_error_frame(&writer, "prepare_failed", &err.to_string());
            return Ok(());
        }
    };

    if prepared.pty {
        if let Err(err) = handle_pty_connection(reader, prepared) {
            let _ = write_error_frame(&writer, "pty_failed", &err.to_string());
        }
        return Ok(());
    }

    let mut command = Command::new(&prepared.command);
    command
        .args(&prepared.args)
        .current_dir(&prepared.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Err(err) = configure_command_process_group(&mut command) {
        let _ = write_error_frame(&writer, "spawn_setup_failed", &err.to_string());
        return Ok(());
    }
    command.env_clear();
    for (key, value) in &prepared.env {
        command.env(key, value);
    }
    if !prepared.env.contains_key("PATH") {
        command.env("PATH", DEFAULT_PATH);
    }
    if let Some(user) = prepared.user.as_deref() {
        let account = match lookup_user(user) {
            Ok(account) => account,
            Err(err) => {
                let _ = write_error_frame(&writer, "user_lookup_failed", &err.to_string());
                return Ok(());
            }
        };
        if !prepared.env.contains_key("HOME") {
            command.env("HOME", &account.home);
        }
        if !prepared.env.contains_key("USER") {
            command.env("USER", &account.user);
        }
        if !prepared.env.contains_key("LOGNAME") {
            command.env("LOGNAME", &account.user);
        }
        if let Err(err) = configure_command_user(&mut command, &account) {
            let _ = write_error_frame(&writer, "spawn_setup_failed", &err.to_string());
            return Ok(());
        }
    }

    let mut child = match command.spawn().with_context(|| {
        format!(
            "spawning guest command `{}` in {}",
            prepared.command,
            prepared.cwd.display()
        )
    }) {
        Ok(child) => child,
        Err(err) => {
            let _ = write_error_frame(&writer, "spawn_failed", &err.to_string());
            return Ok(());
        }
    };
    let child_pid = child.id();
    let Some(mut child_stdin) = child.stdin.take() else {
        let _ = write_error_frame(&writer, "spawn_failed", "guest command stdin unavailable");
        return Ok(());
    };
    let Some(mut child_stdout) = child.stdout.take() else {
        let _ = write_error_frame(&writer, "spawn_failed", "guest command stdout unavailable");
        return Ok(());
    };
    let Some(mut child_stderr) = child.stderr.take() else {
        let _ = write_error_frame(&writer, "spawn_failed", "guest command stderr unavailable");
        return Ok(());
    };

    let stdout_writer = Arc::clone(&writer);
    let stdout_thread = std::thread::spawn(move || -> Result<()> {
        relay_stream_output(&mut child_stdout, &stdout_writer, true)
    });
    let stderr_writer = Arc::clone(&writer);
    let stderr_thread = std::thread::spawn(move || -> Result<()> {
        relay_stream_output(&mut child_stderr, &stderr_writer, false)
    });

    let input_thread = std::thread::spawn(move || -> Result<()> {
        loop {
            match read_exec_frame(&mut reader) {
                Ok(Some(AvfLinuxExecFrame::Stdin(bytes))) => child_stdin
                    .write_all(&bytes)
                    .and_then(|_| child_stdin.flush())
                    .context("writing guest stdin")?,
                Ok(Some(AvfLinuxExecFrame::CloseStdin)) => {
                    drop(child_stdin);
                    return Ok(());
                }
                Ok(Some(AvfLinuxExecFrame::Resize(_))) => continue,
                Ok(None) => {
                    drop(child_stdin);
                    let _ = terminate_exec_process_group(child_pid);
                    return Ok(());
                }
                Ok(Some(other)) => {
                    let _ = terminate_exec_process_group(child_pid);
                    bail!("unexpected exec frame after request: {other:?}");
                }
                Err(err) => {
                    drop(child_stdin);
                    let _ = terminate_exec_process_group(child_pid);
                    return Err(err).context("reading exec input frame");
                }
            }
        }
    });

    let status = child.wait().context("waiting for guest command")?;
    let exit_code = status.code().unwrap_or(1);
    let input_result = input_thread
        .join()
        .map_err(|_| anyhow::anyhow!("guest exec stdin relay thread panicked"))?;
    let stdout_result = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("guest exec stdout relay thread panicked"))?;
    let stderr_result = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("guest exec stderr relay thread panicked"))?;
    if let Err(err) = &input_result {
        eprintln!(
            "guest-agent stdin relay failed for {:?} in {} as {:?}: {err:#}",
            prepared.command,
            prepared.cwd.display(),
            prepared.user
        );
    }
    if let Err(err) = &stdout_result {
        eprintln!(
            "guest-agent stdout relay failed for {:?} in {} as {:?}: {err:#}",
            prepared.command,
            prepared.cwd.display(),
            prepared.user
        );
    }
    if let Err(err) = &stderr_result {
        eprintln!(
            "guest-agent stderr relay failed for {:?} in {} as {:?}: {err:#}",
            prepared.command,
            prepared.cwd.display(),
            prepared.user
        );
    }
    if exit_code != 0 {
        eprintln!(
            "guest-agent command {:?} in {} as {:?} exited with {}",
            prepared.command,
            prepared.cwd.display(),
            prepared.user,
            exit_code
        );
    }

    write_stream_frame(
        &writer,
        AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code }),
    )
}

#[cfg(target_os = "linux")]
fn relay_stream_output(
    reader: &mut impl Read,
    writer: &Arc<Mutex<File>>,
    stdout: bool,
) -> Result<()> {
    let mut buf = [0u8; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => emit_exec_stream_frames(&buf[..n], stdout, |frame| {
                write_stream_frame(writer, frame)
            })?,
            Err(err) => return Err(err).context("reading child output"),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn emit_exec_stream_frames<F>(bytes: &[u8], stdout: bool, mut emit: F) -> Result<()>
where
    F: FnMut(AvfLinuxExecFrame) -> Result<()>,
{
    for chunk in bytes.chunks(AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD) {
        let frame = if stdout {
            AvfLinuxExecFrame::Stdout(chunk.to_vec())
        } else {
            AvfLinuxExecFrame::Stderr(chunk.to_vec())
        };
        emit(frame)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_error_frame(writer: &Arc<Mutex<File>>, code: &str, message: &str) -> Result<()> {
    write_stream_frame(
        writer,
        AvfLinuxExecFrame::Error(AvfLinuxExecError {
            code: code.to_string(),
            message: message.to_string(),
        }),
    )
}

#[cfg(target_os = "linux")]
fn write_stream_frame(writer: &Arc<Mutex<File>>, frame: AvfLinuxExecFrame) -> Result<()> {
    let mut guard = writer
        .lock()
        .map_err(|_| anyhow::anyhow!("guest-agent writer mutex poisoned"))?;
    write_exec_frame(&mut *guard, &frame).context("writing exec frame")
}
