use super::*;

#[path = "guest_exec/capture.rs"]
mod capture;
#[path = "guest_exec/workspace.rs"]
mod workspace;

pub(super) use capture::*;
pub(super) use workspace::*;

pub(super) struct GuestExecCaptureResult {
    pub(super) exit_code: i32,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

enum GuestExecTerminalFrame {
    Exit(AvfLinuxExecExit),
    Error(AvfLinuxExecError),
}

#[cfg(unix)]
pub(super) fn run_guest_exec_cli(
    control_socket: &Path,
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
    pty: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<i32> {
    if pty {
        return run_guest_exec_process(control_socket, cwd, command, args, user, env, true);
    }

    let stdin = std::io::stdin();
    let stdin_reader = if unsafe { libc::isatty(stdin.as_raw_fd()) } == 1 {
        None
    } else {
        Some(Box::new(stdin) as Box<dyn Read + Send>)
    };
    run_guest_exec_cli_with_streaming_stdin(
        control_socket,
        cwd,
        command,
        args,
        user,
        env,
        stdin_reader,
        stdout,
        stderr,
    )
}

#[cfg(unix)]
pub(super) fn run_guest_exec_cli_with_streaming_stdin(
    control_socket: &Path,
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
    stdin_reader: Option<Box<dyn Read + Send>>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<i32> {
    if command.trim().is_empty() {
        bail!("guest exec command must not be empty");
    }
    if cwd.as_os_str().is_empty() {
        bail!("guest exec cwd must not be empty");
    }

    let mut stream = connect_shared_vm_control_socket(control_socket)?;
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
        .context("writing AVF Linux guest exec request")?;

    let writer =
        Arc::new(Mutex::new(stream.try_clone().context(
            "cloning shared VM control stream for stdin forwarding",
        )?));
    let _stdin_forwarder = if let Some(mut reader) = stdin_reader {
        let writer = Arc::clone(&writer);
        Some(std::thread::spawn(move || {
            let mut buf = [0u8; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let Ok(mut guard) = writer.lock() else {
                            return;
                        };
                        let _ = write_exec_frame(&mut *guard, &AvfLinuxExecFrame::CloseStdin);
                        return;
                    }
                    Ok(n) => {
                        let Ok(mut guard) = writer.lock() else {
                            return;
                        };
                        if write_exec_frame(
                            &mut *guard,
                            &AvfLinuxExecFrame::Stdin(buf[..n].to_vec()),
                        )
                        .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => {
                        let Ok(mut guard) = writer.lock() else {
                            return;
                        };
                        let _ = write_exec_frame(&mut *guard, &AvfLinuxExecFrame::CloseStdin);
                        return;
                    }
                }
            }
        }))
    } else {
        write_exec_frame(
            &mut *writer
                .lock()
                .map_err(|_| anyhow::anyhow!("guest exec writer mutex poisoned"))?,
            &AvfLinuxExecFrame::CloseStdin,
        )
        .context("closing AVF Linux guest exec stdin")?;
        None
    };

    loop {
        match read_exec_frame(&mut stream).context("reading AVF Linux guest exec response")? {
            Some(AvfLinuxExecFrame::Stdout(bytes)) => {
                stdout
                    .write_all(&bytes)
                    .and_then(|_| stdout.flush())
                    .context("writing guest stdout")?;
            }
            Some(AvfLinuxExecFrame::Stderr(bytes)) => {
                stderr
                    .write_all(&bytes)
                    .and_then(|_| stderr.flush())
                    .context("writing guest stderr")?;
            }
            Some(AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code })) => return Ok(exit_code),
            Some(AvfLinuxExecFrame::Error(AvfLinuxExecError { code, message })) => {
                bail!("{code}: {message}");
            }
            Some(other) => {
                bail!("received unexpected AVF Linux exec frame: {other:?}");
            }
            None => {
                bail!(
                    "shared VM control socket {} closed before sending an exit frame",
                    control_socket.display()
                );
            }
        }
    }
}

