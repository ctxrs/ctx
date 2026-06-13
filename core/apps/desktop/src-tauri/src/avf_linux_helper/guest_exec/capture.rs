use super::*;

#[cfg(unix)]
fn configure_shared_vm_control_stream_timeout(
    stream: &UnixStream,
    timeout: Option<Duration>,
) -> Result<()> {
    stream
        .set_read_timeout(timeout)
        .context("configuring shared VM control stream read timeout")?;
    stream
        .set_write_timeout(timeout)
        .context("configuring shared VM control stream write timeout")?;
    Ok(())
}

#[cfg(unix)]
fn collect_guest_exec_capture_response(
    response_stream: &mut impl Read,
) -> Result<(GuestExecTerminalFrame, Vec<u8>, Vec<u8>)> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        match read_exec_frame(response_stream)
            .context("reading AVF Linux guest exec capture frame")?
        {
            Some(AvfLinuxExecFrame::Stdout(bytes)) => stdout.extend_from_slice(&bytes),
            Some(AvfLinuxExecFrame::Stderr(bytes)) => stderr.extend_from_slice(&bytes),
            Some(AvfLinuxExecFrame::Exit(exit)) => {
                return Ok((GuestExecTerminalFrame::Exit(exit), stdout, stderr));
            }
            Some(AvfLinuxExecFrame::Error(error)) => {
                return Ok((GuestExecTerminalFrame::Error(error), stdout, stderr));
            }
            Some(
                AvfLinuxExecFrame::Request(_)
                | AvfLinuxExecFrame::Stdin(_)
                | AvfLinuxExecFrame::CloseStdin
                | AvfLinuxExecFrame::Resize(_),
            ) => bail!("received unexpected frame while waiting for guest exec result"),
            None => bail!("shared VM control socket closed before guest exec exit"),
        }
    }
}

