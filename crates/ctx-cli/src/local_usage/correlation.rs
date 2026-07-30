use std::{
    collections::{hash_map::Entry, HashMap},
    hash::Hash,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

use serde_json::Value;

use super::{
    mcp_search_context_targets, resolved_mcp_context_target, store, McpContextTarget,
    McpInvocation, Outcome,
};

pub(super) const CONTEXT_CORRELATION_MAX_RECORDS: usize = 1_024;

#[derive(Debug, Clone, Copy, Default)]
struct ContextRecordState {
    opened: bool,
}

/// Bounded, process-local search-to-open correlation.
///
/// The keys never cross the persistence boundary. Definition 2 has no open or
/// citation-credit counter, so this state is observational only and is cleared
/// on a local-usage control revision. Citation correlation is intentionally
/// unsupported until a production citation event exists.
#[derive(Debug, Clone)]
struct EphemeralContextCorrelation<K> {
    records: HashMap<K, ContextRecordState>,
}

impl<K> Default for EphemeralContextCorrelation<K> {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> EphemeralContextCorrelation<K> {
    fn record_found(&mut self, key: K) -> bool {
        if self.records.len() >= CONTEXT_CORRELATION_MAX_RECORDS {
            return false;
        }
        if let Entry::Vacant(entry) = self.records.entry(key) {
            entry.insert(ContextRecordState::default());
            true
        } else {
            false
        }
    }

    fn record_opened(&mut self, key: &K) -> bool {
        let Some(state) = self.records.get_mut(key) else {
            return false;
        };
        if state.opened {
            false
        } else {
            state.opened = true;
            true
        }
    }
}

pub(crate) struct McpUsageRecorder {
    data_root: PathBuf,
    enabled: bool,
    control_resolver: crate::config::LocalUsageConfigResolver,
    control_revision: Option<ControlFileRevision>,
    context: EphemeralContextCorrelation<McpContextTarget>,
    #[cfg(test)]
    trace: Option<std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>>,
}

impl McpUsageRecorder {
    pub(crate) fn start(data_root: PathBuf) -> Self {
        let mut control_resolver = crate::config::LocalUsageConfigResolver::default();
        let revision_before = ControlFileRevision::capture(&data_root);
        let enabled = control_resolver.resolve(&data_root).effective_on_startup();
        let revision_after = ControlFileRevision::capture(&data_root);
        Self {
            data_root,
            enabled,
            control_resolver,
            control_revision: stable_control_revision(revision_before, revision_after),
            context: EphemeralContextCorrelation::default(),
            #[cfg(test)]
            trace: None,
        }
    }

    pub(crate) fn record_delivered(
        &mut self,
        invocation: McpInvocation,
        response: &Value,
        duration: Duration,
        serialized_response_bytes: usize,
    ) {
        self.refresh_control();
        if !self.enabled {
            return;
        }
        let operation = invocation.completed(response, duration, serialized_response_bytes);
        let mut next_context = self.context.clone();
        if operation.outcome == Outcome::Success {
            Self::apply_delivered_correlation(&mut next_context, &invocation, response);
        }
        if store::record(&self.data_root, operation).is_ok() {
            self.context = next_context;
            #[cfg(test)]
            if let Some(trace) = &self.trace {
                trace.lock().unwrap().push("local_usage");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_test_trace(
        &mut self,
        trace: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    ) {
        self.trace = Some(trace);
    }

    fn refresh_control(&mut self) {
        let revision_before = ControlFileRevision::capture(&self.data_root);
        let enabled = self
            .control_resolver
            .resolve(&self.data_root)
            .effective_after(Some(self.enabled));
        let revision_after = ControlFileRevision::capture(&self.data_root);
        let control_revision = stable_control_revision(revision_before, revision_after);
        if control_revision.is_none()
            || self.control_revision.as_ref() != control_revision.as_ref()
            || self.enabled && !enabled
        {
            self.context = EphemeralContextCorrelation::default();
        }
        self.control_revision = control_revision;
        self.enabled = enabled;
    }

    fn apply_delivered_correlation(
        context: &mut EphemeralContextCorrelation<McpContextTarget>,
        invocation: &McpInvocation,
        response: &Value,
    ) -> bool {
        if invocation.operation == "search" {
            for target in mcp_search_context_targets(response) {
                context.record_found(target);
            }
            return false;
        }
        // Never correlate on the caller-supplied selector: show accepts UUID
        // prefixes. Only the canonical full ID returned by the successful
        // result is eligible. Missing canonical IDs make correlation
        // unavailable for that delivery.
        resolved_mcp_context_target(invocation.operation, response)
            .is_some_and(|target| context.record_opened(&target))
    }

    #[cfg(test)]
    pub(in crate::local_usage) fn correlate_delivered_for_test(
        &mut self,
        invocation: &McpInvocation,
        response: &Value,
    ) -> bool {
        Self::apply_delivered_correlation(&mut self.context, invocation, response)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ControlFileRevision {
    Missing,
    File {
        len: u64,
        modified: SystemTime,
        created: Option<SystemTime>,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        changed_seconds: i64,
        #[cfg(unix)]
        changed_nanoseconds: i64,
    },
}

impl ControlFileRevision {
    fn capture(data_root: &Path) -> Option<Self> {
        let path = crate::config::AppConfig::config_path(data_root);
        match path.metadata() {
            Ok(metadata) if metadata.is_file() => Some(Self::File {
                len: metadata.len(),
                modified: metadata.modified().ok()?,
                created: metadata.created().ok(),
                #[cfg(unix)]
                device: metadata.dev(),
                #[cfg(unix)]
                inode: metadata.ino(),
                #[cfg(unix)]
                changed_seconds: metadata.ctime(),
                #[cfg(unix)]
                changed_nanoseconds: metadata.ctime_nsec(),
            }),
            Ok(_) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Self::Missing),
            Err(_) => None,
        }
    }
}

fn stable_control_revision(
    before: Option<ControlFileRevision>,
    after: Option<ControlFileRevision>,
) -> Option<ControlFileRevision> {
    match (before, after) {
        (Some(before), Some(after)) if before == after => Some(after),
        _ => None,
    }
}
