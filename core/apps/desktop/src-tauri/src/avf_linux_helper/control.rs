use super::*;

pub(super) fn serve_shared_vm(data_root: &Path) -> Result<()> {
    let listener = bind_shared_vm_control_listener(data_root)?;
    loop {
        let (stream, _) = listener.accept().with_context(|| {
            format!(
                "accepting {}",
                shared_vm_control_socket_path(data_root).display()
            )
        })?;
        let data_root = data_root.to_path_buf();
        std::thread::spawn(move || {
            if let Err(err) = handle_shared_vm_control_connection(&data_root, stream) {
                let _ = append_shared_vm_log_line(
                    &data_root,
                    &format!("shared VM control connection failed: {err:#}"),
                );
            }
        });
    }
}

pub(super) fn serve_guest_agent(data_root: &Path) -> Result<()> {
    let listener = bind_guest_agent_control_listener(data_root)?;
    loop {
        let (stream, _) = listener.accept().with_context(|| {
            format!(
                "accepting {}",
                shared_vm_guest_agent_socket_path(data_root).display()
            )
        })?;
        let data_root = data_root.to_path_buf();
        std::thread::spawn(move || {
            if let Err(err) = handle_guest_agent_control_connection(&data_root, stream) {
                let _ = append_shared_vm_log_line(
                    &data_root,
                    &format!("guest-agent control connection failed: {err:#}"),
                );
            }
        });
    }
}

#[cfg(unix)]
pub(super) fn bind_shared_vm_control_listener(data_root: &Path) -> Result<UnixListener> {
    let socket_path = shared_vm_control_socket_path(data_root);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if socket_path.exists() {
        fs::remove_file(&socket_path)
            .with_context(|| format!("removing stale {}", socket_path.display()))?;
    }
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding {}", socket_path.display()))?;
    Ok(listener)
}

#[cfg(unix)]
pub(super) fn bind_guest_agent_control_listener(data_root: &Path) -> Result<UnixListener> {
    let socket_path = shared_vm_guest_agent_socket_path(data_root);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if socket_path.exists() {
        fs::remove_file(&socket_path)
            .with_context(|| format!("removing stale {}", socket_path.display()))?;
    }
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding {}", socket_path.display()))?;
    Ok(listener)
}

#[cfg(not(unix))]
pub(super) fn bind_shared_vm_control_listener(_data_root: &Path) -> Result<()> {
    bail!("shared VM control listener requires unix domain sockets")
}

#[cfg(not(unix))]
pub(super) fn bind_guest_agent_control_listener(_data_root: &Path) -> Result<()> {
    bail!("guest-agent control listener requires unix domain sockets")
}

#[cfg(unix)]
pub(super) fn handle_shared_vm_control_connection(
    data_root: &Path,
    mut stream: UnixStream,
) -> Result<()> {
    let request = match read_exec_frame(&mut stream).context("reading shared VM exec request")? {
        Some(AvfLinuxExecFrame::Request(request)) => request,
        Some(other) => {
            write_exec_frame(
                &mut stream,
                &AvfLinuxExecFrame::Error(AvfLinuxExecError {
                    code: "invalid_request".to_string(),
                    message: format!("expected request frame, received {other:?}"),
                }),
            )
            .ok();
            bail!("expected request frame, received {other:?}");
        }
        None => bail!("shared VM control socket closed before request frame"),
    };

    let resolved = resolve_simulated_exec_request(data_root, &request)?;
    let forwarded_request = AvfLinuxExecRequest::new(
        resolved.command,
        resolved.args,
        resolved.cwd.display().to_string(),
        request.user,
        resolved.env,
        request.pty,
    );
    proxy_request_to_guest_agent(data_root, stream, forwarded_request)
}

#[cfg(not(unix))]
pub(super) fn handle_shared_vm_control_connection(_data_root: &Path, _stream: ()) -> Result<()> {
    bail!("shared VM control connections require unix domain sockets")
}

#[cfg(unix)]
pub(super) fn handle_guest_agent_control_connection(
    _data_root: &Path,
    mut stream: UnixStream,
) -> Result<()> {
    let request = match read_exec_frame(&mut stream).context("reading guest-agent exec request")? {
        Some(AvfLinuxExecFrame::Request(request)) => request,
        Some(other) => {
            write_exec_frame(
                &mut stream,
                &AvfLinuxExecFrame::Error(AvfLinuxExecError {
                    code: "invalid_request".to_string(),
                    message: format!("expected request frame, received {other:?}"),
                }),
            )
            .ok();
            bail!("expected request frame, received {other:?}");
        }
        None => bail!("guest-agent control socket closed before request frame"),
    };
    run_guest_agent_exec_request(stream, request)
}

#[cfg(not(unix))]
pub(super) fn handle_guest_agent_control_connection(_data_root: &Path, _stream: ()) -> Result<()> {
    bail!("guest-agent control connections require unix domain sockets")
}