#[cfg(unix)]
fn finish_guest_exec_capture_result(
    terminal: GuestExecTerminalFrame,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<GuestExecCaptureResult> {
    match terminal {
        GuestExecTerminalFrame::Exit(exit) => Ok(GuestExecCaptureResult {
            exit_code: exit.exit_code,
            stdout,
            stderr,
        }),
        GuestExecTerminalFrame::Error(error) => {
            let stderr_text = String::from_utf8_lossy(&stderr).trim().to_string();
            let stdout_text = String::from_utf8_lossy(&stdout).trim().to_string();
            let extra = [stderr_text, stdout_text]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if extra.is_empty() {
                bail!("guest exec failed: {} ({})", error.message, error.code);
            }
            bail!(
                "guest exec failed: {} ({})\n{}",
                error.message,
                error.code,
                extra
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn run_guest_exec_capture_over_connected_stream(
    stream: &mut (impl Read + Write),
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
) -> Result<GuestExecCaptureResult> {
    if command.trim().is_empty() {
        bail!("guest exec command must not be empty");
    }
    if cwd.as_os_str().is_empty() {
        bail!("guest exec cwd must not be empty");
    }

    let request = AvfLinuxExecRequest::new(
        command,
        args.to_vec(),
        cwd.display().to_string(),
        user.map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        env,
        false,
    );
    write_exec_frame(stream, &AvfLinuxExecFrame::Request(request))
        .context("writing AVF Linux guest exec capture request")?;
    write_exec_frame(stream, &AvfLinuxExecFrame::CloseStdin)
        .context("closing AVF Linux guest exec stdin")?;

    let (terminal, stdout, stderr) = collect_guest_exec_capture_response(stream)?;
    finish_guest_exec_capture_result(terminal, stdout, stderr)
}

#[cfg(unix)]
pub(crate) fn run_guest_exec_capture(
    control_socket: &Path,
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
    stdin_reader: Option<&mut dyn Read>,
) -> Result<GuestExecCaptureResult> {
    run_guest_exec_capture_with_socket_timeout(
        control_socket,
        cwd,
        command,
        args,
        user,
        env,
        stdin_reader,
        None,
    )
}

#[cfg(unix)]
pub(crate) fn run_guest_exec_capture_with_socket_timeout(
    control_socket: &Path,
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
    stdin_reader: Option<&mut dyn Read>,
    socket_timeout: Option<Duration>,
) -> Result<GuestExecCaptureResult> {
    if command.trim().is_empty() {
        bail!("guest exec command must not be empty");
    }
    if cwd.as_os_str().is_empty() {
        bail!("guest exec cwd must not be empty");
    }

    let mut stream = connect_shared_vm_control_socket(control_socket)?;
    configure_shared_vm_control_stream_timeout(&stream, socket_timeout)?;
    let request = AvfLinuxExecRequest::new(
        command,
        args.to_vec(),
        cwd.display().to_string(),
        user.map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        env,
        false,
    );
    write_exec_frame(&mut stream, &AvfLinuxExecFrame::Request(request))
        .context("writing AVF Linux guest exec capture request")?;

    let mut response_stream = stream
        .try_clone()
        .context("cloning shared VM control stream for capture response")?;
    configure_shared_vm_control_stream_timeout(&response_stream, socket_timeout)?;
    let response_thread = std::thread::spawn(move || -> Result<_> {
        collect_guest_exec_capture_response(&mut response_stream)
    });

    let mut stdin_error = None;
    if let Some(reader) = stdin_reader {
        let mut buf = [0u8; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
        loop {
            let read = reader
                .read(&mut buf)
                .context("reading staged guest exec stdin")?;
            if read == 0 {
                break;
            }
            if let Err(err) =
                write_exec_frame(&mut stream, &AvfLinuxExecFrame::Stdin(buf[..read].to_vec()))
                    .context("writing AVF Linux guest exec stdin frame")
            {
                stdin_error = Some(err);
                break;
            }
        }
    }
    if stdin_error.is_none() {
        if let Err(err) = write_exec_frame(&mut stream, &AvfLinuxExecFrame::CloseStdin)
            .context("closing AVF Linux guest exec stdin")
        {
            stdin_error = Some(err);
        }
    }

    let (terminal, stdout, stderr) = response_thread
        .join()
        .map_err(|_| anyhow::anyhow!("guest exec response reader thread panicked"))??;

    if let Some(err) = stdin_error {
        if !is_ignorable_guest_exec_stdin_write_error(&err) {
            return Err(err);
        }
    }

    finish_guest_exec_capture_result(terminal, stdout, stderr)
}

#[cfg(not(unix))]
pub(crate) fn run_guest_exec_capture(
    _control_socket: &Path,
    _cwd: &Path,
    _command: &str,
    _args: &[String],
    _user: Option<&str>,
    _env: HashMap<String, String>,
    _stdin_reader: Option<&mut dyn Read>,
) -> Result<GuestExecCaptureResult> {
    bail!("programmatic AVF guest exec capture requires unix domain sockets")
}

pub(crate) fn format_guest_exec_output(output: &[u8]) -> String {
    String::from_utf8_lossy(output).trim().to_string()
}

fn is_ignorable_guest_exec_stdin_write_error(err: &anyhow::Error) -> bool {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .is_some_and(|io_err| {
            matches!(
                io_err.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::NotConnected
            )
        })
}

pub(crate) fn ensure_guest_exec_success(
    context: &str,
    result: GuestExecCaptureResult,
) -> Result<()> {
    if result.exit_code == 0 {
        return Ok(());
    }
    let stdout = format_guest_exec_output(&result.stdout);
    let stderr = format_guest_exec_output(&result.stderr);
    let joined = [stderr, stdout]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        bail!("{context} failed with exit code {}", result.exit_code);
    }
    bail!(
        "{context} failed with exit code {}:\n{}",
        result.exit_code,
        joined
    );
}
