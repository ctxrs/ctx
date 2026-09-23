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
mod jev;
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
    match cli::run(args) {
        Ok(status) => status,
        Err(error) if is_broken_pipe(&error) => 0,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "ctx output: {error:#}");
            1
        }
    }
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