#[cfg(unix)]
pub(super) fn proxy_request_to_guest_agent(
    data_root: &Path,
    client_stream: UnixStream,
    request: AvfLinuxExecRequest,
) -> Result<()> {
    let mut agent_stream =
        connect_guest_agent_control_socket(&shared_vm_guest_agent_socket_path(data_root))?;
    write_exec_frame(&mut agent_stream, &AvfLinuxExecFrame::Request(request))
        .context("writing guest-agent exec request")?;

    let mut client_reader = client_stream;
    let mut agent_reader = agent_stream;
    let client_writer = Arc::new(Mutex::new(
        client_reader
            .try_clone()
            .context("cloning shared VM client stream")?,
    ));
    let agent_writer = Arc::new(Mutex::new(
        agent_reader
            .try_clone()
            .context("cloning guest-agent control stream")?,
    ));

    let _stdin_forwarder = {
        let agent_writer = Arc::clone(&agent_writer);
        std::thread::spawn(move || loop {
            match read_exec_frame(&mut client_reader) {
                Ok(Some(
                    frame @ (AvfLinuxExecFrame::Stdin(_)
                    | AvfLinuxExecFrame::CloseStdin
                    | AvfLinuxExecFrame::Resize(_)),
                )) => {
                    let Ok(mut guard) = agent_writer.lock() else {
                        return;
                    };
                    if write_exec_frame(&mut *guard, &frame).is_err() {
                        return;
                    }
                }
                Ok(Some(_)) | Ok(None) => {
                    let Ok(mut guard) = agent_writer.lock() else {
                        return;
                    };
                    let _ = write_exec_frame(&mut *guard, &AvfLinuxExecFrame::CloseStdin);
                    return;
                }
                Err(_) => {
                    let Ok(mut guard) = agent_writer.lock() else {
                        return;
                    };
                    let _ = write_exec_frame(&mut *guard, &AvfLinuxExecFrame::CloseStdin);
                    return;
                }
            }
        })
    };

    loop {
        match read_exec_frame(&mut agent_reader).context("reading guest-agent response frame")? {
            Some(frame) => {
                let terminal = matches!(
                    frame,
                    AvfLinuxExecFrame::Exit(_) | AvfLinuxExecFrame::Error(_)
                );
                let mut guard = client_writer
                    .lock()
                    .map_err(|_| anyhow::anyhow!("shared VM client writer mutex poisoned"))?;
                write_exec_frame(&mut *guard, &frame)
                    .context("writing proxied guest-agent frame")?;
                drop(guard);
                if terminal {
                    return Ok(());
                }
            }
            None => {
                bail!("guest-agent control socket closed before sending an exit frame");
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn run_guest_agent_exec_request(
    mut stream: UnixStream,
    request: AvfLinuxExecRequest,
) -> Result<()> {
    if request.command.trim().is_empty() {
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Error(AvfLinuxExecError {
                code: "invalid_request".to_string(),
                message: "guest exec command must not be empty".to_string(),
            }),
        )
        .ok();
        bail!("guest exec command must not be empty");
    }
    if request.cwd.trim().is_empty() {
        write_exec_frame(
            &mut stream,
            &AvfLinuxExecFrame::Error(AvfLinuxExecError {
                code: "invalid_request".to_string(),
                message: "guest exec cwd must not be empty".to_string(),
            }),
        )
        .ok();
        bail!("guest exec cwd must not be empty");
    }

    let resolved = SimulatedExecRequest {
        command: request.command,
        args: request.args,
        cwd: PathBuf::from(request.cwd),
        env: request.env,
    };
    if request.pty {
        return handle_shared_vm_pty_connection(stream, resolved);
    }

    let mut child = Command::new(&resolved.command);
    child.args(&resolved.args);
    child.current_dir(&resolved.cwd);
    child.stdin(Stdio::piped());
    child.stdout(Stdio::piped());
    child.stderr(Stdio::piped());
    for (key, value) in &resolved.env {
        child.env(key, value);
    }

    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(err) => {
            write_exec_frame(
                &mut stream,
                &AvfLinuxExecFrame::Error(AvfLinuxExecError {
                    code: "spawn_failed".to_string(),
                    message: err.to_string(),
                }),
            )
            .ok();
            return Err(err).with_context(|| {
                format!(
                    "spawning guest-agent command `{}` in {}",
                    resolved.command,
                    resolved.cwd.display()
                )
            });
        }
    };

    let writer = Arc::new(Mutex::new(
        stream.try_clone().context("cloning guest-agent stream")?,
    ));
    let mut stdin_reader = stream;
    let stdin_thread = if let Some(mut child_stdin) = child.stdin.take() {
        Some(std::thread::spawn(move || loop {
            match read_exec_frame(&mut stdin_reader) {
                Ok(Some(AvfLinuxExecFrame::Stdin(bytes))) => {
                    if child_stdin.write_all(&bytes).is_err() {
                        return;
                    }
                    let _ = child_stdin.flush();
                }
                Ok(Some(AvfLinuxExecFrame::CloseStdin)) | Ok(None) => return,
                Ok(Some(_)) => return,
                Err(_) => return,
            }
        }))
    } else {
        None
    };

    let stdout_thread = child.stdout.take().map(|mut stdout| {
        let writer = Arc::clone(&writer);
        std::thread::spawn(move || {
            relay_child_output(&mut stdout, writer, true);
        })
    });
    let stderr_thread = child.stderr.take().map(|mut stderr| {
        let writer = Arc::clone(&writer);
        std::thread::spawn(move || {
            relay_child_output(&mut stderr, writer, false);
        })
    });

    let status = child.wait().context("waiting for guest-agent child")?;
    if let Some(handle) = stdout_thread {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }
    let exit_code = status.code().unwrap_or(1);
    write_exec_frame(
        &mut *writer
            .lock()
            .map_err(|_| anyhow::anyhow!("guest-agent writer mutex poisoned"))?,
        &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code }),
    )
    .context("writing guest-agent exit frame")?;
    let _ = stdin_thread;
    Ok(())
}

