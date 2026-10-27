//! Native ASR export is independent of the searchable Core store.
pub(crate) use ctx_cli_presentation::commands::asr_export::ExportArgs;

pub(crate) fn run(args: ExportArgs) -> i32 {
    let result = args
        .execute(crate::identity::home_dir().as_deref())
        .and_then(|summary| Ok(serde_json::to_string(&summary)?));
    match result {
        Ok(summary) => {
            crate::output::write_stdout_line(format_args!("{summary}"));
            0
        }
        Err(error) => {
            crate::output::write_stderr_line(format_args!("ASR export failed: {error:#}"));
            1
        }
    }
}
