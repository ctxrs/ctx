use super::*;
#[cfg(target_os = "macos")]
use objc2_virtualization::{VZVirtioSocketConnection, VZVirtioSocketDevice};
#[cfg(all(target_os = "macos", unix))]
use std::os::fd::FromRawFd;

#[cfg(unix)]
fn io_error_is_benign(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
    )
}

#[cfg(unix)]
fn write_exec_error_frame_best_effort(
    writer: &mut impl Write,
    code: &str,
    message: impl Into<String>,
) {
    let _ = write_exec_frame(
        writer,
        &AvfLinuxExecFrame::Error(AvfLinuxExecError {
            code: code.to_string(),
            message: message.into(),
        }),
    );
}

#[cfg(unix)]
fn close_guest_exec_stdin_best_effort(writer: &Arc<Mutex<File>>) {
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    let _ = write_exec_frame(&mut *guard, &AvfLinuxExecFrame::CloseStdin);
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
enum SharedVmGuestControlConnectOutcome {
    Connected(File),
    Retryable(String),
    Fatal(String),
}

#[cfg(target_os = "macos")]
pub(crate) fn is_transient_guest_control_connect_nserror(domain: &str, code: isize) -> bool {
    domain == "NSPOSIXErrorDomain"
        && matches!(
            code as i32,
            libc::ECONNRESET
                | libc::ECONNABORTED
                | libc::ECONNREFUSED
                | libc::ETIMEDOUT
                | libc::EAGAIN
                | libc::EINTR
                | libc::ENOTCONN
        )
}

#[cfg(all(target_os = "macos", unix))]
fn connect_shared_vm_guest_control_socket_once(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
) -> Result<SharedVmGuestControlConnectOutcome> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let virtual_machine_addr = (&**virtual_machine as *const VZVirtualMachine) as usize;
    let request_sender = sender.clone();
    exec_on_dispatch_queue(
        queue,
        "shared AVF Linux VM guest control connect dispatch",
        move || -> Result<()> {
            let completion_sender = request_sender.clone();
            let completion = RcBlock::new(
                move |connection: *mut VZVirtioSocketConnection, error: *mut NSError| {
                    let result = if !error.is_null() {
                        let error = unsafe { &*error };
                        let domain = error.domain().to_string();
                        let code = error.code();
                        let message = format_nserror(error);
                        if is_transient_guest_control_connect_nserror(&domain, code) {
                            SharedVmGuestControlConnectOutcome::Retryable(message)
                        } else {
                            SharedVmGuestControlConnectOutcome::Fatal(message)
                        }
                    } else if connection.is_null() {
                        SharedVmGuestControlConnectOutcome::Fatal(
                            "guest control connection completed without a socket".to_string(),
                        )
                    } else {
                        let fd = unsafe { (*connection).fileDescriptor() };
                        if fd < 0 {
                            SharedVmGuestControlConnectOutcome::Fatal(
                                "guest control connection reported a closed file descriptor"
                                    .to_string(),
                            )
                        } else {
                            let dup_fd = unsafe { libc::dup(fd) };
                            if dup_fd < 0 {
                                SharedVmGuestControlConnectOutcome::Fatal(format!(
                                    "duplicating guest control file descriptor failed: {}",
                                    std::io::Error::last_os_error()
                                ))
                            } else {
                                SharedVmGuestControlConnectOutcome::Connected(unsafe {
                                    File::from_raw_fd(dup_fd)
                                })
                            }
                        }
                    };
                    let _ = completion_sender.send(result);
                },
            );

            let virtual_machine = unsafe { &*(virtual_machine_addr as *const VZVirtualMachine) };
            let socket_devices = unsafe { virtual_machine.socketDevices() };
            let Some(socket_device) = socket_devices.iter().next() else {
                bail!("shared AVF Linux VM has no socket devices configured");
            };
            let socket_device =
                unsafe { &*((&*socket_device) as *const _ as *const VZVirtioSocketDevice) };
            unsafe {
                socket_device.connectToPort_completionHandler(
                    SHARED_VM_GUEST_CONTROL_VSOCK_PORT,
                    &completion,
                );
            }
            Ok(())
        },
    )??;

    match receiver.recv_timeout(GUEST_EXEC_CONNECT_TIMEOUT) {
        Ok(result) => Ok(result),
        Err(mpsc::RecvTimeoutError::Timeout) => bail!(
            "timed out waiting for guest vsock port {}",
            SHARED_VM_GUEST_CONTROL_VSOCK_PORT
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!("guest control completion handler disconnected unexpectedly")
        }
    }
}

