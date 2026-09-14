use std::{
    ffi::{OsStr, OsString},
    io::{self, Read},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{PluginCommandDiagnostic, PluginCommandFailureKind, PluginCommandStage};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) struct CommandOutput {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub(super) fn run_bounded(
    command: &mut Command,
    stage: PluginCommandStage,
    timeout: Duration,
    output_limit_bytes: usize,
) -> Result<CommandOutput, PluginCommandDiagnostic> {
    platform::configure(command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        diagnostic(
            stage,
            PluginCommandFailureKind::Spawn,
            None,
            Vec::new(),
            error.to_string().into_bytes(),
        )
    })?;
    let process_tree = match platform::ProcessTree::start(&child) {
        Ok(process_tree) => process_tree,
        Err(error) => {
            terminate_direct(&mut child);
            return Err(diagnostic(
                stage,
                PluginCommandFailureKind::Spawn,
                None,
                Vec::new(),
                error.to_string().into_bytes(),
            ));
        }
    };

    let stdout = match child.stdout.take() {
        Some(stdout) => match Capture::start("plugin-manager-stdout", stdout, output_limit_bytes) {
            Ok(capture) => capture,
            Err(error) => {
                terminate(&process_tree, &mut child);
                return Err(capture_failure(stage, error, Vec::new(), Vec::new()));
            }
        },
        None => {
            terminate(&process_tree, &mut child);
            return Err(capture_failure(
                stage,
                io::Error::other("stdout pipe was not available"),
                Vec::new(),
                Vec::new(),
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => match Capture::start("plugin-manager-stderr", stderr, output_limit_bytes) {
            Ok(capture) => capture,
            Err(error) => {
                terminate(&process_tree, &mut child);
                let stdout = stdout.finish().bytes;
                return Err(capture_failure(stage, error, stdout, Vec::new()));
            }
        },
        None => {
            terminate(&process_tree, &mut child);
            let stdout = stdout.finish().bytes;
            return Err(capture_failure(
                stage,
                io::Error::other("stderr pipe was not available"),
                stdout,
                Vec::new(),
            ));
        }
    };

    let started = Instant::now();
    let outcome = loop {
        if stdout.exceeded() || stderr.exceeded() {
            break WaitOutcome::OutputLimit;
        }
        match child.try_wait() {
            Ok(Some(status)) => break WaitOutcome::Exited(status),
            Ok(None) if started.elapsed() >= timeout => break WaitOutcome::Timeout,
            Ok(None) => thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed()))),
            Err(error) => break WaitOutcome::WaitError(error),
        }
    };

    // Manager commands are synchronous. Closing the containment object after
    // the direct child exits also prevents descendants from retaining pipes or
    // continuing a mutation after the receipt has been computed.
    let termination_error = process_tree.terminate().err();
    let status = match outcome {
        WaitOutcome::Exited(status) => Some(status),
        _ => {
            let _ = child.kill();
            child.wait().ok()
        }
    };
    let stdout = stdout.finish();
    let stderr = stderr.finish();

    if matches!(outcome, WaitOutcome::OutputLimit) || stdout.exceeded || stderr.exceeded {
        return Err(diagnostic(
            stage,
            PluginCommandFailureKind::OutputLimit,
            status.and_then(|status| status.code()),
            stdout.bytes,
            stderr.bytes,
        ));
    }
    if matches!(outcome, WaitOutcome::Timeout) {
        return Err(diagnostic(
            stage,
            PluginCommandFailureKind::Timeout,
            status.and_then(|status| status.code()),
            stdout.bytes,
            stderr.bytes,
        ));
    }
    if let WaitOutcome::WaitError(error) = outcome {
        return Err(capture_failure(stage, error, stdout.bytes, stderr.bytes));
    }
    if let Some(error) = stdout.error.or(stderr.error).or(termination_error) {
        return Err(capture_failure(stage, error, stdout.bytes, stderr.bytes));
    }

    let status = status.expect("an exited process has a status");
    if !status.success() {
        return Err(diagnostic(
            stage,
            PluginCommandFailureKind::NonZero,
            status.code(),
            stdout.bytes,
            stderr.bytes,
        ));
    }
    Ok(CommandOutput {
        exit_code: status.code(),
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

pub(super) fn manager_environment(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    variables
        .into_iter()
        .filter(|(name, _)| manager_environment_variable_allowed(name))
        .collect()
}

fn manager_environment_variable_allowed(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "APPDATA",
        "LOCALAPPDATA",
        "LANG",
        "LANGUAGE",
        "LC_ALL",
        "LC_COLLATE",
        "LC_CTYPE",
        "LC_MESSAGES",
        "LC_MONETARY",
        "LC_NUMERIC",
        "LC_TIME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SystemRoot",
        "WINDIR",
        "PATHEXT",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ];
    #[cfg(windows)]
    {
        ALLOWED
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed))
    }
    #[cfg(not(windows))]
    {
        ALLOWED.contains(&name)
    }
}

