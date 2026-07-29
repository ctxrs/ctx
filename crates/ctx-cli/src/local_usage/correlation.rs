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
    mcp_search_context_targets, store, CompletedOperation, McpContextTarget, McpInvocation, Outcome,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContextUsage {
    pub(super) context_searches: u64,
    pub(super) context_found: u64,
    pub(super) context_opened: u64,
    pub(super) context_cited: u64,
    pub(super) validated_discoveries: u64,
}

impl ContextUsage {
    pub(super) fn saturating_add(&mut self, other: Self) {
        self.context_searches = self.context_searches.saturating_add(other.context_searches);
        self.context_found = self.context_found.saturating_add(other.context_found);
        self.context_opened = self.context_opened.saturating_add(other.context_opened);
        self.context_cited = self.context_cited.saturating_add(other.context_cited);
        self.validated_discoveries = self
            .validated_discoveries
            .saturating_add(other.validated_discoveries);
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ContextRecordState {
    opened: bool,
    cited: bool,
    validated: bool,
}

/// In-memory-only identity correlation for a bounded foreground workflow.
///
/// Keys are never exposed to the persisted DTO. Consuming `finish` drops every
/// key and returns only aggregate counters.
#[derive(Debug, Clone)]
pub(crate) struct EphemeralContextCorrelation<K> {
    records: HashMap<K, ContextRecordState>,
    usage: ContextUsage,
}

pub(super) const CONTEXT_CORRELATION_MAX_RECORDS: usize = 1_024;

impl<K> Default for EphemeralContextCorrelation<K> {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
            usage: ContextUsage::default(),
        }
    }
}

impl<K: Eq + Hash> EphemeralContextCorrelation<K> {
    fn has_activity(&self) -> bool {
        !self.records.is_empty() || self.usage != ContextUsage::default()
    }

    pub(crate) fn record_search(&mut self) {
        self.usage.context_searches = self.usage.context_searches.saturating_add(1);
    }

    pub(crate) fn record_found(&mut self, key: K) -> bool {
        if self.records.len() >= CONTEXT_CORRELATION_MAX_RECORDS {
            return false;
        }
        if let Entry::Vacant(entry) = self.records.entry(key) {
            entry.insert(ContextRecordState::default());
            self.usage.context_found = self.usage.context_found.saturating_add(1);
            true
        } else {
            false
        }
    }

    pub(crate) fn record_opened(&mut self, key: &K) -> ContextUsage {
        let Some(state) = self.records.get_mut(key) else {
            return ContextUsage::default();
        };
        let mut delta = ContextUsage::default();
        if !state.opened {
            state.opened = true;
            self.usage.context_opened = self.usage.context_opened.saturating_add(1);
            delta.context_opened = 1;
        }
        if validate_discovery(state, &mut self.usage) {
            delta.validated_discoveries = 1;
        }
        delta
    }

    pub(crate) fn record_cited(&mut self, key: &K) -> ContextUsage {
        let Some(state) = self.records.get_mut(key) else {
            return ContextUsage::default();
        };
        let mut delta = ContextUsage::default();
        if !state.cited {
            state.cited = true;
            self.usage.context_cited = self.usage.context_cited.saturating_add(1);
            delta.context_cited = 1;
        }
        if validate_discovery(state, &mut self.usage) {
            delta.validated_discoveries = 1;
        }
        delta
    }

    pub(crate) fn finish(self) -> ContextUsage {
        self.usage
    }
}

fn validate_discovery(state: &mut ContextRecordState, usage: &mut ContextUsage) -> bool {
    if !state.validated {
        state.validated = true;
        usage.validated_discoveries = usage.validated_discoveries.saturating_add(1);
        true
    } else {
        false
    }
}

