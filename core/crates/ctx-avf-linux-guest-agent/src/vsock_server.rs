use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    handle_connection, GUEST_CONTROL_READY_MARKER_ENV, GUEST_VSOCK_PORT,
    VSOCK_LISTENER_RETRY_INTERVAL,
};

pub(crate) fn serve() -> Result<()> {
    let ready_marker = guest_control_ready_marker_path();
    eprintln!(
        "guest-agent starting serve loop (ready_marker={})",
        ready_marker
            .as_deref()
            .map(Path::display)
            .map(|display| display.to_string())
            .unwrap_or_else(|| "<unset>".to_string())
    );
    clear_guest_control_ready_marker(ready_marker.as_deref());
    loop {
        clear_guest_control_ready_marker(ready_marker.as_deref());
        let listener = match wait_for_vsock_listener(GUEST_VSOCK_PORT) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("guest-agent listener setup failed permanently: {err:#}");
                std::thread::sleep(VSOCK_LISTENER_RETRY_INTERVAL);
                continue;
            }
        };
        announce_guest_control_ready(ready_marker.as_deref(), GUEST_VSOCK_PORT);
        loop {
            match accept_vsock_connection(&listener) {
                Ok(conn) => {
                    std::thread::spawn(move || {
                        if let Err(err) = handle_connection(conn) {
                            eprintln!("guest-agent connection failed: {err:#}");
                        }
                    });
                }
                Err(err) if is_transient_vsock_accept_error(&err) => {
                    eprintln!("guest-agent transient accept error: {err:#}");
                    continue;
                }
                Err(err) => {
                    clear_guest_control_ready_marker(ready_marker.as_deref());
                    eprintln!("guest-agent accept failed, recreating listener: {err:#}");
                    break;
                }
            }
        }
    }
}

fn guest_control_ready_marker_path() -> Option<PathBuf> {
    std::env::var_os(GUEST_CONTROL_READY_MARKER_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn clear_guest_control_ready_marker(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

fn announce_guest_control_ready(path: Option<&Path>, port: u32) {
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!(
                    "guest-agent failed to create ready-marker parent {}: {err}",
                    parent.display()
                );
            }
        }
        if let Err(err) = fs::write(path, format!("listening:{port}\n")) {
            eprintln!(
                "guest-agent failed to publish ready marker {}: {err}",
                path.display()
            );
        }
    }
    eprintln!("guest-agent listening on AF_VSOCK port {port}");
}

fn bind_vsock_listener(port: u32) -> Result<OwnedFd> {
    eprintln!("guest-agent creating AF_VSOCK listener socket on port {port}");
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        anyhow::bail!(
            "creating AF_VSOCK listener failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let listener = unsafe { OwnedFd::from_raw_fd(fd) };
    eprintln!(
        "guest-agent created AF_VSOCK listener fd {} for port {port}",
        listener.as_raw_fd()
    );
    let addr = libc::sockaddr_vm {
        svm_family: libc::AF_VSOCK as libc::sa_family_t,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: libc::VMADDR_CID_ANY,
        svm_zero: [0; 4],
    };
    eprintln!(
        "guest-agent binding AF_VSOCK listener fd {} on port {port}",
        listener.as_raw_fd()
    );
    let bind_rc = unsafe {
        libc::bind(
            listener.as_raw_fd(),
            (&addr as *const libc::sockaddr_vm).cast(),
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if bind_rc != 0 {
        anyhow::bail!(
            "binding AF_VSOCK listener on port {port} failed: {}",
            std::io::Error::last_os_error()
        );
    }
    eprintln!(
        "guest-agent bound AF_VSOCK listener fd {} on port {port}",
        listener.as_raw_fd()
    );
    eprintln!("guest-agent enabling listen() on AF_VSOCK port {port}");
    let listen_rc = unsafe { libc::listen(listener.as_raw_fd(), 128) };
    if listen_rc != 0 {
        anyhow::bail!(
            "listening on AF_VSOCK port {port} failed: {}",
            std::io::Error::last_os_error()
        );
    }
    eprintln!("guest-agent listen() succeeded on AF_VSOCK port {port}");
    Ok(listener)
}

fn wait_for_vsock_listener(port: u32) -> Result<OwnedFd> {
    loop {
        match bind_vsock_listener(port) {
            Ok(listener) => return Ok(listener),
            Err(err) => {
                eprintln!("guest-agent waiting for AF_VSOCK port {port}: {err:#}");
                std::thread::sleep(VSOCK_LISTENER_RETRY_INTERVAL);
            }
        }
    }
}

fn accept_vsock_connection(listener: &OwnedFd) -> Result<OwnedFd> {
    let fd = unsafe {
        libc::accept(
            listener.as_raw_fd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if fd < 0 {
        anyhow::bail!(
            "accepting AF_VSOCK connection failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn is_transient_vsock_accept_error(err: &anyhow::Error) -> bool {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .and_then(std::io::Error::raw_os_error)
        .is_some_and(|code| {
            matches!(
                code,
                libc::EINTR | libc::EAGAIN | libc::ECONNABORTED | libc::ECONNRESET
            )
        })
}
