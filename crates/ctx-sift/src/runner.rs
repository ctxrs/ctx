// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Execute argv directly with bounded output capture and optional completion wait.
use std::ffi::OsString;
#[cfg(not(any(unix, windows)))]
use std::io::Write;
use std::io::{self, IsTerminal, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use sift::{CompactResult, Compactor};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

const LIMIT: usize = 8 * 1024 * 1024;
const WINDOW: Duration = Duration::from_millis(250);
const TICK: Duration = Duration::from_millis(10);

type Packet = (bool, io::Result<Vec<u8>>);

/// One child stream. Token counts exist only for a complete UTF-8 buffer that
/// was measured and successfully emitted. Inherited streams have no byte counts.
#[derive(Default)]
pub struct StreamObservation<'a> {
    pub original: Option<&'a [u8]>,
    pub compacted: Option<&'a CompactResult>,
    /// Exact original/emitted counts for a semantic presentation. These do not
    /// imply that the original stream can be restored from the presentation.
    pub presented_tokens: Option<(usize, usize)>,
    pub read_bytes: Option<u64>,
    pub emitted_bytes: Option<u64>,
}

pub enum Presentation {
    Bytes(Vec<u8>),
    #[allow(dead_code)] // API consumers may use only explicit byte transforms.
    Compacted(CompactResult),
    #[allow(dead_code)] // The CLI supplies semantic proposals; direct runners need not.
    Semantic {
        bytes: Vec<u8>,
        tokens: (usize, usize),
    },
}

pub struct Observation<'a> {
    pub stdout: StreamObservation<'a>,
    pub stderr: StreamObservation<'a>,
    pub duration: Duration,
    pub status: i32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Inherit output unchanged. Takes precedence over capture.
    pub raw: bool,
    /// Wait for complete output, even with terminal stdin/stdout. Intended for
    /// finite commands: progress/prompts are delayed until both pipes close or
    /// the combined 8 MiB limit switches output to raw streaming. Stdin remains
    /// inherited; this does not allocate a pseudo-terminal for the child.
    pub capture: bool,
}

/// Run argv directly. Windows batch files use Rust's standard batch escaping;
/// unsupported batch arguments return an error rather than being reinterpreted.
#[allow(dead_code)] // Convenience entry point when the caller does not record observations.
pub fn run(args: &[OsString], raw: bool) -> anyhow::Result<i32> {
    run_with_options(
        args,
        Options {
            raw,
            capture: false,
        },
    )
}

#[allow(dead_code)] // Convenience entry point without observations.
pub fn run_with_options(args: &[OsString], options: Options) -> anyhow::Result<i32> {
    run_observed_with_options(args, options, |_| {})
}

