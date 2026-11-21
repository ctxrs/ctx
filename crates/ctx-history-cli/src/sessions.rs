mod render;

use std::path::PathBuf;

use anyhow::Result;
use ctx_history_index::IndexError;
use ctx_history_read_application::{
    execute_sessions, GenerationReadError, GenerationReadTarget, SessionsApplicationError,
    SessionsApplicationRequest,
};

use crate::{
    analytics::{count_bucket, ShowTelemetry},
    cli::SessionsArgs,
    local_usage::{CliUsage, ResultObservationAction},
    output::print_json,
    request::SessionsRequest,
    source_index::{
        externalize_query_error, open_generation_read, pretty_json_stdout_bytes,
        render_active_generation_race, ActiveGenerationRaceCommand,
    },
    ui::{canonical_human_output_bytes, Ui},
};

pub use ctx_history_read_application::{DEFAULT_SESSIONS_LIMIT, MAX_SESSIONS_LIMIT};

pub fn run_sessions(
    args: SessionsArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.format.is_json();
    let request = ctx_history_read_application::SessionsRequest::from(SessionsRequest::from(args));
    let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
        open_generation_read(&data_root, read)
    };
    let result = match execute_sessions(
        SessionsApplicationRequest {
            request,
            generation_target: GenerationReadTarget::Active,
            compact_projection: !json_output,
        },
        &mut generation,
    ) {
        Ok(result) => result,
        Err(error) => return handle_sessions_error(error, json_output, ui),
    };

    let session_count = result.page().sessions.len();
    let (value, compact_value) = result.into_read_models();
    let content_bytes = serde_json::to_vec(&value)?.len();
    let render_value = compact_value.as_ref().unwrap_or(&value);
    let output_bytes = if json_output {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else {
        let document = render::render_sessions_document(render_value, ui.stdout_context());
        let output_bytes = canonical_human_output_bytes(|context| {
            render::render_sessions_document(render_value, context)
        });
        ui.write_stdout(&document)?;
        output_bytes
    };

    telemetry.events_returned = Some(count_bucket(session_count as u64));
    local_usage.set_result_observation(
        ResultObservationAction::OpenSession,
        session_count,
        content_bytes,
    );
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

fn handle_sessions_error(
    error: SessionsApplicationError<anyhow::Error>,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    match error {
        SessionsApplicationError::Generation(GenerationReadError::Port(error)) => {
            let error = match render_active_generation_race(
                Err::<(), _>(error),
                json_output,
                ActiveGenerationRaceCommand::Sessions,
                ui,
            ) {
                Ok(()) => unreachable!("active generation race helper received an error"),
                Err(error) => error,
            };
            if error.is::<crate::RenderedCliError>() {
                return Err(error);
            }
            if !json_output && is_missing_active_generation(&error) {
                let document = render::render_sessions_not_ready_document(ui.stderr_context());
                ui.write_stderr(&document)?;
                return Err(crate::dispatch::rendered_cli_error());
            }
            Err(error)
        }
        SessionsApplicationError::Generation(GenerationReadError::Authority(error)) => {
            Err(anyhow::Error::new(error))
        }
        SessionsApplicationError::Query(error) => Err(externalize_sessions_query_error(error)),
        SessionsApplicationError::Projection(error) => render_active_generation_race(
            Err::<(), _>(error),
            json_output,
            ActiveGenerationRaceCommand::Sessions,
            ui,
        ),
    }
}

fn is_missing_active_generation(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<IndexError>(),
            Some(IndexError::MissingActiveGenerationPointer)
        )
    })
}

fn externalize_sessions_query_error(error: anyhow::Error) -> anyhow::Error {
    let text = error.to_string();
    if text.contains("invalid since") {
        return anyhow::anyhow!(text.replacen("invalid since", "invalid --since", 1));
    }
    externalize_query_error(error)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::analytics::{RenderFormat, TargetKind};
    use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_ui() -> (Ui, SharedWriter) {
        let stderr = SharedWriter::default();
        let captured = stderr.clone();
        let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
        let stderr_context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stderr, 100).color(ColorMode::Never),
        );
        (
            Ui::with_writers(io::sink(), stdout_context, stderr, stderr_context),
            captured,
        )
    }

    fn show_telemetry() -> ShowTelemetry {
        ShowTelemetry {
            target_kind: TargetKind::Session,
            transcript_mode: None,
            output_format: RenderFormat::Text,
            writes_out_file: false,
            provider_lookup: false,
            window: None,
            events_returned: None,
        }
    }

    #[test]
    fn missing_index_renders_not_ready_diagnostic_in_text_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut telemetry = show_telemetry();
        let mut local_usage = CliUsage::excluded();
        let (mut ui, captured_stderr) = test_ui();

        let err = run_sessions(
            SessionsArgs {
                since: None,
                provider: None,
                workspace: None,
                no_subagents: false,
                limit: DEFAULT_SESSIONS_LIMIT,
                format: crate::JsonOutputFormat::Text,
            },
            dir.path().to_path_buf(),
            &mut telemetry,
            &mut local_usage,
            &mut ui,
        )
        .unwrap_err();
        assert!(err.is::<crate::RenderedCliError>(), "{err:?}");
        ui.flush().unwrap();
        let stderr = captured_stderr.text();
        assert!(
            stderr.contains("History sessions are not ready"),
            "{stderr}"
        );
        assert!(stderr.contains("ctx setup"), "{stderr}");
    }

    #[test]
    fn invalid_since_error_reports_cli_flag_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut telemetry = show_telemetry();
        let mut local_usage = CliUsage::excluded();
        let (mut ui, _captured_stderr) = test_ui();

        let err = run_sessions(
            SessionsArgs {
                since: Some("bogus".to_owned()),
                provider: None,
                workspace: None,
                no_subagents: false,
                limit: DEFAULT_SESSIONS_LIMIT,
                format: crate::JsonOutputFormat::Json,
            },
            dir.path().to_path_buf(),
            &mut telemetry,
            &mut local_usage,
            &mut ui,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid --since"), "{err}");
    }

    #[test]
    fn deferred_projection_generation_race_renders_active_generation_race_diagnostic() {
        let (mut ui, captured_stderr) = test_ui();
        let err = handle_sessions_error(
            SessionsApplicationError::Projection(anyhow::Error::new(
                IndexError::ConcurrentGenerationChange,
            )),
            false,
            &mut ui,
        )
        .unwrap_err();
        assert!(err.is::<crate::RenderedCliError>(), "{err:?}");
        ui.flush().unwrap();
        let stderr = captured_stderr.text();
        assert!(
            stderr.contains("generation") || stderr.contains("retry") || stderr.contains("updated"),
            "{stderr}"
        );
    }
}