pub(crate) struct McpUsageRecorder {
    data_root: PathBuf,
    enabled: bool,
    control_resolver: crate::config::LocalUsageConfigResolver,
    control_revision: Option<ControlFileRevision>,
    context: EphemeralContextCorrelation<McpContextTarget>,
    context_generation: Option<store::StoreGeneration>,
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
            context_generation: None,
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
        self.record_delivered_with_store_hook(
            invocation,
            response,
            duration,
            serialized_response_bytes,
            || {},
        );
    }

    fn record_delivered_with_store_hook<T>(
        &mut self,
        invocation: McpInvocation,
        response: &Value,
        duration: Duration,
        serialized_response_bytes: usize,
        after_generation_check: impl FnOnce() -> T,
    ) {
        self.refresh_control();
        if !self.enabled {
            return;
        }
        let operation = invocation.completed(response, duration, serialized_response_bytes);
        let recorded =
            if operation.outcome == Outcome::Success && invocation.participates_in_correlation() {
                let is_search = invocation.operation == "search";
                let mut matching_context = self.context.clone();
                let mut matching_operation = operation;
                Self::apply_delivered_correlation(
                    &mut matching_context,
                    invocation.clone(),
                    response,
                    &mut matching_operation,
                );
                let mut stale_context = EphemeralContextCorrelation::default();
                let mut stale_operation = operation;
                Self::apply_delivered_correlation(
                    &mut stale_context,
                    invocation,
                    response,
                    &mut stale_operation,
                );
                store::record_correlated_with_hook(
                    &self.data_root,
                    self.context_generation,
                    matching_operation,
                    stale_operation,
                    after_generation_check,
                )
                .map(|commit| {
                    if is_search {
                        self.context = if commit.expected_generation_matched {
                            matching_context
                        } else {
                            stale_context
                        };
                        self.context_generation =
                            self.context.has_activity().then_some(commit.generation);
                    } else if commit.expected_generation_matched {
                        self.context = matching_context;
                    } else {
                        self.clear_context();
                    }
                })
            } else {
                store::record(&self.data_root, operation).map(|generation| {
                    if self
                        .context_generation
                        .is_some_and(|expected| expected != generation)
                    {
                        self.clear_context();
                    }
                })
            };
        if recorded.is_ok() {
            #[cfg(test)]
            if let Some(trace) = &self.trace {
                trace.lock().unwrap().push("local_usage");
            }
        }
    }

    #[cfg(test)]
    pub(in crate::local_usage) fn record_delivered_with_store_hook_for_test<T>(
        &mut self,
        invocation: McpInvocation,
        response: &Value,
        duration: Duration,
        serialized_response_bytes: usize,
        after_generation_check: impl FnOnce() -> T,
    ) {
        self.record_delivered_with_store_hook(
            invocation,
            response,
            duration,
            serialized_response_bytes,
            after_generation_check,
        );
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
        // A persistent opt-out can be enabled again between two delivered MCP
        // calls. Treat every observable config-file write as a correlation
        // barrier even if the currently resolved value is enabled again.
        if control_revision.is_none()
            || self.control_revision.as_ref() != control_revision.as_ref()
            || self.enabled && !enabled
        {
            self.clear_context();
        }
        self.control_revision = control_revision;
        self.enabled = enabled;
    }

    fn clear_context(&mut self) {
        self.context = EphemeralContextCorrelation::default();
        self.context_generation = None;
    }

    #[cfg(test)]
    pub(in crate::local_usage) fn correlate_delivered(
        &mut self,
        invocation: McpInvocation,
        response: &Value,
        operation: &mut CompletedOperation,
    ) {
        Self::apply_delivered_correlation(&mut self.context, invocation, response, operation);
    }

    fn apply_delivered_correlation(
        context: &mut EphemeralContextCorrelation<McpContextTarget>,
        invocation: McpInvocation,
        response: &Value,
        operation: &mut CompletedOperation,
    ) {
        if operation.outcome != Outcome::Success {
            return;
        }
        if invocation.operation == "search" {
            context.record_search();
            operation.context.context_searches = 1;
            for target in mcp_search_context_targets(response) {
                if context.record_found(target) {
                    operation.context.context_found =
                        operation.context.context_found.saturating_add(1);
                }
            }
            return;
        }
        if let Some(target) = invocation.context_target {
            operation
                .context
                .saturating_add(context.record_opened(&target));
        }
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
