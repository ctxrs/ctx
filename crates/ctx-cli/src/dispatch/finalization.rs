use std::{
    io::{self, Write},
    time::Duration,
};

use crate::{
    analytics::{SearchFailurePhase, SearchHealthFacts, SearchTelemetry},
    local_usage,
};

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

pub(super) fn flush_cli_output(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::result::Result<(), FinalOutputFlushError> {
    let stdout_result = stdout.flush();
    let stderr_result = stderr.flush();
    match (stdout_result, stderr_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(stdout), Ok(())) => Err(FinalOutputFlushError::Stdout(stdout)),
        (Ok(()), Err(stderr)) => Err(FinalOutputFlushError::Stderr(stderr)),
        (Err(stdout), Err(stderr)) => Err(FinalOutputFlushError::Both { stdout, stderr }),
    }
}

pub(super) fn record_search_final_delivery(
    telemetry: &mut SearchTelemetry,
    boundary_succeeded: bool,
    output_duration: Duration,
) -> bool {
    let command_output_succeeded = telemetry
        .health
        .as_ref()
        .and_then(|health| health.failure_phase)
        != Some(SearchFailurePhase::Output);
    let served = boundary_succeeded && command_output_succeeded;
    telemetry.output_duration = Some(
        telemetry
            .output_duration
            .unwrap_or_default()
            .saturating_add(output_duration),
    );
    telemetry.output_served = Some(served);
    if !served {
        telemetry
            .health
            .get_or_insert_with(SearchHealthFacts::default)
            .failure_phase = Some(SearchFailurePhase::Output);
    }
    served
}

