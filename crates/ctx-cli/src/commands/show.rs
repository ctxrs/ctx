use std::path::PathBuf;

use anyhow::Result;

use ctx_history_core::database_path;

use crate::analytics::AnalyticsProperties;
use crate::output::effective_format;
use crate::provider_args::ProviderArg;
use crate::store_util::open_existing_store_read_only;
use crate::transcript::{
    event_window, resolve_event, resolve_session, write_rendered_events, write_rendered_session,
};
use crate::{analytics, ShowArgs, ShowTarget};

pub(crate) fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let db_path = database_path(data_root);
    let store = open_existing_store_read_only(&db_path, "ctx show")?;
    match args.target {
        ShowTarget::Session(args) => {
            let session = resolve_session(
                &store,
                args.id,
                args.provider.map(ProviderArg::capture_provider),
                args.provider_session.as_deref(),
            )?;
            let events = store.events_for_session(session.id)?;
            analytics::insert_count_bucket(
                analytics_properties,
                "events_returned_bucket",
                events.len() as u64,
            );
            let format = effective_format(args.format, args.json);
            write_rendered_session(&store, &session, &events, args.mode, format, args.out)?;
        }
        ShowTarget::Event(args) => {
            let event = resolve_event(&store, &args.id)?;
            let events = event_window(&store, &event, args.before, args.after, args.window)?;
            analytics::insert_count_bucket(
                analytics_properties,
                "events_returned_bucket",
                events.len() as u64,
            );
            let format = effective_format(args.format, args.json);
            write_rendered_events(&store, &event, &events, format, None)?;
        }
    }
    Ok(())
}
