#[allow(unused_imports)]
use super::*;

#[derive(Debug, Args)]
pub(crate) struct LocateArgs {
    #[command(subcommand)]
    pub(crate) target: LocateTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LocateFormat {
    Text,
    Json,
}

impl LocateFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

pub(crate) fn run_locate(
    args: LocateArgs,
    data_root: PathBuf,
    _analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let db_path = database_path(data_root);
    let store = open_existing_store_read_only(&db_path, "ctx locate")?;
    match args.target {
        LocateTarget::Session(args) => {
            let session = resolve_session(
                &store,
                args.id,
                args.provider.map(ProviderArg::capture_provider),
                args.provider_session.as_deref(),
            )?;
            let value = locate_session_json(&store, &session);
            if locate_json_output(args.format, args.json) {
                print_json(value)?;
            } else {
                print_locate_session_text(&value)?;
            }
        }
        LocateTarget::Event(args) => {
            let event = resolve_event(&store, &args.id)?;
            let value = locate_event_json(&store, &event);
            if locate_json_output(args.format, args.json) {
                print_json(value)?;
            } else {
                print_locate_event_text(&value)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn locate_json_output(format: LocateFormat, json: bool) -> bool {
    json || format == LocateFormat::Json
}
