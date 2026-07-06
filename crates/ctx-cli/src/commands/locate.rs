use std::path::PathBuf;

use anyhow::Result;

use ctx_history_core::database_path;

use crate::analytics::AnalyticsProperties;
use crate::output::{locate_json_output, print_json};
use crate::provider_args::ProviderArg;
use crate::store_util::open_existing_store_read_only;
use crate::transcript::{
    locate_event_json, locate_session_json, print_locate_event_text, print_locate_session_text,
    resolve_event, resolve_session,
};
use crate::{LocateArgs, LocateTarget};

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