/// Observe one completed invocation. The callback borrows bounded originals;
/// streaming/raw-inherited output never accumulates a retrievable transcript.
/// Setup/I/O errors returning Err do not invoke the callback. Spawn statuses
/// 126/127 do invoke it, with both streams unmeasured.
#[allow(dead_code)] // Preserve the original API for callers without options.
pub fn run_observed(
    args: &[OsString],
    raw: bool,
    observer: impl FnOnce(Observation<'_>),
) -> anyhow::Result<i32> {
    run_observed_with_options(
        args,
        Options {
            raw,
            capture: false,
        },
        observer,
    )
}

/// Like `run_observed`, with explicit complete-capture control. Unix signal
/// termination/cancellation returns numeric 128 + signal; it does not re-raise
/// the signal in the caller (normal exit and signal termination differ to wait()).
pub fn run_observed_with_options(
    args: &[OsString],
    options: Options,
    observer: impl FnOnce(Observation<'_>),
) -> anyhow::Result<i32> {
    run_transformed(args, options, |_, _| None, observer)
}

/// Apply an explicitly requested view only to complete bounded streams. A
/// custom view has no CompactResult/token accounting; overflow stays raw.
pub fn run_transformed(
    args: &[OsString],
    options: Options,
    mut transform: impl FnMut(&[u8], bool) -> Option<Vec<u8>>,
    observer: impl FnOnce(Observation<'_>),
) -> anyhow::Result<i32> {
    run_presented(
        args,
        options,
        |bytes, stderr| transform(bytes, stderr).map(Presentation::Bytes),
        observer,
    )
}

/// Present complete captured streams. Explicit views may leave counts unknown;
/// semantic proposals supply exact counts without claiming reversible encoding.
pub fn run_presented(
    args: &[OsString],
    options: Options,
    mut transform: impl FnMut(&[u8], bool) -> Option<Presentation>,
    observer: impl FnOnce(Observation<'_>),
) -> anyhow::Result<i32> {
    let started = Instant::now();
    let (program, args) = args
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("missing command"))?;
    let interactive = io::stdin().is_terminal() || io::stdout().is_terminal();
    let capture = !options.raw && (options.capture || !interactive);
    #[cfg(unix)]
    let signals = Signals::new()?;
    #[cfg(windows)]
    let windows = windows::State::new()?;
    #[cfg(windows)]
    let resolved = windows::resolve_program(program);
    #[cfg(windows)]
    let mut command = Command::new(&resolved);
    #[cfg(not(windows))]
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::inherit());
    if capture {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    #[cfg(unix)]
    if !interactive {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("ctx sift: {}: {error}", program.to_string_lossy());
            observer(Observation {
                stdout: Default::default(),
                stderr: Default::default(),
                duration: started.elapsed(),
                status: 127,
            });
            return Ok(127);
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!("ctx sift: {}: {error}", program.to_string_lossy());
            observer(Observation {
                stdout: Default::default(),
                stderr: Default::default(),
                duration: started.elapsed(),
                status: 126,
            });
            return Ok(126);
        }
        Err(error) => return Err(error.into()),
    };
    let read_bytes: [Arc<AtomicU64>; 2] = Default::default();
    let emitted_bytes: [Arc<AtomicU64>; 2] = Default::default();
    let mut originals: Option<[Vec<u8>; 2]> = None;
    let mut compacted: [Option<CompactResult>; 2] = [None, None];
    let mut presented_tokens = [None, None];
    let mut process = Process {
        emitted_bytes: emitted_bytes.clone(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        exit_notification: exit_notification(&child),
        child,
        #[cfg(unix)]
        signals,
        #[cfg(unix)]
        group: !interactive,
        #[cfg(windows)]
        windows,
        cancelled: None,
        reaped: false,
        complete: false,
    };
    #[cfg(windows)]
    process.windows.assign_and_resume(&process.child)?;
    let result = (|| -> anyhow::Result<i32> {
        let (tx, rx) = mpsc::sync_channel(8);
        if capture {
            reader(
                process.child.stdout.take().unwrap(),
                false,
                tx.clone(),
                read_bytes[0].clone(),
            );
            reader(
                process.child.stderr.take().unwrap(),
                true,
                tx.clone(),
                read_bytes[1].clone(),
            );
        }
        drop(tx);
        let mut pending = [Vec::new(), Vec::new()];
        let mut size = 0;
        let mut first = None;
        let mut passthrough = !capture;
        let mut eof = !capture;
        let mut status = None;
        loop {
            process.check()?;
            if status.is_none() {
                status = process.child.try_wait()?;
                process.reaped = status.is_some();
            }
            if status.is_some() && eof {
                break;
            }
            if !passthrough
                && !options.capture
                && first.is_some_and(|time: Instant| time.elapsed() >= WINDOW)
            {
                // A descendant can keep the pipes open after the direct child exits.
                // The same deadline applies until both pipes close.
                flush_pending(&mut process, &mut pending)?;
                passthrough = true;
            }
            if !capture || eof {
                process.wait_for_exit()?;
                continue;
            }
            let timeout = if passthrough || options.capture {
                TICK
            } else {
                first.map_or(TICK, |time: Instant| {
                    WINDOW.saturating_sub(time.elapsed()).min(TICK)
                })
            };
            match rx.recv_timeout(timeout) {
                Ok((stderr, bytes)) => {
                    let bytes = bytes?;
                    first.get_or_insert_with(Instant::now);
                    if !passthrough && size + bytes.len() >= LIMIT {
                        flush_pending(&mut process, &mut pending)?;
                        passthrough = true;
                    }
                    if passthrough {
                        process.write(stderr, &bytes)?;
                    } else {
                        size += bytes.len();
                        pending[usize::from(stderr)].extend_from_slice(&bytes);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => eof = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        if !passthrough {
            originals = Some(pending);
            let mut compactor = None;
            for (index, bytes) in originals.as_ref().unwrap().iter().enumerate() {
                process.check()?;
                if let Some(output) = transform(bytes, index == 1) {
                    match output {
                        Presentation::Bytes(bytes) => process.write(index == 1, &bytes)?,
                        Presentation::Compacted(result) => {
                            process.write(index == 1, result.text.as_bytes())?;
                            compacted[index] = Some(result);
                        }
                        Presentation::Semantic { bytes, tokens } => {
                            process.write(index == 1, &bytes)?;
                            presented_tokens[index] = Some(tokens);
                        }
                    }
                    process.check()?;
                    continue;
                }
                // Tiny responses cannot amortize tokenizer startup or framing.
                let candidate = if bytes.len() >= 256 {
                    std::str::from_utf8(bytes).ok().and_then(|text| {
                        compactor
                            .get_or_insert_with(Compactor::new)
                            .as_ref()
                            .ok()
                            .map(|compactor| compactor.compact(text))
                    })
                } else {
                    None
                };
                process.write(
                    index == 1,
                    candidate
                        .as_ref()
                        .map_or(bytes.as_slice(), |result| result.text.as_bytes()),
                )?;
                compacted[index] = candidate;
                // Tokenizer work and empty output must not hide a pending signal.
                process.check()?;
            }
        }
        process.check()?;
        #[cfg(windows)]
        if status.unwrap().success() && process.cancelled.is_none() {
            // A successful launcher may intentionally leave background children.
            // Captured execution also reaches here only after both pipes close;
            // those descendants have released our output and may keep running.
            process.windows.preserve_descendants()?;
            process.check()?;
        }
        process.complete = true;
        Ok(exit_code(status.unwrap()))
    })();
    let result = process
        .cancelled
        .map_or(result, |(signal, _)| Ok(128 + signal));
    // Reap/terminate before invoking application code, which may itself do I/O.
    drop(process);
    let status = result?;
    let stream = |index: usize| StreamObservation {
        original: originals.as_ref().map(|streams| streams[index].as_slice()),
        compacted: compacted[index].as_ref(),
        presented_tokens: presented_tokens[index],
        read_bytes: capture.then(|| read_bytes[index].load(Ordering::Relaxed)),
        emitted_bytes: capture.then(|| emitted_bytes[index].load(Ordering::Relaxed)),
    };
    observer(Observation {
        stdout: stream(0),
        stderr: stream(1),
        duration: started.elapsed(),
        status,
    });
    Ok(status)
}

fn reader(
    mut input: impl Read + Send + 'static,
    stderr: bool,
    tx: SyncSender<Packet>,
    read_bytes: Arc<AtomicU64>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0; 16 * 1024];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    read_bytes.fetch_add(n as u64, Ordering::Relaxed);
                    if tx.send((stderr, Ok(buffer[..n].to_vec()))).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = tx.send((stderr, Err(error)));
                    break;
                }
            }
        }
    });
}

fn flush_pending(process: &mut Process, pending: &mut [Vec<u8>; 2]) -> io::Result<()> {
    for (index, bytes) in pending.iter_mut().enumerate() {
        process.write(index == 1, bytes)?;
        *bytes = Vec::new();
    }
    Ok(())
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(1)
    }
}

