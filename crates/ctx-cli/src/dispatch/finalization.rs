use std::{
    io::{self, Write},
    time::Duration,
};

use crate::local_usage;

#[derive(Debug, thiserror::Error)]
pub(super) enum FinalOutputFlushError {
    #[error("flush CLI stdout: {0}")]
    Stdout(io::Error),
    #[error("flush CLI stderr: {0}")]
    Stderr(io::Error),
    #[error("flush CLI stdout: {stdout}; flush CLI stderr: {stderr}")]
    Both {
        stdout: io::Error,
        stderr: io::Error,
    },
}

pub(super) fn flush_cli_output_then<T>(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    after_delivery: impl FnOnce() -> T,
) -> std::result::Result<T, FinalOutputFlushError> {
    let stdout_result = stdout.flush();
    let stderr_result = stderr.flush();
    match (stdout_result, stderr_result) {
        (Ok(()), Ok(())) => Ok(after_delivery()),
        (Err(stdout), Ok(())) => Err(FinalOutputFlushError::Stdout(stdout)),
        (Ok(()), Err(stderr)) => Err(FinalOutputFlushError::Stderr(stderr)),
        (Err(stdout), Err(stderr)) => Err(FinalOutputFlushError::Both { stdout, stderr }),
    }
}

pub(super) fn complete_local_usage(
    mut draft: local_usage::CliUsage,
    success: bool,
    duration: Duration,
    delivered_output_bytes: u64,
) -> Option<local_usage::CompletedOperation> {
    // Runtime accounting is authoritative over command-local canonical
    // estimates: this is the final adapted stdout + stderr byte count after
    // error rendering and successful delivery flushes.
    let output_bytes = usize::try_from(delivered_output_bytes).unwrap_or(usize::MAX);
    draft.set_measured_output_bytes(output_bytes);
    draft.completed(success, duration)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        io::{self, Write},
        rc::Rc,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use clap::Parser;

    use super::{complete_local_usage, flush_cli_output_then};
    use crate::{
        cli::Cli,
        dispatch::{command_local_usage_draft, render_generic_command_error},
        output::OutputMeasurement,
        ui::{ColorMode, RenderContext, StreamKind, TestContext, Ui},
    };

    #[derive(Clone, Default)]
    struct SharedBytes(Arc<Mutex<Vec<u8>>>);

    impl SharedBytes {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().map(|bytes| bytes.clone()).unwrap_or_default()
        }
    }

    impl Write for SharedBytes {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("shared test writer was poisoned"))?
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn final_accounting_replaces_estimates_with_both_delivered_streams() {
        for success in [true, false] {
            let measurement = OutputMeasurement::start();
            let stdout = SharedBytes::default();
            let stdout_copy = stdout.clone();
            let stderr = SharedBytes::default();
            let stderr_copy = stderr.clone();
            let mut ui = Ui::with_writers(
                stdout,
                RenderContext::for_test(
                    TestContext::tty(StreamKind::Stdout, 32).color(ColorMode::Always),
                ),
                stderr,
                RenderContext::for_test(
                    TestContext::tty(StreamKind::Stderr, 48).color(ColorMode::Always),
                ),
            );
            let document = crate::ui::Document::from_line(crate::ui::Line::text(
                "stdout result with enough words to wrap",
            ));
            ui.write_stdout(&document).unwrap();
            if success {
                let document =
                    crate::ui::Document::from_line(crate::ui::Line::text("stderr delivery note"));
                ui.write_stderr(&document).unwrap();
            } else {
                render_generic_command_error(
                    &anyhow::anyhow!("final command failure"),
                    false,
                    &mut ui,
                )
                .unwrap();
            }
            ui.flush().unwrap();

            let cli = Cli::try_parse_from(["ctx", "docs", "list"]).unwrap();
            let mut draft = command_local_usage_draft(&cli.command);
            draft.set_measured_output_bytes(1);
            let delivered = measurement.total_bytes();
            let completed =
                complete_local_usage(draft, success, Duration::from_millis(25), delivered).unwrap();

            let expected = stdout_copy.bytes().len() + stderr_copy.bytes().len();
            assert_eq!(usize::try_from(delivered).unwrap(), expected);
            assert_eq!(
                completed.delivered_output_bytes_for_test(),
                u64::try_from(expected).unwrap()
            );
            assert_eq!(completed.duration_bucket_for_test(), "10_to_49_ms");
        }
    }

    struct FlushWriter {
        failure: Option<&'static str>,
        flushes: Rc<Cell<usize>>,
    }

    impl Write for FlushWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes.set(self.flushes.get() + 1);
            match self.failure {
                Some(message) => Err(io::Error::new(io::ErrorKind::BrokenPipe, message)),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn local_usage_hook_runs_only_after_both_final_output_flushes_succeed() {
        for (stdout_failure, stderr_failure, expected_delivery, expected_error) in [
            (None, None, 1, None),
            (Some("stdout"), None, 0, Some("flush CLI stdout: stdout")),
            (None, Some("stderr"), 0, Some("flush CLI stderr: stderr")),
            (
                Some("stdout"),
                Some("stderr"),
                0,
                Some("flush CLI stdout: stdout; flush CLI stderr: stderr"),
            ),
        ] {
            let stdout_flushes = Rc::new(Cell::new(0));
            let stderr_flushes = Rc::new(Cell::new(0));
            let mut stdout = FlushWriter {
                failure: stdout_failure,
                flushes: stdout_flushes.clone(),
            };
            let mut stderr = FlushWriter {
                failure: stderr_failure,
                flushes: stderr_flushes.clone(),
            };
            let mut deliveries = 0;

            let result = flush_cli_output_then(&mut stdout, &mut stderr, || {
                deliveries += 1;
                (stdout_flushes.get(), stderr_flushes.get())
            });

            assert_eq!(deliveries, expected_delivery);
            assert_eq!(stdout_flushes.get(), 1);
            assert_eq!(stderr_flushes.get(), 1);
            match expected_error {
                Some(expected) => {
                    assert!(result.is_err());
                    assert!(result.unwrap_err().to_string().contains(expected));
                }
                None => assert_eq!(result.unwrap(), (1, 1)),
            }
        }
    }

    #[test]
    fn duration_is_closed_after_both_final_stream_flushes() {
        struct TimedFlushWriter {
            clock_ms: Rc<Cell<u64>>,
            finish_at_ms: u64,
        }

        impl Write for TimedFlushWriter {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                Ok(buffer.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.clock_ms.set(self.finish_at_ms);
                Ok(())
            }
        }

        let clock_ms = Rc::new(Cell::new(0));
        let mut stdout = TimedFlushWriter {
            clock_ms: clock_ms.clone(),
            finish_at_ms: 11,
        };
        let mut stderr = TimedFlushWriter {
            clock_ms: clock_ms.clone(),
            finish_at_ms: 57,
        };
        let duration = flush_cli_output_then(&mut stdout, &mut stderr, || {
            Duration::from_millis(clock_ms.get())
        })
        .unwrap();

        assert_eq!(duration, Duration::from_millis(57));
        let cli = Cli::try_parse_from(["ctx", "doctor"]).unwrap();
        let completed =
            complete_local_usage(command_local_usage_draft(&cli.command), true, duration, 0)
                .unwrap();
        assert_eq!(completed.duration_bucket_for_test(), "50_to_249_ms");
    }
}
