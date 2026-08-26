use std::{
    io::{self, Write as _},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use crate::{
    limits::LimitConfiguration,
    protocol::InstalledCompanion,
    request::{CancellationToken, CapturedProcessRequest},
    BridgeError, ConcurrencyPermit, ExitClass, McpFinishOutcome, TerminationReason,
};

use super::{classify_exit, configure_command, platform};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
// The only post-outcome operation deadline. It covers receipt write/flush and
// direct-child exit validation in the established bounded delivery window.
const FINALIZATION_GRACE: Duration = Duration::from_secs(60);

pub(crate) struct McpProcessOutput {
    pub(crate) response_frame: Vec<u8>,
    pub(crate) outcome: SyncSender<McpFinishOutcome>,
    pub(crate) lifecycle_owner: thread::JoinHandle<Result<(), BridgeError>>,
}

pub(crate) fn launch_mcp(
    companion: &InstalledCompanion,
    request: CapturedProcessRequest,
    cancellation: CancellationToken,
    limits: LimitConfiguration,
    permit: ConcurrencyPermit,
) -> Result<McpProcessOutput, BridgeError> {
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    let (outcome_sender, outcome_receiver) = mpsc::sync_channel(1);
    let companion = companion.clone();
    let lifecycle_owner = thread::Builder::new()
        .name("ctx-companion-mcp-owner".to_owned())
        .spawn(move || {
            LifecycleOwner::spawn_and_run(
                companion,
                request,
                cancellation,
                limits,
                permit,
                response_sender,
                outcome_receiver,
            )
        })
        .map_err(BridgeError::Transport)?;
    match response_receiver.recv() {
        Ok(Ok(response_frame)) => Ok(McpProcessOutput {
            response_frame,
            outcome: outcome_sender,
            lifecycle_owner,
        }),
        Ok(Err(error)) => {
            let _ = lifecycle_owner.join();
            Err(error)
        }
        Err(_) => match lifecycle_owner.join() {
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) | Err(_) => Err(BridgeError::WorkerFailed),
        },
    }
}

struct LifecycleOwner {
    child: Child,
    tree: platform::ProcessTree,
    stdin: Option<ChildStdin>,
    #[cfg(windows)]
    pipe_writer: Option<platform::PipeWriter>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    request_frame: Vec<u8>,
    #[cfg(unix)]
    request_offset: usize,
    request_complete: bool,
    response_frame: Vec<u8>,
    frame_seen: bool,
    cancellation: CancellationToken,
    started: Instant,
    wall_time: Duration,
    stdout_limit: usize,
    terminated: bool,
    _permit: ConcurrencyPermit,
}