struct Process {
    emitted_bytes: [Arc<AtomicU64>; 2],
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    exit_notification: Option<std::os::fd::OwnedFd>,
    child: Child,
    #[cfg(unix)]
    signals: Signals,
    #[cfg(unix)]
    group: bool,
    #[cfg(windows)]
    windows: windows::State,
    cancelled: Option<(i32, Instant)>,
    reaped: bool,
    complete: bool,
}
impl Process {
    /// Wait until exit or the next cancellation check. A fixed sleep adds its
    /// entire interval even when a short-lived child has already finished.
    fn wait_for_exit(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(notification) = &self.exit_notification {
            use std::os::fd::AsRawFd;
            let mut descriptor = libc::pollfd {
                fd: notification.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: the owned descriptor and pollfd remain live during poll.
            let result = unsafe { libc::poll(&mut descriptor, 1, TICK.as_millis() as i32) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if let Some(notification) = &self.exit_notification {
            use std::os::fd::AsRawFd;
            // SAFETY: kevent is a plain C event record; zero initializes all fields.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: TICK.as_nanos() as _,
            };
            // SAFETY: the event output and timeout remain valid for this call.
            let result = unsafe {
                libc::kevent(
                    notification.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &timeout,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            return Ok(());
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::WAIT_FAILED;
            use windows_sys::Win32::System::Threading::WaitForSingleObject;
            // SAFETY: Child owns a process handle for the duration of this wait.
            let result =
                unsafe { WaitForSingleObject(self.child.as_raw_handle(), TICK.as_millis() as u32) };
            if result == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            std::thread::sleep(TICK);
            Ok(())
        }
    }

    fn check(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::sync::atomic::Ordering;
            let signal = self.signals.pending.swap(0, Ordering::Relaxed) as i32;
            if signal != 0 {
                self.signal(signal);
                self.cancelled.get_or_insert((signal, Instant::now()));
            }
            if self
                .cancelled
                .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
            {
                self.signal(libc::SIGKILL);
            }
            for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: one initialized pollfd, valid for the duration of this call.
                let result = unsafe { libc::poll(&mut poll, 1, 0) };
                if result >= 0
                    && poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "output consumer closed",
                    ));
                }
            }
        }
        #[cfg(windows)]
        {
            self.windows.check_cancelled(&mut self.cancelled);
        }
        Ok(())
    }
    #[cfg(unix)]
    fn signal(&self, signal: i32) {
        // A reaped PID can be reused. A private process group is still ours while
        // its descendants hold the captured pipes open.
        if self.reaped && !self.group {
            return;
        }
        let pid = self.child.id() as i32;
        // SAFETY: kill takes integer identifiers and does not access memory.
        unsafe {
            libc::kill(if self.group { -pid } else { pid }, signal);
        }
    }
    fn write(&mut self, stderr: bool, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.check()?;
            if self
                .cancelled
                .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
            {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "command cancelled",
                ));
            }
            #[cfg(unix)]
            {
                let fd = if stderr {
                    libc::STDERR_FILENO
                } else {
                    libc::STDOUT_FILENO
                };
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // Poll before bounded writes so cancellation also works under backpressure.
                // SAFETY: pollfd and byte slice remain valid through their calls.
                let ready = unsafe { libc::poll(&mut poll, 1, 10) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if ready == 0 {
                    continue;
                }
                #[allow(clippy::unnecessary_cast)] // libc uses c_int on Solaris.
                let pipe_buf = libc::PIPE_BUF as usize;
                let n =
                    unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len().min(pipe_buf)) };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                self.emitted_bytes[usize::from(stderr)].fetch_add(n as u64, Ordering::Relaxed);
                bytes = &bytes[n as usize..];
                if self
                    .cancelled
                    .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "command cancelled",
                    ));
                }
            }
            #[cfg(windows)]
            {
                self.write_windows(stderr, bytes)?;
                bytes = &[];
            }
            #[cfg(not(any(unix, windows)))]
            {
                if stderr {
                    io::stderr().write_all(bytes)?;
                    io::stderr().flush()?;
                } else {
                    io::stdout().write_all(bytes)?;
                    io::stdout().flush()?;
                }
                self.emitted_bytes[usize::from(stderr)]
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                bytes = &[];
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn exit_notification(child: &Child) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    // SAFETY: pidfd_open takes integer arguments and returns a fresh descriptor.
    // Older kernels or a restricted syscall policy keep the existing polling path.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0u32) };
    (descriptor >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor as i32) })
}