#[cfg(all(target_os = "macos", unix))]
pub(crate) fn connect_shared_vm_guest_control_socket(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
) -> Result<File> {
    let deadline = std::time::Instant::now() + GUEST_EXEC_CONNECT_TIMEOUT;
    loop {
        match connect_shared_vm_guest_control_socket_once(queue, virtual_machine)? {
            SharedVmGuestControlConnectOutcome::Connected(file) => return Ok(file),
            SharedVmGuestControlConnectOutcome::Retryable(message) => {
                if std::time::Instant::now() >= deadline {
                    bail!(
                        "timed out waiting for guest vsock port {} after transient connect errors: {}",
                        SHARED_VM_GUEST_CONTROL_VSOCK_PORT,
                        message
                    );
                }
                std::thread::sleep(GUEST_EXEC_CONNECT_RETRY_INTERVAL);
            }
            SharedVmGuestControlConnectOutcome::Fatal(message) => {
                return Err(anyhow::anyhow!(message)).with_context(|| {
                    format!(
                        "connecting to guest vsock port {}",
                        SHARED_VM_GUEST_CONTROL_VSOCK_PORT
                    )
                });
            }
        }
    }
}

#[cfg(all(target_os = "macos", unix))]
fn socket_timeout_to_timeval(timeout: Option<Duration>) -> libc::timeval {
    match timeout {
        Some(timeout) => libc::timeval {
            tv_sec: timeout.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
            tv_usec: timeout.subsec_micros() as libc::suseconds_t,
        },
        None => libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
    }
}

#[cfg(all(target_os = "macos", unix))]
fn configure_guest_control_socket_timeout(socket: &File, timeout: Option<Duration>) -> Result<()> {
    use std::os::fd::AsRawFd;

    let fd = socket.as_raw_fd();
    let timeout = socket_timeout_to_timeval(timeout);
    let optlen = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
    for (option, direction) in [(libc::SO_RCVTIMEO, "read"), (libc::SO_SNDTIMEO, "write")] {
        let status = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&timeout as *const libc::timeval).cast(),
                optlen,
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("configuring shared AVF Linux guest control {direction} timeout")
            });
        }
    }
    Ok(())
}

#[cfg(all(target_os = "macos", unix))]
pub(in super::super) fn run_owner_guest_exec_capture(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
    cwd: &Path,
    command: &str,
    args: &[String],
    user: Option<&str>,
    env: HashMap<String, String>,
) -> Result<GuestExecCaptureResult> {
    let mut socket = connect_shared_vm_guest_control_socket(queue, virtual_machine)?;
    configure_guest_control_socket_timeout(&socket, Some(SHARED_VM_RUNTIME_GUEST_EXEC_IO_TIMEOUT))?;
    run_guest_exec_capture_over_connected_stream(&mut socket, cwd, command, args, user, env)
}

pub(in super::super) fn shared_vm_owner_guest_probe_ready(data_root: &Path) -> bool {
    shared_vm_guest_control_ready_path(data_root).is_file()
}

