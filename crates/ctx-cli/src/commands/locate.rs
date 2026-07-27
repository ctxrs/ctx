use std::path::PathBuf;

use anyhow::Result;

use ctx_history_core::database_path;

use crate::analytics::LocateTelemetry;
use crate::output::{locate_json_output, print_json};
use crate::pro::ResourceKindArg;
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
    _telemetry: &mut LocateTelemetry,
) -> Result<()> {
    match args.target {
        LocateTarget::Session(args) => {
            let store = open_existing_store_read_only(&database_path(data_root), "ctx locate")?;
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
            let store = open_existing_store_read_only(&database_path(data_root), "ctx locate")?;
            let event = resolve_event(&store, &args.id)?;
            let value = locate_event_json(&store, &event);
            if locate_json_output(args.format, args.json) {
                print_json(value)?;
            } else {
                print_locate_event_text(&value)?;
            }
        }
        LocateTarget::Commit(args) => {
            return run_resource(args, ResourceKindArg::Commit, data_root)
        }
        LocateTarget::PullRequest(args) => {
            return run_resource(args, ResourceKindArg::PullRequest, data_root)
        }
        LocateTarget::Issue(args) => return run_resource(args, ResourceKindArg::Issue, data_root),
        LocateTarget::File(args) => {
            return run_resource(args.into(), ResourceKindArg::File, data_root)
        }
        LocateTarget::Branch(args) => {
            return run_resource(args, ResourceKindArg::Branch, data_root)
        }
        LocateTarget::Repository(args) => {
            return run_resource(args, ResourceKindArg::Repository, data_root)
        }
    }
    Ok(())
}

fn run_resource(
    args: crate::commands::work_graph::ResourceValueArgs,
    kind: ResourceKindArg,
    data_root: PathBuf,
) -> Result<()> {
    crate::commands::work_graph::run(
        args.into_work_graph(kind),
        data_root,
        ctx_pro_host_protocol::QueryKind::Locate,
        "pro_location",
    )
}
