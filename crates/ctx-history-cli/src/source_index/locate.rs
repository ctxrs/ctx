use std::path::PathBuf;

use anyhow::Result;
use ctx_history_read_application::{
    execute_locate, GenerationReadError, GenerationReadTarget, LocateApplicationError,
    LocateApplicationRequest, LocateRequest,
};

use crate::{
    cli::{LocateArgs, LocateTarget},
    local_usage::{CliUsage, ResultObservationAction},
    output::print_json,
    ui::{canonical_human_output_bytes, Ui},
};

use super::{
    compact_presentation::generation_read,
    render::{pretty_json_stdout_bytes, render_locate_document},
    shared::{externalize_query_error, index_root, open_index},
};

pub fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let (request, json_output) = match args.target {
        LocateTarget::Session(args) => {
            let json_output = args.format.is_json();
            let provider = args.provider.map(|provider| provider.capture_provider());
            (
                LocateRequest::Session {
                    selector: args.id,
                    provider_session_id: args.provider_session,
                    provider,
                    provider_key: args.provider_key,
                    source_id: args.source_id,
                },
                json_output,
            )
        }
        LocateTarget::Event(args) => (
            LocateRequest::Event { selector: args.id },
            args.format.is_json(),
        ),
    };
    let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
        generation_read(open_index(&data_root)?, &index_root(&data_root), read)
    };
    let result = execute_locate(
        LocateApplicationRequest {
            request,
            generation_target: GenerationReadTarget::Active,
            compact_projection: !json_output,
        },
        &mut generation,
    )
    .map_err(locate_application_error)?;
    let (value, compact_value) = result.into_read_models();

    let content_bytes = serde_json::to_vec(&value)?.len();
    let render_value = compact_value.as_ref().unwrap_or(&value);
    let output_bytes = if json_output {
        let output_bytes = pretty_json_stdout_bytes(&value)?;
        print_json(value)?;
        output_bytes
    } else {
        let document = render_locate_document(render_value, ui.stdout_context());
        let output_bytes =
            canonical_human_output_bytes(|context| render_locate_document(render_value, context));
        ui.write_stdout(&document)?;
        output_bytes
    };
    local_usage.set_result_observation(ResultObservationAction::Locate, 1, content_bytes);
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

fn locate_application_error(error: LocateApplicationError<anyhow::Error>) -> anyhow::Error {
    match error {
        LocateApplicationError::Generation(GenerationReadError::Port(error))
        | LocateApplicationError::Projection(error) => error,
        LocateApplicationError::Generation(GenerationReadError::Authority(error)) => {
            anyhow::Error::new(error)
        }
        LocateApplicationError::Query(error) => externalize_query_error(error),
    }
}
