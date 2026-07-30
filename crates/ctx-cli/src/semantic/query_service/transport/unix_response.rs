use std::{
    io::Read as _,
    os::fd::AsRawFd as _,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use super::DaemonQueryResponseTooLarge;

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

    fn remaining(&self) -> std::io::Result<Duration> {
        let remaining = self.timeout.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "daemon query response read timed out",
            ));
        }
        Ok(remaining)
    }
}

fn wait_until_readable(stream: &UnixStream, deadline: &UnixIoDeadline) -> std::io::Result<()> {
    loop {
        let mut poll_fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = deadline
            .remaining()?
            .as_millis()
            .max(1)
            .min(i32::MAX as u128) as i32;
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            continue;
        }
        if poll_fd.revents & libc::POLLNVAL != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "daemon query response socket became invalid",
            ));
        }
        if poll_fd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            return Ok(());
        }
        return Err(std::io::Error::other(format!(
            "unexpected daemon query response readiness flags {}",
            poll_fd.revents
        )));
    }
}

pub(in crate::semantic) fn read_daemon_query_response_unix(
    stream: &mut UnixStream,
    max_response_bytes: u64,
    timeout: Duration,
) -> Result<Vec<u8>> {
    const READ_CHUNK_BYTES: usize = 64 * 1024;

    stream
        .set_nonblocking(true)
        .context("configure daemon query response socket")?;
    let deadline = UnixIoDeadline::new(timeout);
    let initial_capacity = match usize::try_from(max_response_bytes.min(READ_CHUNK_BYTES as u64)) {
        Ok(limit) => limit,
        Err(_) => READ_CHUNK_BYTES,
    };
    let mut response = Vec::with_capacity(initial_capacity);
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    loop {
        deadline.remaining()?;
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
                wait_until_readable(stream, &deadline)
                    .context("wait for daemon query response readiness")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