fn terminate(process_tree: &platform::ProcessTree, child: &mut Child) {
    let _ = process_tree.terminate();
    terminate_direct(child);
}

fn terminate_direct(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn diagnostic(
    stage: PluginCommandStage,
    kind: PluginCommandFailureKind,
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> PluginCommandDiagnostic {
    PluginCommandDiagnostic::new(stage, kind, exit_code, stdout, stderr)
}

fn capture_failure(
    stage: PluginCommandStage,
    error: io::Error,
    stdout: Vec<u8>,
    mut stderr: Vec<u8>,
) -> PluginCommandDiagnostic {
    if stderr.len() < 4096 {
        stderr.extend_from_slice(error.to_string().as_bytes());
    }
    diagnostic(
        stage,
        PluginCommandFailureKind::Capture,
        None,
        stdout,
        stderr,
    )
}

enum WaitOutcome {
    Exited(ExitStatus),
    Timeout,
    OutputLimit,
    WaitError(io::Error),
}

struct Capture {
    exceeded: Arc<AtomicBool>,
    handle: JoinHandle<CaptureResult>,
}

impl Capture {
    fn start(
        thread_name: &str,
        mut reader: impl Read + Send + 'static,
        limit: usize,
    ) -> io::Result<Self> {
        let exceeded = Arc::new(AtomicBool::new(false));
        let thread_exceeded = Arc::clone(&exceeded);
        let handle = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                let mut bytes = Vec::with_capacity(limit.min(8192));
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => {
                            return CaptureResult::success(
                                bytes,
                                thread_exceeded.load(Ordering::Acquire),
                            )
                        }
                        Ok(read) => {
                            let remaining = limit.saturating_sub(bytes.len());
                            bytes.extend_from_slice(&buffer[..read.min(remaining)]);
                            if read > remaining {
                                thread_exceeded.store(true, Ordering::Release);
                                return CaptureResult::success(bytes, true);
                            }
                        }
                        Err(error) => return CaptureResult::error(bytes, error),
                    }
                }
            })?;
        Ok(Self { exceeded, handle })
    }

    fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::Acquire)
    }

    fn finish(self) -> CaptureResult {
        self.handle.join().unwrap_or_else(|_| CaptureResult {
            bytes: Vec::new(),
            exceeded: false,
            error: Some(io::Error::other("output capture thread panicked")),
        })
    }
}

struct CaptureResult {
    bytes: Vec<u8>,
    exceeded: bool,
    error: Option<io::Error>,
}

impl CaptureResult {
    fn success(bytes: Vec<u8>, exceeded: bool) -> Self {
        Self {
            bytes,
            exceeded,
            error: None,
        }
    }

    fn error(bytes: Vec<u8>, error: io::Error) -> Self {
        Self {
            bytes,
            exceeded: false,
            error: Some(error),
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::{io, os::unix::process::CommandExt as _, process::Command};

    use super::Child;

    pub(super) fn configure(command: &mut Command) {
        command.process_group(0);
    }

    pub(super) struct ProcessTree {
        process_group: i32,
    }

    impl ProcessTree {
        pub(super) fn start(child: &Child) -> io::Result<Self> {
            let process_group = i32::try_from(child.id())
                .map_err(|_| io::Error::other("child process ID was out of range"))?;
            Ok(Self { process_group })
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: A negative PID targets the dedicated child process group.
            if unsafe { libc::kill(-self.process_group, libc::SIGKILL) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(windows)]
#[path = "process/windows.rs"]
mod platform;

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::manager_environment;

    #[test]
    fn manager_environment_is_an_explicit_allowlist() {
        let filtered = manager_environment([
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("HOME"), OsString::from("/home/test")),
            (OsString::from("XDG_CONFIG_HOME"), OsString::from("/config")),
            (OsString::from("LANG"), OsString::from("C.UTF-8")),
            (OsString::from("TMPDIR"), OsString::from("/tmp")),
            (OsString::from("SystemRoot"), OsString::from(r"C:\\Windows")),
            (OsString::from("PATHEXT"), OsString::from(".EXE;.CMD")),
            (OsString::from("CODEX_HOME"), OsString::from("/codex")),
            (
                OsString::from("CLAUDE_CONFIG_DIR"),
                OsString::from("/claude"),
            ),
            (OsString::from("GIT_CONFIG_GLOBAL"), OsString::from("/evil")),
            (OsString::from("GIT_DIR"), OsString::from("/evil")),
            (
                OsString::from("NODE_OPTIONS"),
                OsString::from("--require=/evil"),
            ),
            (OsString::from("LD_PRELOAD"), OsString::from("/evil.so")),
            (OsString::from("RUSTC_WRAPPER"), OsString::from("/evil")),
        ]);
        let names = filtered
            .iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            [
                "PATH",
                "HOME",
                "XDG_CONFIG_HOME",
                "LANG",
                "TMPDIR",
                "SystemRoot",
                "PATHEXT",
                "CODEX_HOME",
                "CLAUDE_CONFIG_DIR",
            ]
        );
    }
}
