use std::{
    io::{Read as _, Write as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{ffi::OsStrExt as _, net::UnixStream},
    },
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use super::{
    mark_request_may_have_been_submitted, DaemonIpcWaitControl, DaemonQueryResponseTooLarge,
    UninterruptedIpcWait,
};

const CONNECT_RETRY_BACKOFF: Duration = Duration::from_millis(2);

struct UnixIoDeadline {
    started: Instant,
    timeout: Duration,
}

impl UnixIoDeadline {
    fn new(timeout: Duration) -> Self {
        Self {
            started: Instant::now(),
            timeout,
        }
    }

    fn remaining(&self, operation: &str) -> std::io::Result<Duration> {
        let remaining = self.timeout.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("daemon query {operation} timed out"),
            ));
        }
        Ok(remaining)
    }

    fn poll_timeout_ms(&self, operation: &str, quantum: Option<Duration>) -> std::io::Result<i32> {
        let remaining = self.remaining(operation)?;
        let remaining = quantum.map_or(remaining, |quantum| quantum.min(remaining));
        let millis = remaining.as_millis();
        let rounded_up = if remaining.subsec_nanos().is_multiple_of(1_000_000) {
            millis
        } else {
            millis.saturating_add(1)
        };
        Ok(rounded_up.max(1).min(i32::MAX as u128) as i32)
    }
}

fn wait_for_fd(
    fd: std::os::fd::RawFd,
    events: libc::c_short,
    deadline: &UnixIoDeadline,
    operation: &str,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<libc::c_short> {
    loop {
        control.checkpoint()?;
        let mut poll_fd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut poll_fd,
                1,
                deadline.poll_timeout_ms(operation, control.blocking_quantum())?,
            )
        };
        control.checkpoint()?;
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if result == 0 {
            deadline.remaining(operation)?;
            continue;
        }
        if poll_fd.revents & libc::POLLNVAL != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("daemon query {operation} socket became invalid"),
            )
            .into());
        }
        if poll_fd.revents & (events | libc::POLLERR | libc::POLLHUP) != 0 {
            return Ok(poll_fd.revents);
        }
        return Err(std::io::Error::other(format!(
            "unexpected daemon query {operation} readiness flags {}",
            poll_fd.revents
        ))
        .into());
    }
}

fn fcntl_get(fd: std::os::fd::RawFd, command: libc::c_int) -> std::io::Result<libc::c_int> {
    loop {
        let result = unsafe { libc::fcntl(fd, command) };
        if result >= 0 {
            return Ok(result);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn fcntl_set(
    fd: std::os::fd::RawFd,
    command: libc::c_int,
    value: libc::c_int,
) -> std::io::Result<()> {
    loop {
        let result = unsafe { libc::fcntl(fd, command, value) };
        if result >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn create_nonblocking_unix_stream() -> std::io::Result<OwnedFd> {
    let raw_fd = loop {
        let raw_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if raw_fd >= 0 {
            break raw_fd;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    };
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let descriptor_flags = fcntl_get(fd.as_raw_fd(), libc::F_GETFD)?;
    fcntl_set(
        fd.as_raw_fd(),
        libc::F_SETFD,
        descriptor_flags | libc::FD_CLOEXEC,
    )?;
    let status_flags = fcntl_get(fd.as_raw_fd(), libc::F_GETFL)?;
    fcntl_set(
        fd.as_raw_fd(),
        libc::F_SETFL,
        status_flags | libc::O_NONBLOCK,
    )?;
    Ok(fd)
}

fn unix_socket_address(path: &Path) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "daemon query socket path is invalid or too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *source as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path)
        .saturating_add(bytes.len())
        .saturating_add(1);
    let length = libc::socklen_t::try_from(length).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "daemon query socket address length is invalid",
        )
    })?;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        address.sun_len = u8::try_from(length).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "daemon query socket address length is invalid",
            )
        })?;
    }
    Ok((address, length))
}

fn socket_error(fd: std::os::fd::RawFd) -> std::io::Result<Option<std::io::Error>> {
    let mut value = 0;
    loop {
        let mut length =
            libc::socklen_t::try_from(std::mem::size_of_val(&value)).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "daemon query socket error length is invalid",
                )
            })?;
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut value as *mut libc::c_int).cast(),
                &mut length,
            )
        };
        if result == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok((value != 0).then(|| std::io::Error::from_raw_os_error(value)))
}