pub(super) fn relay_child_output(
    reader: &mut impl Read,
    writer: Arc<Mutex<UnixStream>>,
    stdout: bool,
) {
    let mut buf = [0u8; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let frame = if stdout {
                    AvfLinuxExecFrame::Stdout(buf[..n].to_vec())
                } else {
                    AvfLinuxExecFrame::Stderr(buf[..n].to_vec())
                };
                let Ok(mut guard) = writer.lock() else {
                    return;
                };
                if write_exec_frame(&mut *guard, &frame).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

pub(super) fn relay_pty_output(reader: &mut impl Read, writer: Arc<Mutex<UnixStream>>) {
    let mut buf = [0u8; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let Ok(mut guard) = writer.lock() else {
                    return;
                };
                if write_exec_frame(&mut *guard, &AvfLinuxExecFrame::Stdout(buf[..n].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

pub(super) fn handle_shared_vm_pty_connection(
    stream: UnixStream,
    resolved: SimulatedExecRequest,
) -> Result<()> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: DEFAULT_PTY_ROWS,
            cols: DEFAULT_PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("opening simulated shared VM PTY")?;
    let mut cmd = PtyCommandBuilder::new(resolved.command.clone());
    for arg in &resolved.args {
        cmd.arg(arg);
    }
    cmd.cwd(&resolved.cwd);
    for (key, value) in &resolved.env {
        cmd.env(key, value);
    }

    let child = pair.slave.spawn_command(cmd).with_context(|| {
        format!(
            "spawning simulated shared VM PTY command `{}` in {}",
            resolved.command,
            resolved.cwd.display()
        )
    })?;
    drop(pair.slave);

    let mut pty_reader = pair
        .master
        .try_clone_reader()
        .context("cloning simulated shared VM PTY reader")?;
    let mut pty_writer = pair
        .master
        .take_writer()
        .context("taking simulated shared VM PTY writer")?;
    let master = Arc::new(Mutex::new(pair.master));

    let writer = Arc::new(Mutex::new(
        stream
            .try_clone()
            .context("cloning shared VM stream for PTY output")?,
    ));
    let mut stdin_reader = stream;
    let resize_master = Arc::clone(&master);
    let _stdin_thread = std::thread::spawn(move || loop {
        match read_exec_frame(&mut stdin_reader) {
            Ok(Some(AvfLinuxExecFrame::Stdin(bytes))) => {
                if pty_writer.write_all(&bytes).is_err() {
                    return;
                }
                let _ = pty_writer.flush();
            }
            Ok(Some(AvfLinuxExecFrame::Resize(AvfLinuxExecResize { cols, rows }))) => {
                let Ok(master) = resize_master.lock() else {
                    return;
                };
                if master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Ok(Some(AvfLinuxExecFrame::CloseStdin)) | Ok(None) => return,
            Ok(Some(_)) => return,
            Err(_) => return,
        }
    });
    let output_writer = Arc::clone(&writer);
    let stdout_thread = std::thread::spawn(move || {
        relay_pty_output(&mut pty_reader, output_writer);
    });

    let child = Arc::new(Mutex::new(child));
    let exit_code = loop {
        let exit = {
            let mut child = child
                .lock()
                .map_err(|_| anyhow::anyhow!("shared VM PTY child mutex poisoned"))?;
            child.try_wait().ok().flatten()
        };
        if let Some(status) = exit {
            break i32::try_from(status.exit_code()).unwrap_or(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };

    let _ = stdout_thread.join();
    write_exec_frame(
        &mut *writer
            .lock()
            .map_err(|_| anyhow::anyhow!("shared VM PTY writer mutex poisoned"))?,
        &AvfLinuxExecFrame::Exit(AvfLinuxExecExit { exit_code }),
    )
    .context("writing shared VM PTY exit frame")?;
    Ok(())
}