pub(super) fn send_online_after_output(
    output_result: anyhow::Result<()>,
    send: impl FnOnce(),
) -> anyhow::Result<()> {
    send();
    output_result
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
        sync::{Arc, Barrier, Mutex},
        thread,
        time::Duration,
    };

    use clap::Parser;

    use super::{
        complete_local_usage, flush_cli_output, record_search_final_delivery,
        send_online_after_output,
    };
    use crate::{
        analytics::{
            ClientOperationDraft, Outcome, PublicEventV1, SearchFailurePhase, SearchHealthFacts,
            SearchTelemetry,
        },
        cli::Cli,
        dispatch::{
            command_local_usage_draft, command_operation_descriptor, render_generic_command_error,
            write_machine_error,
        },
        operation_descriptor::{CliOperation, OperationDescriptor},
        output::{MeasuredWriter, OutputMeasurement},
        ui::{ColorMode, RenderContext, StreamKind, TestContext, Ui},
    };

    fn search_telemetry() -> SearchTelemetry {
        let cli = Cli::try_parse_from(["ctx", "search", "needle"]).unwrap();
        let OperationDescriptor::Cli(CliOperation::Search(telemetry)) =
            command_operation_descriptor(&cli.command)
        else {
            panic!("search command must have Search telemetry")
        };
        telemetry
    }

    #[test]
    fn search_final_delivery_records_served_success_and_duration() {
        let mut telemetry = search_telemetry();

        let served = record_search_final_delivery(&mut telemetry, true, Duration::ZERO);

        assert!(served);
        assert_eq!(telemetry.output_served, Some(true));
        assert_eq!(telemetry.output_duration, Some(Duration::ZERO));
        assert_eq!(telemetry.health, None);
    }

    #[test]
    fn online_delivery_precedes_output_failure_return_and_is_unchanged_on_success() {
        let sends = Cell::new(0);
        let failure = send_online_after_output(Err(anyhow::anyhow!("output failed")), || {
            sends.set(sends.get() + 1);
        });
        assert!(failure.is_err());
        assert_eq!(sends.get(), 1);

        send_online_after_output(Ok(()), || sends.set(sends.get() + 1)).unwrap();
        assert_eq!(sends.get(), 2);
    }

    #[test]
    fn search_final_delivery_preserves_served_command_error_phase() {
        let mut telemetry = search_telemetry();
        telemetry.health = Some(SearchHealthFacts {
            failure_phase: Some(SearchFailurePhase::Render),
            ..SearchHealthFacts::default()
        });

        let served = record_search_final_delivery(&mut telemetry, true, Duration::from_millis(2));

        assert!(served);
        assert_eq!(telemetry.output_served, Some(true));
        assert_eq!(
            telemetry.health.unwrap().failure_phase,
            Some(SearchFailurePhase::Render)
        );
    }

    #[test]
    fn search_command_ui_failure_remains_unserved_after_final_flush() {
        let mut telemetry = search_telemetry();
        telemetry.health = Some(SearchHealthFacts {
            failure_phase: Some(SearchFailurePhase::Output),
            ..SearchHealthFacts::default()
        });

        let served = record_search_final_delivery(&mut telemetry, true, Duration::from_millis(3));

        assert!(!served);
        assert_eq!(telemetry.output_served, Some(false));
        assert_eq!(
            telemetry.health.unwrap().failure_phase,
            Some(SearchFailurePhase::Output)
        );
    }

    #[test]
    fn search_owned_output_failure_with_successful_error_delivery_is_sent() {
        let mut telemetry = search_telemetry();
        telemetry.health = Some(SearchHealthFacts {
            failure_phase: Some(SearchFailurePhase::Output),
            ..SearchHealthFacts::default()
        });

        // A command-body result write or not-ready diagnostic write failed,
        // but its generic error and both final stream flushes subsequently
        // succeeded at the terminal boundary.
        let served = record_search_final_delivery(&mut telemetry, true, Duration::from_millis(3));
        let sends = Cell::new(0);
        send_online_after_output(Ok(()), || sends.set(sends.get() + 1)).unwrap();

        assert!(!served);
        assert_eq!(telemetry.output_served, Some(false));
        assert_eq!(
            telemetry.health.unwrap().failure_phase,
            Some(SearchFailurePhase::Output)
        );
        assert_eq!(sends.get(), 1);
    }

    #[test]
    fn search_output_failure_sends_terminal_receipt_before_returning_original_error() {
        let cli = Cli::try_parse_from(["ctx", "search", "needle"]).unwrap();
        let mut draft = ClientOperationDraft::from_descriptor(
            command_operation_descriptor(&cli.command),
            false,
        )
        .unwrap();
        let served =
            record_search_final_delivery(draft.search_mut(), false, Duration::from_millis(4));
        assert!(!served);
        let event = draft.finish(false, Duration::from_millis(9));
        let sent = std::cell::RefCell::new(None);

        let error = send_online_after_output(
            Err(anyhow::anyhow!("flush CLI stdout: broken pipe")),
            || {
                sent.replace(Some(event));
            },
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "flush CLI stdout: broken pipe");
        let PublicEventV1::OperationCompleted(event) = sent.into_inner().unwrap() else {
            panic!("Search must emit an operation terminal event")
        };
        assert_eq!(event.outcome, Outcome::Failure);
        let OperationDescriptor::Cli(CliOperation::Search(telemetry)) = event.descriptor else {
            panic!("terminal event must retain Search telemetry")
        };
        assert_eq!(telemetry.output_served, Some(false));
        assert_eq!(telemetry.output_duration, Some(Duration::from_millis(4)));
        assert_eq!(
            telemetry.health.unwrap().failure_phase,
            Some(SearchFailurePhase::Output)
        );
    }

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

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "measured Search error write failed",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn search_machine_error_uses_the_ui_writer_and_propagates_failure() {
        let stderr = SharedBytes::default();
        let stderr_copy = stderr.clone();
        let mut ui = Ui::with_writers(
            io::sink(),
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            stderr,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
        );

        write_machine_error(true, &mut ui, "structured search error").unwrap();
        assert_eq!(stderr_copy.bytes(), b"structured search error\n");

        let mut failing_ui = Ui::with_writers(
            io::sink(),
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            FailingWriter,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
        );
        let error = write_machine_error(true, &mut failing_ui, "failure").unwrap_err();
        assert_eq!(error.to_string(), "measured Search error write failed");
    }

    fn render_and_assert_final_accounting_fixture(
        success: bool,
        measurement: &OutputMeasurement,
    ) -> usize {
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
            render_generic_command_error(&anyhow::anyhow!("final command failure"), false, &mut ui)
                .unwrap();
        }
        ui.flush().unwrap();

        let cli = Cli::try_parse_from(["ctx", "docs", "list"]).unwrap();
        let mut draft = command_local_usage_draft(&cli.command);
        draft.set_measured_output_bytes(1);
        let delivered = measurement.total_bytes();
        let completed =
            complete_local_usage(draft, success, Duration::from_millis(25), delivered).unwrap();

        let expected_stdout = stdout_copy.bytes().len();
        let expected_stderr = stderr_copy.bytes().len();
        let expected = expected_stdout + expected_stderr;
        assert_eq!(
            measurement.stream_bytes(StreamKind::Stdout),
            u64::try_from(expected_stdout).unwrap()
        );
        assert_eq!(
            measurement.stream_bytes(StreamKind::Stderr),
            u64::try_from(expected_stderr).unwrap()
        );
        assert_eq!(usize::try_from(delivered).unwrap(), expected);
        assert_eq!(
            completed.delivered_output_bytes_for_test(),
            u64::try_from(expected).unwrap()
        );
        assert_eq!(completed.duration_bucket_for_test(), "10_to_49_ms");
        expected
    }

    #[test]
    fn final_accounting_replaces_estimates_with_both_delivered_streams() {
        for success in [true, false] {
            let measurement = OutputMeasurement::start_for_current_thread();
            render_and_assert_final_accounting_fixture(success, &measurement);
        }
    }

    #[test]
    fn final_accounting_isolated_scope_excludes_foreign_bytes_and_restores_its_parent() {
        const FOREIGN_BYTES: usize = 1_120;
        const RESTORED_BYTES: &[u8] = b"restored parent output";

        // ctx-terminal is a normal dependency of this test binary, so this
        // exercises the non-test implementation of its measurement state.
        let parent_measurement = OutputMeasurement::start_for_current_thread();
        let foreign_bytes = SharedBytes::default();
        let foreign_bytes_copy = foreign_bytes.clone();
        let ready = Arc::new(Barrier::new(2));
        let finished = Arc::new(Barrier::new(2));
        let foreign_writer = {
            let ready = ready.clone();
            let finished = finished.clone();
            thread::spawn(move || {
                ready.wait();
                let mut writer = MeasuredWriter::current(foreign_bytes, StreamKind::Stdout);
                writer.write_all(&[b'x'; FOREIGN_BYTES]).unwrap();
                finished.wait();
            })
        };

        let nested_measurement = OutputMeasurement::start_for_current_thread();
        ready.wait();
        let expected = render_and_assert_final_accounting_fixture(true, &nested_measurement);
        finished.wait();
        foreign_writer.join().unwrap();

        assert_eq!(expected, 61, "the focused two-stream fixture changed");
        assert_eq!(nested_measurement.total_bytes(), 61);
        assert_eq!(parent_measurement.total_bytes(), 0);
        assert_eq!(
            foreign_bytes_copy.bytes().len(),
            FOREIGN_BYTES,
            "the barrier-controlled foreign writer must deliver every byte"
        );

        drop(nested_measurement);
        let mut restored_writer = MeasuredWriter::current(io::sink(), StreamKind::Stdout);
        restored_writer.write_all(RESTORED_BYTES).unwrap();
        assert_eq!(
            parent_measurement.total_bytes(),
            u64::try_from(RESTORED_BYTES.len()).unwrap(),
            "dropping the nested scope must restore its thread-local parent"
        );
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
    fn search_final_stdout_and_stderr_flush_failures_are_unserved() {
        for (stdout_failure, stderr_failure) in [
            (Some("stdout"), None),
            (None, Some("stderr")),
            (Some("stdout"), Some("stderr")),
        ] {
            let mut stdout = FlushWriter {
                failure: stdout_failure,
                flushes: Rc::new(Cell::new(0)),
            };
            let mut stderr = FlushWriter {
                failure: stderr_failure,
                flushes: Rc::new(Cell::new(0)),
            };
            let delivered = flush_cli_output(&mut stdout, &mut stderr).is_ok();
            let mut telemetry = search_telemetry();

            let served =
                record_search_final_delivery(&mut telemetry, delivered, Duration::from_millis(4));

            assert!(!served);
            assert_eq!(telemetry.output_served, Some(false));
            assert_eq!(telemetry.output_duration, Some(Duration::from_millis(4)));
            assert_eq!(
                telemetry.health.unwrap().failure_phase,
                Some(SearchFailurePhase::Output)
            );
        }
    }

    #[test]
    fn local_usage_gate_opens_only_after_both_final_output_flushes_succeed() {
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

            let result = flush_cli_output(&mut stdout, &mut stderr);
            let delivered_at = result.as_ref().ok().map(|()| {
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
                None => {
                    result.unwrap();
                    assert_eq!(delivered_at, Some((1, 1)));
                }
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
        flush_cli_output(&mut stdout, &mut stderr).unwrap();
        let duration = Duration::from_millis(clock_ms.get());

        assert_eq!(duration, Duration::from_millis(57));
        let cli = Cli::try_parse_from(["ctx", "doctor"]).unwrap();
        let completed =
            complete_local_usage(command_local_usage_draft(&cli.command), true, duration, 0)
                .unwrap();
        assert_eq!(completed.duration_bucket_for_test(), "50_to_249_ms");
    }
}
