//! Process-owned engine projections and optional delivery. Engines know neither
//! history storage nor analytics configuration, identities, endpoints or queues.
use ctx_client_observability::analytics::PublicEventV1;
use std::{
    path::{Path, PathBuf},
    process::Child,
};

mod graph;
mod graph_vocabulary;
mod remote;
mod sift;
mod sift_vocabulary;
pub(crate) use graph::{graph, graph_completed};
pub(crate) use remote::remote;
pub(crate) use sift::sift;

/// Keep the existing bounded sender alive for the whole CLI/native MCP run.
/// SDK callbacks only project typed facts and enqueue; the worker owns IO.
pub(crate) fn observe_graph(
    data_root: Option<PathBuf>,
    run: impl FnOnce(Option<ctx_graph::GraphObserver>) -> i32,
) -> i32 {
    let Some(owner) = EngineTelemetry::optional(data_root) else {
        return run(None);
    };
    let owner = std::sync::Mutex::new(owner);
    let sender =
        ctx_client_observability::mcp_observation::ProductEventSender::start(move |events| {
            if let Ok(mut owner) = owner.lock() {
                owner.record(events);
            }
            Ok(())
        });
    let enqueue = sender.event_callback();
    let observer = std::sync::Arc::new(move |facts| {
        if let Some(event) = graph(facts) {
            enqueue(event);
        }
    });
    let status = run(Some(observer));
    sender.shutdown();
    status
}

/// One process/runtime owner; later observations reap its one pending uploader.
/// A finite command may exit with its detached uploader still running.
pub(crate) struct EngineTelemetry {
    root: PathBuf,
    owner: String,
    endpoint: String,
    pending_delivery: Option<Child>,
}

impl EngineTelemetry {
    pub(crate) fn optional(data_root: Option<PathBuf>) -> Option<Self> {
        let root = crate::dispatch::resolve_history_data_root(data_root).ok()?;
        Self::at_root(root)
    }

    pub(crate) fn at_root(root: PathBuf) -> Option<Self> {
        // An empty append applies opt-out purge without creating an identity.
        let _ = crate::observability_composition::append_optional_analytics_batch(&root, &[]);
        let endpoint = crate::observability_composition::optional_analytics_endpoint(&root)?;
        let owner = crate::identity::try_installation_id(&root).ok().flatten()?;
        Some(Self {
            root,
            owner,
            endpoint,
            pending_delivery: None,
        })
    }

    pub(crate) fn record(&mut self, events: &[PublicEventV1]) {
        let mut direct_start = 0;
        for (index, event) in events.iter().enumerate() {
            if let PublicEventV1::SiftSummary(summary) = event {
                self.append(&events[direct_start..index]);
                crate::analytics_summary::record_sift_for_owner(
                    &self.root,
                    &self.owner,
                    &self.endpoint,
                    *summary,
                );
                direct_start = index + 1;
            }
        }
        self.append(&events[direct_start..]);
        self.schedule();
    }

    fn append(&self, events: &[PublicEventV1]) {
        let _ = crate::observability_composition::append_optional_analytics_batch_for_owner(
            &self.root,
            &self.owner,
            &self.endpoint,
            events,
        );
    }

    pub(crate) fn record_sift(&mut self, facts: ctx_sift::observation::SiftObservation) {
        crate::analytics_summary::record_sift_for_owner(
            &self.root,
            &self.owner,
            &self.endpoint,
            sift(facts),
        );
        self.schedule();
    }

    fn schedule(&mut self) {
        if let Some(child) = self.pending_delivery.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => self.pending_delivery = None,
                // A transient reap error is not permission to spawn again.
                Ok(None) | Err(_) => return,
            }
        }
        if crate::identity::try_existing_installation_id(&self.root)
            .ok()
            .flatten()
            .as_deref()
            != Some(self.owner.as_str())
            || crate::observability_composition::optional_analytics_endpoint(&self.root).as_deref()
                != Some(self.endpoint.as_str())
        {
            return;
        }
        self.pending_delivery = crate::analytics_delivery::schedule(&self.root);
    }
}

/// The MCP dispatch closure is called by the existing bounded sender, never by
/// the SDK callback. This owner serializes access to its one delivery child.
pub(crate) fn mcp(root: &Path, remote: bool) -> ctx_agent_application::mcp::McpTelemetry {
    let owner = EngineTelemetry::at_root(root.to_path_buf());
    let enabled = owner.is_some();
    let owner = std::sync::Mutex::new(owner);
    let telemetry = ctx_agent_application::mcp::McpTelemetry::start(enabled, move |events| {
        if let Ok(mut owner) = owner.lock() {
            if let Some(owner) = owner.as_mut() {
                owner.record(events);
            }
        }
        Ok(())
    });
    if remote {
        telemetry.for_remote()
    } else {
        telemetry
    }
}

#[cfg(test)]
mod tests;