#[cfg(all(unix, test))]
pub(super) fn run_guest_exec_cli_with_capture_stdin(
    control_socket: &Path,
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
    stdin_reader: Option<&mut dyn Read>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<i32> {
    let result =
        run_guest_exec_capture(control_socket, cwd, command, args, user, env, stdin_reader)?;
    stdout
        .write_all(&result.stdout)
        .and_then(|_| stdout.flush())
        .context("writing captured guest stdout")?;
    stderr
        .write_all(&result.stderr)
        .and_then(|_| stderr.flush())
        .context("writing captured guest stderr")?;
    Ok(result.exit_code)
}

#[cfg(not(unix))]
pub(super) fn run_guest_exec_cli(
    _control_socket: &Path,
    _cwd: &Path,
    _command: &str,
    _args: &[String],
    _user: Option<&str>,
    _env: HashMap<String, String>,
    _pty: bool,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
) -> Result<i32> {
    bail!("AVF Linux guest exec relay requires unix domain sockets")
}

#[cfg(unix)]
pub(super) fn connect_shared_vm_control_socket(socket_path: &Path) -> Result<UnixStream> {
    let deadline = std::time::Instant::now() + GUEST_EXEC_CONNECT_TIMEOUT;
    loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(err)
                if err.kind() == std::io::ErrorKind::NotFound
                    || err.kind() == std::io::ErrorKind::ConnectionRefused =>
            {
                if std::time::Instant::now() >= deadline {
                    return Err(err).with_context(|| {
                        format!(
                            "connecting to shared VM control socket {}",
                            socket_path.display()
                        )
                    });
                }
                std::thread::sleep(GUEST_EXEC_CONNECT_RETRY_INTERVAL);
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "connecting to shared VM control socket {}",
                        socket_path.display()
                    )
                });
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn connect_guest_agent_control_socket(socket_path: &Path) -> Result<UnixStream> {
    let deadline = std::time::Instant::now() + GUEST_EXEC_CONNECT_TIMEOUT;
    loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(err)
                if err.kind() == std::io::ErrorKind::NotFound
                    || err.kind() == std::io::ErrorKind::ConnectionRefused =>
            {
                if std::time::Instant::now() >= deadline {
                    return Err(err).with_context(|| {
                        format!(
                            "connecting to guest-agent control socket {}",
                            socket_path.display()
                        )
                    });
                }
                std::thread::sleep(GUEST_EXEC_CONNECT_RETRY_INTERVAL);
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "connecting to guest-agent control socket {}",
                        socket_path.display()
                    )
                });
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn run_guest_exec_process(
    control_socket: &Path,
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
    pty: bool,
) -> Result<i32> {
    if command.trim().is_empty() {
        bail!("guest exec command must not be empty");
    }
    if cwd.as_os_str().is_empty() {
        bail!("guest exec cwd must not be empty");
    }

    let mut stream = connect_shared_vm_control_socket(control_socket)?;
    let request = AvfLinuxExecRequest::new(
        command,
        args.to_vec(),
        cwd.display().to_string(),
        user.map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        env,
        pty,
    );
    write_exec_frame(&mut stream, &AvfLinuxExecFrame::Request(request))
        .context("writing AVF Linux guest exec request")?;

    let writer =
        Arc::new(Mutex::new(stream.try_clone().context(
            "cloning shared VM control stream for stdin forwarding",
        )?));
    if pty {
        spawn_terminal_resize_forwarder(Arc::clone(&writer));
    }
    let stdin_writer = Arc::clone(&writer);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    let Ok(mut guard) = stdin_writer.lock() else {
                        return;
                    };
                    let _ = write_exec_frame(&mut *guard, &AvfLinuxExecFrame::CloseStdin);
                    return;
                }
                Ok(n) => {
                    let Ok(mut guard) = stdin_writer.lock() else {
                        return;
                    };
                    if write_exec_frame(&mut *guard, &AvfLinuxExecFrame::Stdin(buf[..n].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) => {
                    let Ok(mut guard) = stdin_writer.lock() else {
                        return;
                    };
                    let _ = write_exec_frame(&mut *guard, &AvfLinuxExecFrame::CloseStdin);
                    return;
                }
            }
        }
    });

    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    loop {
        match read_exec_frame(&mut stream).context("reading AVF Linux guest exec response")? {
            Some(AvfLinuxExecFrame::Stdout(bytes)) => {
                stdout
                    .write_all(&bytes)
                    .and_then(|_| stdout.flush())
                    .context("writing guest stdout")?;
            }
            Some(AvfLinuxExecFrame::Stderr(bytes)) => {
                stderr
                    .write_all(&bytes)
                    .and_then(|_| stderr.flush())
                    .context("writing guest stderr")?;
            }
            Some(AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code })) => return Ok(exit_code),
            Some(AvfLinuxExecFrame::Error(AvfLinuxExecError { code, message })) => {
                bail!("{code}: {message}");
            }
            Some(other) => {
                bail!("received unexpected AVF Linux exec frame: {other:?}");
            }
            None => {
                bail!(
                    "shared VM control socket {} closed before sending an exit frame",
                    control_socket.display()
                );
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn spawn_terminal_resize_forwarder(writer: Arc<Mutex<UnixStream>>) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let fd = stdin.as_raw_fd();
        let mut last_size = None;
        loop {
            let Some((cols, rows)) = current_terminal_size(fd) else {
                return;
            };
            if last_size != Some((cols, rows)) {
                let Ok(mut guard) = writer.lock() else {
                    return;
                };
                if write_exec_frame(
                    &mut *guard,
                    &AvfLinuxExecFrame::Resize(AvfLinuxExecResize { cols, rows }),
                )
                .is_err()
                {
                    return;
                }
                last_size = Some((cols, rows));
            }
            std::thread::sleep(GUEST_EXEC_TTY_RESIZE_POLL_INTERVAL);
        }
    });
}

#[cfg(unix)]
pub(super) fn current_terminal_size(fd: std::os::fd::RawFd) -> Option<(u16, u16)> {
    unsafe {
        if libc::isatty(fd) != 1 {
            return None;
        }
        let mut winsize = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut winsize) != 0 {
            return None;
        }
        if winsize.ws_col == 0 || winsize.ws_row == 0 {
            return None;
        }
        Some((winsize.ws_col, winsize.ws_row))
    }
}

#[cfg(not(unix))]
pub(super) fn run_guest_exec_process(
    _control_socket: &Path,
    _cwd: &Path,
    _command: &str,
    _args: &[String],
    _user: Option<&str>,
    _env: HashMap<String, String>,
    _pty: bool,
) -> Result<i32> {
    bail!("AVF Linux guest exec relay requires unix domain sockets")
}