impl LifecycleOwner {
    #[allow(clippy::too_many_arguments)]
    fn spawn_and_run(
        companion: InstalledCompanion,
        request: CapturedProcessRequest,
        cancellation: CancellationToken,
        limits: LimitConfiguration,
        permit: ConcurrencyPermit,
        response_sender: SyncSender<Result<Vec<u8>, BridgeError>>,
        outcome_receiver: Receiver<McpFinishOutcome>,
    ) -> Result<(), BridgeError> {
        let CapturedProcessRequest {
            control,
            stdin: request_frame,
        } = request;
        let mut command = std::process::Command::new(companion.executable());
        configure_command(&mut command, &control)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = Instant::now();
        let (mut child, tree) = platform::spawn(&mut command)?;
        let pipes = (|| {
            let stdin = take_pipe(child.stdin.take(), "child stdin missing")?;
            let stdout = take_pipe(child.stdout.take(), "child stdout missing")?;
            let stderr = take_pipe(child.stderr.take(), "child stderr missing")?;
            platform::prepare_pipes(&stdin, &stdout, &stderr).map_err(BridgeError::Transport)?;
            Ok::<_, BridgeError>((stdin, stdout, stderr))
        })();
        let (stdin, stdout, stderr) = match pipes {
            Ok(pipes) => pipes,
            Err(error) => {
                tree.terminate();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut owner = Self {
            child,
            tree,
            stdin: Some(stdin),
            #[cfg(windows)]
            pipe_writer: None,
            stdout: Some(stdout),
            stderr: Some(stderr),
            request_frame,
            #[cfg(unix)]
            request_offset: 0,
            request_complete: false,
            response_frame: Vec::new(),
            frame_seen: false,
            cancellation,
            started,
            wall_time: limits.captured_wall_time,
            stdout_limit: limits.stdout_bytes,
            terminated: false,
            _permit: permit,
        };
        #[cfg(windows)]
        {
            let stdin = owner.stdin.take().ok_or(BridgeError::WorkerFailed)?;
            owner.pipe_writer = Some(
                match platform::PipeWriter::spawn(stdin, owner.request_frame.clone()) {
                    Ok(writer) => writer,
                    Err(error) => return Err(BridgeError::Transport(error)),
                },
            );
        }
        owner.run(response_sender, outcome_receiver)
    }

    fn run(
        &mut self,
        response_sender: SyncSender<Result<Vec<u8>, BridgeError>>,
        outcome_receiver: Receiver<McpFinishOutcome>,
    ) -> Result<(), BridgeError> {
        loop {
            self.poll_request_write()?;
            self.drain_pipes(false)?;
            if let Some(status) = self.try_wait()? {
                return Err(BridgeError::McpExchangeFailed {
                    exit: classify_exit(status),
                });
            }
            if self.cancellation.is_cancelled() {
                if let Ok(outcome) = outcome_receiver.try_recv() {
                    return self.finalize(outcome);
                }
                return Err(self.terminated(TerminationReason::Cancelled));
            }
            if self.started.elapsed() >= self.wall_time {
                if let Ok(outcome) = outcome_receiver.try_recv() {
                    return self.finalize(outcome);
                }
                return Err(self.terminated(TerminationReason::WallTime));
            }
            if self.request_complete && self.frame_seen {
                // Check bytes already ready, then close Core's local read ends.
                // Escaped pipe holders cannot retain this owner or its permit.
                self.drain_pipes(true)?;
                self.stdout.take();
                self.stderr.take();
                let response = std::mem::take(&mut self.response_frame);
                response_sender
                    .send(Ok(response))
                    .map_err(|_| BridgeError::WorkerFailed)?;
                return self.await_outcome(outcome_receiver);
            }
            self.sleep_until(self.started + self.wall_time);
        }
    }

    fn await_outcome(
        &mut self,
        outcome_receiver: Receiver<McpFinishOutcome>,
    ) -> Result<(), BridgeError> {
        loop {
            // A queued known outcome wins over cancellation/deadline checks.
            match outcome_receiver.try_recv() {
                Ok(outcome) => return self.finalize(outcome),
                Err(TryRecvError::Disconnected) => return Err(BridgeError::WorkerFailed),
                Err(TryRecvError::Empty) => {}
            }
            if self.cancellation.is_cancelled() {
                if let Ok(outcome) = outcome_receiver.try_recv() {
                    return self.finalize(outcome);
                }
                return Err(self.terminated(TerminationReason::Cancelled));
            }
            if self.started.elapsed() >= self.wall_time {
                if let Ok(outcome) = outcome_receiver.try_recv() {
                    return self.finalize(outcome);
                }
                return Err(self.terminated(TerminationReason::WallTime));
            }
            self.sleep_until(self.started + self.wall_time);
        }
    }

    fn finalize(&mut self, outcome: McpFinishOutcome) -> Result<(), BridgeError> {
        let deadline = Instant::now() + FINALIZATION_GRACE;
        self.write_receipt(outcome, deadline)?;
        self.stdin.take();
        loop {
            if let Some(status) = self.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(BridgeError::McpExchangeFailed {
                        exit: classify_exit(status),
                    })
                };
            }
            if Instant::now() >= deadline {
                return Err(self.terminated(if self.cancellation.is_cancelled() {
                    TerminationReason::Cancelled
                } else {
                    TerminationReason::WallTime
                }));
            }
            self.sleep_until(deadline);
        }
    }

    fn poll_request_write(&mut self) -> Result<(), BridgeError> {
        #[cfg(unix)]
        {
            if self.request_complete {
                return Ok(());
            }
            let stdin = self.stdin.as_mut().ok_or(BridgeError::WorkerFailed)?;
            match stdin.write(&self.request_frame[self.request_offset..]) {
                Ok(0) => return Err(write_zero("MCP request pipe closed")),
                Ok(written) => self.request_offset += written,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(BridgeError::Transport(error)),
            }
            if self.request_offset == self.request_frame.len() {
                stdin.flush().map_err(BridgeError::Transport)?;
                self.request_complete = true;
            }
        }
        #[cfg(windows)]
        {
            let Some(writer) = self.pipe_writer.as_mut() else {
                return Ok(());
            };
            let Some(result) = writer.poll() else {
                return Ok(());
            };
            self.stdin = Some(result.map_err(BridgeError::Transport)?);
            self.pipe_writer = None;
            self.request_complete = true;
        }
        Ok(())
    }