#[cfg(all(target_os = "macos", unix))]
pub(crate) fn relay_shared_vm_control_client(client: UnixStream, guest: File) -> Result<()> {
    // The listener itself stays nonblocking so the owner loop can poll `accept()`, but the
    // per-client relay must switch back to blocking mode before proxying framed exec traffic.
    // Otherwise large stdin streams like disk-isolated tar imports can race with early guest
    // response frames and spuriously fail on `WouldBlock` while the client has not started
    // reading yet.
    client
        .set_nonblocking(false)
        .context("restoring shared VM control client blocking mode")?;
    let mut client_reader = client;
    let mut guest_reader = guest.try_clone().context("cloning guest control socket")?;
    let client_writer = Arc::new(Mutex::new(
        client_reader
            .try_clone()
            .context("cloning shared VM control client")?,
    ));
    let guest_writer = Arc::new(Mutex::new(guest));

    let _client_forwarder = {
        let guest_writer = Arc::clone(&guest_writer);
        std::thread::spawn(move || loop {
            match read_exec_frame(&mut client_reader) {
                Ok(Some(
                    frame @ (AvfLinuxExecFrame::Request(_)
                    | AvfLinuxExecFrame::Stdin(_)
                    | AvfLinuxExecFrame::CloseStdin
                    | AvfLinuxExecFrame::Resize(_)),
                )) => {
                    let Ok(mut guard) = guest_writer.lock() else {
                        return;
                    };
                    if write_exec_frame(&mut *guard, &frame).is_err() {
                        return;
                    }
                }
                Ok(Some(_)) | Ok(None) => {
                    close_guest_exec_stdin_best_effort(&guest_writer);
                    return;
                }
                Err(_) => {
                    close_guest_exec_stdin_best_effort(&guest_writer);
                    return;
                }
            }
        })
    };

    loop {
        match read_exec_frame(&mut guest_reader) {
            Ok(Some(frame)) => {
                let terminal = matches!(
                    frame,
                    AvfLinuxExecFrame::Exit(_) | AvfLinuxExecFrame::Error(_)
                );
                let mut guard = client_writer.lock().map_err(|_| {
                    anyhow::anyhow!("shared VM control client writer mutex poisoned")
                })?;
                write_exec_frame(&mut *guard, &frame)
                    .context("writing proxied shared VM guest frame")?;
                drop(guard);
                if terminal {
                    return Ok(());
                }
            }
            Ok(None) => {
                let message = "shared VM guest control stream closed before sending an exit frame";
                if let Ok(mut guard) = client_writer.lock() {
                    write_exec_error_frame_best_effort(
                        &mut *guard,
                        "guest_control_stream_closed",
                        message,
                    );
                }
                bail!(message);
            }
            Err(err) => {
                let code = if io_error_is_benign(&err) {
                    "guest_control_stream_closed"
                } else {
                    "guest_control_stream_failed"
                };
                let message = format!("reading shared VM guest response frame failed: {err}");
                if let Ok(mut guard) = client_writer.lock() {
                    write_exec_error_frame_best_effort(&mut *guard, code, &message);
                }
                return Err(err).context("reading shared VM guest response frame");
            }
        }
    }
}

#[cfg(all(target_os = "macos", unix))]
pub(crate) fn service_real_shared_vm_control_clients(
    queue: &DispatchQueue,
    virtual_machine: &Retained<VZVirtualMachine>,
    listener: &UnixListener,
    data_root: &Path,
) -> Result<()> {
    loop {
        match listener.accept() {
            Ok((mut client, _)) => {
                let guest = match connect_shared_vm_guest_control_socket(queue, virtual_machine) {
                    Ok(guest) => guest,
                    Err(err) => {
                        let message = format!(
                            "connecting to guest vsock port {SHARED_VM_GUEST_CONTROL_VSOCK_PORT}: {err:#}"
                        );
                        append_shared_vm_log_line(data_root, &message)?;
                        let _ = write_exec_frame(
                            &mut client,
                            &AvfLinuxExecFrame::Error(AvfLinuxExecError {
                                code: "guest_control_connect_failed".to_string(),
                                message,
                            }),
                        );
                        continue;
                    }
                };
                let log_root = data_root.to_path_buf();
                std::thread::spawn(move || {
                    if let Err(err) = relay_shared_vm_control_client(client, guest) {
                        let _ = append_shared_vm_log_line(
                            &log_root,
                            &format!("real shared VM guest relay failed: {err:#}"),
                        );
                    }
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(err) => return Err(err).context("accepting shared VM control client"),
        }
    }
}