fn connect_daemon_query_unix(
    path: &Path,
    deadline: &UnixIoDeadline,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<UnixStream> {
    let fd = create_nonblocking_unix_stream()?;
    let (address, address_length) = unix_socket_address(path)?;
    loop {
        control.checkpoint()?;
        deadline.remaining("connect")?;
        let result = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                address_length,
            )
        };
        if result == 0 {
            return Ok(fd.into());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.raw_os_error() == Some(libc::EISCONN) {
            return Ok(fd.into());
        }
        if error.kind() == std::io::ErrorKind::WouldBlock {
            // Linux reports a full AF_UNIX accept queue as EAGAIN rather than
            // EINPROGRESS. Retrying after a small bounded pause avoids both a
            // blocking connect and a hot loop while the queue remains full.
            control.pause(CONNECT_RETRY_BACKOFF.min(deadline.remaining("connect")?))?;
            continue;
        }
        if matches!(
            error.raw_os_error(),
            Some(libc::EINPROGRESS) | Some(libc::EALREADY)
        ) {
            let _ = wait_for_fd(fd.as_raw_fd(), libc::POLLOUT, deadline, "connect", control)?;
            match socket_error(fd.as_raw_fd())? {
                None => return Ok(fd.into()),
                Some(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::EINPROGRESS) | Some(libc::EALREADY)
                    ) => {}
                Some(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    control.pause(CONNECT_RETRY_BACKOFF.min(deadline.remaining("connect")?))?;
                }
                Some(error) => return Err(error.into()),
            }
            continue;
        }
        return Err(error.into());
    }
}