#[cfg(target_os = "macos")]
fn exit_notification(child: &Child) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: kqueue takes no arguments and returns a fresh owned descriptor.
    let descriptor = unsafe { libc::kqueue() };
    if descriptor < 0 {
        return None;
    }
    let notification = unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor) };
    // SAFETY: kevent is a plain C event record; zero initializes all fields.
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    event.ident = child.id() as _;
    event.filter = libc::EVFILT_PROC;
    event.flags = libc::EV_ADD | libc::EV_ONESHOT;
    event.fflags = libc::NOTE_EXIT;
    // SAFETY: the live queue and initialized change record are valid. A child
    // that already exited can reject registration; ordinary try_wait handles it.
    let result = unsafe {
        libc::kevent(
            notification.as_raw_fd(),
            &event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    (result == 0).then_some(notification)
}
impl Drop for Process {
    fn drop(&mut self) {
        // Also runs on read/write errors and unwinding. Never leave a child alive
        // when the consumer closes its pipe.
        #[cfg(unix)]
        if !self.complete || self.cancelled.is_some() {
            self.signal(libc::SIGKILL);
        }
        #[cfg(windows)]
        if !self.complete || self.cancelled.is_some() {
            // Also covers cancellation racing successful job disarming.
            self.windows.terminate();
        }
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(unix)]
struct Signals {
    pending: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ids: Vec<signal_hook::SigId>,
}
#[cfg(unix)]
impl Signals {
    fn new() -> io::Result<Self> {
        let mut signals = Self {
            pending: Default::default(),
            ids: Vec::new(),
        };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            signals.ids.push(signal_hook::flag::register_usize(
                signal,
                signals.pending.clone(),
                signal as usize,
            )?);
        }
        Ok(signals)
    }
}
#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

#[cfg(windows)]
#[path = "runner/windows.rs"]
mod windows;
