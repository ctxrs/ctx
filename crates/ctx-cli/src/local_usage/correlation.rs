use std::{
    collections::{hash_map::Entry, HashMap},
    ffi::OsString,
    fs,
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
    context: EphemeralContextCorrelation<McpContextTarget>,
    context_store_revision: Option<UsageStoreRevision>,
    #[cfg(test)]
    trace: Option<std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>>,
}

impl McpUsageRecorder {
    pub(crate) fn start(data_root: PathBuf) -> Self {
        let mut control_resolver = crate::config::LocalUsageConfigResolver::default();
        let enabled = control_resolver.resolve(&data_root).effective_on_startup();
        Self {
            data_root,
            enabled,
            control_resolver,
            context: EphemeralContextCorrelation::default(),
            context_store_revision: None,
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
        if self.enabled {
            self.discard_context_after_store_change();
            let mut operation = invocation.completed(response, duration, serialized_response_bytes);
            let mut pending_context = None;
            if operation.outcome == Outcome::Success && invocation.participates_in_correlation() {
                let mut context = self.context.clone();
                Self::apply_delivered_correlation(
                    &mut context,
                    invocation,
                    response,
                    &mut operation,
                );
                pending_context = Some(context);
            }
            if store::record(&self.data_root, operation).is_ok() {
                if let Some(context) = pending_context {
                    self.context = context;
                }
                self.refresh_context_store_revision();
                #[cfg(test)]
                if let Some(trace) = &self.trace {
                    trace.lock().unwrap().push("local_usage");
                }
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
        self.enabled = self
            .control_resolver
            .resolve(&self.data_root)
            .effective_after(Some(self.enabled));
    }

    /// A reset has no persisted generation marker. Compare only cheap file
    /// metadata for the SQLite family before carrying transient identities
    /// into another write. Any external change or uncertain observation is
    /// treated conservatively as a new generation.
    fn discard_context_after_store_change(&mut self) {
        if !self.context.has_activity() {
            return;
        }
        let revision = UsageStoreRevision::capture(&self.data_root);
        if revision != self.context_store_revision {
            self.context = EphemeralContextCorrelation::default();
            self.context_store_revision = None;
        }
    }

    /// Advance the transient revision only after the aggregate write commits.
    /// If the post-write family cannot be observed safely, discard correlation
    /// rather than risk validating against an uncertain persisted generation.
    fn refresh_context_store_revision(&mut self) {
        if !self.context.has_activity() {
            self.context_store_revision = None;
            return;
        }
        if let Some(revision) = UsageStoreRevision::capture(&self.data_root) {
            self.context_store_revision = Some(revision);
        } else {
            self.context = EphemeralContextCorrelation::default();
            self.context_store_revision = None;
        }
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
struct UsageStoreRevision {
    main: UsageStoreFileRevision,
    wal: Option<UsageStoreFileRevision>,
    shm: Option<UsageStoreFileRevision>,
}

impl UsageStoreRevision {
    fn capture(data_root: &Path) -> Option<Self> {
        let main_path = store::usage_path(data_root);
        Some(Self {
            main: UsageStoreFileRevision::capture_required(&main_path)?,
            wal: UsageStoreFileRevision::capture_optional(&sqlite_auxiliary_path(
                &main_path, "-wal",
            ))?,
            shm: UsageStoreFileRevision::capture_optional(&sqlite_auxiliary_path(
                &main_path, "-shm",
            ))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageStoreFileRevision {
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
}

impl UsageStoreFileRevision {
    fn capture_required(path: &Path) -> Option<Self> {
        Self::from_metadata(path.symlink_metadata().ok()?)
    }

    fn capture_optional(path: &Path) -> Option<Option<Self>> {
        match path.symlink_metadata() {
            Ok(metadata) => Self::from_metadata(metadata).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(None),
            Err(_) => None,
        }
    }

    fn from_metadata(metadata: fs::Metadata) -> Option<Self> {
        if !metadata.file_type().is_file() {
            return None;
        }
        Some(Self {
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
        })
    }
}

fn sqlite_auxiliary_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}
