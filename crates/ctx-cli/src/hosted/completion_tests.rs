use super::*;
use clap::Parser;
use ctx_client_observability::analytics::{HostedFailureTypeV1, HostedFailureV1};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    None,
    Write,
    Flush,
}

#[derive(Debug, PartialEq, Eq)]
enum Attempt {
    Write(bool),
    Flush(bool),
    Completion,
}

#[derive(Default)]
struct State {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    attempts: Vec<Attempt>,
    completions: Vec<HostedCompletion>,
}

struct Writer {
    state: Arc<Mutex<State>>,
    stderr: bool,
    failure: Failure,
}

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().unwrap();
        state.attempts.push(Attempt::Write(self.stderr));
        if self.stderr && self.failure == Failure::Write {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic write failure",
            ));
        }
        if self.stderr {
            state.stderr.extend_from_slice(bytes);
        } else {
            state.stdout.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state
            .lock()
            .unwrap()
            .attempts
            .push(Attempt::Flush(self.stderr));
        if self.stderr && self.failure == Failure::Flush {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic flush failure",
            ));
        }
        Ok(())
    }
}

fn run(arguments: &[&str], failure: Failure) -> (Result<()>, State) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("uninitialized");
    let cli = crate::Cli::try_parse_from(arguments).unwrap();
    let crate::cli::CommandRoot::Hosted(command) = cli.command else {
        panic!("hosted command expected")
    };
    let state = Arc::new(Mutex::new(State::default()));
    let mut ui = Ui::with_writers(
        Writer {
            state: state.clone(),
            stderr: false,
            failure: Failure::None,
        },
        crate::ui::RenderContext::canonical_human_measurement(),
        Writer {
            state: state.clone(),
            stderr: true,
            failure,
        },
        crate::ui::RenderContext::canonical_human_measurement(),
    );
    let observed = state.clone();
    let observers = HostedObservers {
        completion: Some(Arc::new(move |completion| {
            let mut state = observed.lock().unwrap();
            state.attempts.push(Attempt::Completion);
            state.completions.push(completion);
        })),
        ..Default::default()
    };
    let result = run_with_ui(&command, Some(&data), &mut ui, &observers);
    assert!(
        !data.exists(),
        "no server, credentials or telemetry identity initialized"
    );
    let state = std::mem::take(&mut *state.lock().unwrap());
    assert_eq!(state.completions.len(), 1);
    assert_eq!(state.attempts.last(), Some(&Attempt::Completion));
    (result, state)
}

#[test]
fn healthy_json_operation_error_completes_after_envelope_flush_and_keeps_original_failure() {
    let (result, state) = run(
        &["ctx", "server", "status", "--format", "json"],
        Failure::None,
    );
    assert!(result
        .unwrap_err()
        .is::<crate::dispatch::RenderedCliError>());
    assert_eq!(
        state.completions[0].operation,
        HostedOperation::ServerStatus
    );
    assert_eq!(
        state.completions[0].result,
        Err(HostedFailureV1::Operation(HostedFailureTypeV1::Other))
    );
    assert!(state.stdout.is_empty());
    let envelope: Value = serde_json::from_slice(&state.stderr).unwrap();
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["error"]["code"], "hosted_error");
    assert!(envelope["error"]["message"]
        .as_str()
        .unwrap()
        .contains("server root is not initialized"));
    assert!(state.attempts.contains(&Attempt::Write(true)));
    assert_eq!(
        state.attempts[state.attempts.len() - 2],
        Attempt::Flush(true)
    );
}

#[test]
fn json_error_write_failure_is_the_single_terminal_output_failure() {
    let (result, state) = run(
        &["ctx", "server", "status", "--format", "json"],
        Failure::Write,
    );
    let error = result.unwrap_err();
    assert!(error.is::<telemetry::OutputFailure>());
    assert!(!error.is::<crate::dispatch::RenderedCliError>());
    assert_eq!(state.completions[0].result, Err(HostedFailureV1::Output));
    assert!(state.stderr.is_empty());
    assert_eq!(state.attempts, [Attempt::Write(true), Attempt::Completion]);
}

#[test]
fn json_error_flush_failure_is_the_single_terminal_output_failure() {
    let (result, state) = run(
        &["ctx", "server", "status", "--format", "json"],
        Failure::Flush,
    );
    let error = result.unwrap_err();
    assert!(error.is::<telemetry::OutputFailure>());
    assert!(!error.is::<crate::dispatch::RenderedCliError>());
    assert_eq!(state.completions[0].result, Err(HostedFailureV1::Output));
    assert!(serde_json::from_slice::<Value>(&state.stderr).is_ok());
    assert_eq!(
        state.attempts[state.attempts.len() - 2],
        Attempt::Flush(true)
    );
}

#[test]
fn ordinary_success_completes_after_normal_output_flush() {
    let (result, state) = run(
        &["ctx", "remote", "status", "synthetic", "--format", "json"],
        Failure::None,
    );
    result.unwrap();
    assert_eq!(
        state.completions[0].operation,
        HostedOperation::RemoteStatus
    );
    assert_eq!(state.completions[0].result, Ok(()));
    assert!(state.stderr.is_empty());
    let output: Value = serde_json::from_slice(&state.stdout).unwrap();
    assert_eq!(output["operation"], "remote_status");
    assert_eq!(output["local"]["connected"], false);
    assert!(state.attempts.contains(&Attempt::Flush(false)));
    assert_eq!(
        state.attempts[state.attempts.len() - 2],
        Attempt::Flush(true)
    );
}

#[test]
fn human_operation_error_preserves_original_error_for_outer_presentation() {
    let (result, state) = run(&["ctx", "server", "status"], Failure::None);
    let error = result.unwrap_err();
    assert!(!error.is::<crate::dispatch::RenderedCliError>());
    assert!(error.to_string().contains("server root is not initialized"));
    assert_eq!(
        state.completions[0].result,
        Err(HostedFailureV1::Operation(HostedFailureTypeV1::Other))
    );
    assert!(state.stdout.is_empty() && state.stderr.is_empty());
    assert_eq!(state.attempts, [Attempt::Completion]);
}
