// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Command output adapters backed by the pinned Sift compaction library.
//!
//! Arguments exclude argv0. This entry point owns output/status only: it never
//! initializes history or exits the process. Run one invocation at a time;
//! runner signal handlers and process standard streams are invocation-scoped.
mod cli;
mod command_view;
mod discover;
mod execute;
mod filter;
mod hooks;
mod input;
mod jev;
pub mod observation;
mod observe;
mod pre_hooks;
mod protocol;
mod rewrite;
mod runner;
mod semantic;
mod state;
mod usage;
mod views;

use std::{
    ffi::OsString,
    io::{self, Write},
};

/// Execute a Sift-style output subcommand, returning its numeric process status.
/// Help returns zero, usage/I/O errors one, spawn errors 126/127, and Unix
/// cancellation 128 + signal. A closed output consumer is a quiet success.
/// Child argv and input file paths retain their native OS representation.
pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    run_observed(args, |_| {})
}

/// Observe content-free terminal facts after output completes or fails.
/// Protocols emit a terminal for each request and a separate session terminal;
/// session terminals carry no stream savings. Callbacks must remain lightweight
/// and must not change output or start network work on the calling thread.
pub fn run_observed(
    args: impl IntoIterator<Item = OsString>,
    mut callback: impl FnMut(observation::SiftObservation),
) -> i32 {
    let mut observed = observe::Observed::new();
    let result = cli::run(args, &mut observed, &mut callback);
    let status = match &result {
        Ok(status) => *status,
        Err(error) if is_broken_pipe(error) => 0,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "ctx sift: {error:#}");
            1
        }
    };
    observed.finish(result.as_ref().err(), &mut callback);
    status
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            || cause
                .downcast_ref::<serde_json::Error>()
                .is_some_and(|error| error.io_error_kind() == Some(io::ErrorKind::BrokenPipe))
    })
}

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod commands_tests;
#[cfg(test)]
mod discover_tests;
#[cfg(test)]
mod filter_tests;
#[cfg(test)]
mod hooks_tests;
#[cfg(test)]
mod pi_session_tests;
#[cfg(test)]
mod pre_hooks_tests;
#[cfg(test)]
mod rewrite_tests;
#[cfg(test)]
mod runner_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod usage_tests;
#[cfg(test)]
mod views_tests;
#[cfg(test)]
mod workflow_tests;

#[cfg(test)]
mod observation_tests;