    fn write_receipt(
        &mut self,
        outcome: McpFinishOutcome,
        deadline: Instant,
    ) -> Result<(), BridgeError> {
        let frame = outcome.receipt_frame();
        #[cfg(windows)]
        {
            let stdin = self.stdin.take().ok_or(BridgeError::WorkerFailed)?;
            self.pipe_writer =
                Some(platform::PipeWriter::spawn(stdin, frame).map_err(BridgeError::Transport)?);
            loop {
                if let Some(result) = self
                    .pipe_writer
                    .as_mut()
                    .and_then(platform::PipeWriter::poll)
                {
                    self.stdin = Some(result.map_err(BridgeError::Transport)?);
                    self.pipe_writer = None;
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(BridgeError::Transport(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "MCP receipt write timed out",
                    )));
                }
                self.sleep_until(deadline);
            }
        }
        #[cfg(unix)]
        {
            let mut offset = 0;
            while offset < frame.len() {
                if Instant::now() >= deadline {
                    return Err(BridgeError::Transport(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "MCP receipt write timed out",
                    )));
                }
                let stdin = self.stdin.as_mut().ok_or(BridgeError::WorkerFailed)?;
                match stdin.write(&frame[offset..]) {
                    Ok(0) => return Err(write_zero("MCP receipt pipe closed")),
                    Ok(written) => offset += written,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        self.sleep_until(deadline)
                    }
                    Err(error) => return Err(BridgeError::Transport(error)),
                }
            }
        }
        loop {
            if Instant::now() >= deadline {
                return Err(BridgeError::Transport(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MCP receipt flush timed out",
                )));
            }
            match self
                .stdin
                .as_mut()
                .ok_or(BridgeError::WorkerFailed)?
                .flush()
            {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.sleep_until(deadline)
                }
                Err(error) => return Err(BridgeError::Transport(error)),
            }
        }
    }

    fn drain_pipes(&mut self, allow_stdout_close: bool) -> Result<(), BridgeError> {
        let mut buffer = [0_u8; 8 * 1024];
        while let Some(stdout) = self.stdout.as_mut() {
            match platform::read_pipe(stdout, &mut buffer).map_err(BridgeError::Transport)? {
                platform::PipeRead::Data(read) => self.consume_stdout(&buffer[..read])?,
                platform::PipeRead::Pending => break,
                platform::PipeRead::Closed => {
                    self.stdout.take();
                    if !allow_stdout_close || !self.frame_seen {
                        return Err(BridgeError::InvalidProtocolResponse("MCP response frame"));
                    }
                    break;
                }
            }
        }
        if let Some(stderr) = self.stderr.as_mut() {
            match platform::read_pipe(stderr, &mut buffer).map_err(BridgeError::Transport)? {
                platform::PipeRead::Data(_) => {
                    return Err(BridgeError::InvalidProtocolResponse("MCP stderr"))
                }
                platform::PipeRead::Pending => {}
                platform::PipeRead::Closed => {
                    self.stderr.take();
                }
            }
        }
        Ok(())
    }

    fn consume_stdout(&mut self, bytes: &[u8]) -> Result<(), BridgeError> {
        if self.frame_seen {
            return Err(BridgeError::InvalidProtocolResponse("MCP response frame"));
        }
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            if self.response_frame.len().saturating_add(newline + 1) > self.stdout_limit {
                return Err(BridgeError::Limit("stdout bytes"));
            }
            self.response_frame.extend_from_slice(&bytes[..=newline]);
            self.frame_seen = true;
            if newline + 1 != bytes.len() {
                return Err(BridgeError::InvalidProtocolResponse("MCP response frame"));
            }
        } else {
            if self.response_frame.len().saturating_add(bytes.len()) >= self.stdout_limit {
                return Err(BridgeError::Limit("stdout bytes"));
            }
            self.response_frame.extend_from_slice(bytes);
        }
        Ok(())
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, BridgeError> {
        self.child.try_wait().map_err(BridgeError::Transport)
    }

    fn terminated(&self, reason: TerminationReason) -> BridgeError {
        BridgeError::McpExchangeFailed {
            exit: ExitClass::Terminated(reason),
        }
    }

    fn sleep_until(&self, deadline: Instant) {
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }

    fn terminate_and_reap(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        self.tree.terminate();
        self.stdin.take();
        self.stdout.take();
        self.stderr.take();
        #[cfg(windows)]
        if let Some(writer) = self.pipe_writer.take() {
            let _ = writer.cancel_and_join();
        }
        let _ = self.child.wait();
    }
}

impl Drop for LifecycleOwner {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

fn write_zero(message: &'static str) -> BridgeError {
    BridgeError::Transport(io::Error::new(io::ErrorKind::WriteZero, message))
}

fn take_pipe<T>(pipe: Option<T>, message: &'static str) -> Result<T, BridgeError> {
    pipe.ok_or_else(|| BridgeError::Transport(io::Error::new(io::ErrorKind::BrokenPipe, message)))
}
