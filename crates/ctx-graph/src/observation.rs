use super::*;
pub(crate) use ctx_graph_core::observation::search;
pub use ctx_graph_core::observation::*;
use std::{sync::Arc, time::Instant};

/// Optional host-owned, nonblocking callback. The graph engine never sends telemetry.
/// The caller resolves consent and must enqueue rather than perform network I/O here.
pub type GraphObserver = Arc<dyn Fn(GraphObservation) + Send + Sync>;

pub(crate) fn emit(observer: &Option<GraphObserver>, observation: GraphObservation) {
    if let Some(observer) = observer {
        observer(observation);
    }
}

pub(crate) fn operation(command: &Command) -> GraphOperation {
    use GraphOperation as O;
    match command {
        Command::Index { .. } => O::Index,
        Command::Update { .. } => O::Update,
        Command::Watch { .. } => O::Watch,
        Command::CheckUpdate => O::CheckUpdate,
        Command::Add { .. } => O::Add,
        Command::Clone { .. } => O::Clone,
        Command::Import { .. } => O::Import,
        Command::Compact => O::Compact,
        Command::Query(_) => O::Search,
        Command::Show(_) => O::Show,
        Command::Callers(_) => O::Callers,
        Command::Callees(_) => O::Callees,
        Command::Impact(_) => O::Impact,
        Command::Path(_) => O::Path,
        Command::Stats => O::Stats,
        Command::Serve(_) => O::Serve,
        Command::Switch(_) => O::Switch,
        Command::Install(_) => O::Install,
        Command::Uninstall(_) => O::Uninstall,
        Command::Hook(_) => O::Hook,
        Command::HookGuard(_) => O::HookGuard,
        Command::Extended(command) => command.operation(),
        Command::Connect(command) => match command {
            connect::Command::Introspect { .. } => O::IntrospectPostgres,
            connect::Command::Push {
                backend: connect::Push::Neo4j(_),
            } => O::PushNeo4j,
            connect::Command::Push {
                backend: connect::Push::Falkordb(_),
            } => O::PushFalkorDb,
        },
        Command::Provider(args) => match &args.command {
            extraction::ProviderCommand::List => O::ProviderList,
            extraction::ProviderCommand::Detect => O::ProviderDetect,
            extraction::ProviderCommand::Template(_) => O::ProviderTemplate,
            extraction::ProviderCommand::Setup { .. } => O::ProviderSetup,
            extraction::ProviderCommand::Show { .. } => O::ProviderShow,
            extraction::ProviderCommand::Add { .. } => O::ProviderAdd,
            extraction::ProviderCommand::Remove { .. } => O::ProviderRemove,
        },
        Command::Cache(args) => match args.command {
            extraction::CacheCommand::Inspect { .. } => O::CacheInspect,
            extraction::CacheCommand::Remove { .. } => O::CacheRemove,
        },
    }
}

/// Finish at the host writer boundary, even when presenting an execution error.
pub(crate) fn finish_cli(
    facts: &mut GraphObservation,
    result: &Result<()>,
    json: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> i32 {
    let output_started = Instant::now();
    let mut output_failed = facts.output_served == Some(false);
    if let Err(error) = result {
        let write_error = error.is::<output::OutputFailure>();
        output_failed |= write_error;
        if write_error {
            facts.phase = GraphPhase::OutputWrite;
            facts.output_failure = Some(failure_kind(error));
        } else {
            failed(facts, error);
        }
        if let Err(error) = output::write_error(stderr, error, json) {
            output_failed = true;
            facts.output_failure = Some(failure_kind(&error.into()));
        }
    } else {
        facts.execution_succeeded = Some(true);
    }
    for flush in [stdout.flush(), stderr.flush()] {
        if let Err(error) = flush {
            output_failed = true;
            facts.output_failure = Some(failure_kind(&error.into()));
        }
    }
    facts.output_served = Some(!output_failed);
    facts.output_boundary = GraphOutputBoundary::CliFlush;
    facts.output_duration = Some(output_started.elapsed());
    i32::from(result.is_err() || output_failed)
}

pub fn failure_kind(error: &anyhow::Error) -> GraphFailureKind {
    if let Some(error) = error.downcast_ref::<crate::output::OutputFailure>() {
        return error.kind;
    }
    if error.is::<crate::paths::MissingIndex>() {
        GraphFailureKind::MissingIndex
    } else {
        ctx_graph_core::observation::failure_kind(error)
    }
}
pub fn failed(facts: &mut GraphObservation, error: &anyhow::Error) {
    if error.is::<crate::paths::MissingIndex>() {
        facts.fail(GraphFailureKind::MissingIndex);
    }
    ctx_graph_core::observation::failed(facts, error);
}

#[cfg(test)]
mod tests;