fn write_daemon_query_request_unix(
    stream: &mut UnixStream,
    request: &[u8],
    deadline: &UnixIoDeadline,
    request_may_have_been_submitted: &mut bool,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<()> {
    let mut written = 0;
    while written < request.len() {
        control.checkpoint()?;
        deadline.remaining("request write")?;
        match stream.write(&request[written..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "daemon query request socket stopped accepting bytes",
                )
                .into());
            }
            Ok(count) => {
                *request_may_have_been_submitted = true;
                written = written.saturating_add(count);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let _ = wait_for_fd(
                    stream.as_raw_fd(),
                    libc::POLLOUT,
                    deadline,
                    "request write",
                    control,
                )?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_daemon_query_response_unix_with_deadline(
    stream: &mut UnixStream,
    max_response_bytes: u64,
    deadline: &UnixIoDeadline,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<Vec<u8>> {
    const READ_CHUNK_BYTES: usize = 64 * 1024;

    let initial_capacity = match usize::try_from(max_response_bytes.min(READ_CHUNK_BYTES as u64)) {
        Ok(limit) => limit,
        Err(_) => READ_CHUNK_BYTES,
    };
    let mut response = Vec::with_capacity(initial_capacity);
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    loop {
        control.checkpoint()?;
        deadline.remaining("response read")?;
        let remaining_with_sentinel = max_response_bytes
            .saturating_sub(response.len() as u64)
            .saturating_add(1);
        let read_limit = remaining_with_sentinel.min(READ_CHUNK_BYTES as u64) as usize;
        match stream.read(&mut chunk[..read_limit]) {
            Ok(0) => return Ok(response),
            Ok(read) => {
                response.extend_from_slice(&chunk[..read]);
                if response.len() as u64 > max_response_bytes {
                    return Err(DaemonQueryResponseTooLarge::new(max_response_bytes).into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let _ = wait_for_fd(
                    stream.as_raw_fd(),
                    libc::POLLIN,
                    deadline,
                    "response read",
                    control,
                )
                .context("wait for daemon query response readiness")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

pub fn daemon_query_roundtrip_unix(
    path: &Path,
    request: &[u8],
    timeout: Duration,
    max_response_bytes: u64,
) -> Result<Vec<u8>> {
    daemon_query_roundtrip_unix_with_control(
        path,
        request,
        timeout,
        max_response_bytes,
        &mut UninterruptedIpcWait,
    )
}

pub(super) fn daemon_query_roundtrip_unix_with_control(
    path: &Path,
    request: &[u8],
    timeout: Duration,
    max_response_bytes: u64,
    control: &mut dyn DaemonIpcWaitControl,
) -> Result<Vec<u8>> {
    let deadline = UnixIoDeadline::new(timeout);
    let mut stream = connect_daemon_query_unix(path, &deadline, control)
        .with_context(|| format!("connect daemon query socket {}", path.display()))?;
    let mut request_may_have_been_submitted = false;
    if let Err(error) = write_daemon_query_request_unix(
        &mut stream,
        request,
        &deadline,
        &mut request_may_have_been_submitted,
        control,
    ) {
        let error = error.context("write daemon query request");
        return Err(if request_may_have_been_submitted {
            mark_request_may_have_been_submitted(error)
        } else {
            error
        });
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    read_daemon_query_response_unix_with_deadline(
        &mut stream,
        max_response_bytes,
        &deadline,
        control,
    )
    .context("read daemon query response")
    .map_err(mark_request_may_have_been_submitted)
}

pub fn read_daemon_query_response_unix(
    stream: &mut UnixStream,
    max_response_bytes: u64,
    timeout: Duration,
) -> Result<Vec<u8>> {
    stream
        .set_nonblocking(true)
        .context("configure daemon query response socket")?;
    let deadline = UnixIoDeadline::new(timeout);
    read_daemon_query_response_unix_with_deadline(
        stream,
        max_response_bytes,
        &deadline,
        &mut UninterruptedIpcWait,
    )
}

#[cfg(test)]
mod cancellation_tests {
    use std::io::Read as _;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use super::*;

    #[derive(Debug)]
    struct TestCancelled;

    impl std::fmt::Display for TestCancelled {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("test IPC cancellation")
        }
    }

    impl std::error::Error for TestCancelled {}

    struct CheckpointControl {
        checks: usize,
        cancel_at: usize,
    }

    impl DaemonIpcWaitControl for CheckpointControl {
        fn checkpoint(&mut self) -> Result<()> {
            self.checks += 1;
            if self.checks == self.cancel_at {
                Err(TestCancelled.into())
            } else {
                Ok(())
            }
        }

        fn blocking_quantum(&self) -> Option<Duration> {
            Some(Duration::from_millis(1))
        }
    }

    #[test]
    fn non_ready_poll_observes_the_post_wait_checkpoint() {
        let (reader, _writer) = UnixStream::pair().unwrap();
        let mut control = CheckpointControl {
            checks: 0,
            cancel_at: 2,
        };

        let error = wait_for_fd(
            reader.as_raw_fd(),
            libc::POLLIN,
            &UnixIoDeadline::new(Duration::from_secs(1)),
            "test read",
            &mut control,
        )
        .unwrap_err();

        assert!(error.is::<TestCancelled>());
        assert_eq!(control.checks, 2);
    }

    struct BarrierControl {
        cancelled: Arc<AtomicBool>,
    }

    impl DaemonIpcWaitControl for BarrierControl {
        fn checkpoint(&mut self) -> Result<()> {
            if self.cancelled.load(Ordering::SeqCst) {
                Err(TestCancelled.into())
            } else {
                Ok(())
            }
        }

        fn blocking_quantum(&self) -> Option<Duration> {
            Some(Duration::from_millis(1))
        }
    }

    #[test]
    fn cancellation_during_response_wait_preserves_submission_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("ipc.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let server_cancelled = Arc::clone(&cancelled);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 3];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"{}\n");
            server_cancelled.store(true, Ordering::SeqCst);
            let mut closed = [0u8; 1];
            let _ = stream.read(&mut closed);
        });
        let mut control = BarrierControl { cancelled };

        let error = daemon_query_roundtrip_unix_with_control(
            &socket,
            b"{}\n",
            Duration::from_secs(1),
            1024,
            &mut control,
        )
        .unwrap_err();
        server.join().unwrap();

        assert!(error.chain().any(|cause| cause.is::<TestCancelled>()));
        assert!(super::super::request_may_have_been_submitted(&error));
    }
}
