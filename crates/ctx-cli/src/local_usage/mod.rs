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

use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, BlameTarget, CommitPredicate, FactState, ProductionRelationship,
    PullRequestBlameRelationship,
};
use serde_json::Value;

use crate::cli::{CommandRoot, DaemonCommand};

mod estimate;
mod report;
mod store;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub(crate) use estimate::{
    estimate_usage, CoveredTokenEstimate, EstimateCoverage, EstimateFacts, EstimateModel,
    UsageEstimates, ESTIMATE_MODEL,
};
pub(crate) use report::{pro_conversion_action, read_report, render_human_summary, UsageReport};
pub(crate) use store::{reset, UsageStoreError};

pub(crate) const DEFINITION_VERSION: i64 = 2;
pub(crate) const RETENTION_DAYS: i64 = 400;
pub(crate) const CTX_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Cli,
    Mcp,
}

impl Surface {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Success,
    Failure,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueClass {
    ResultBearing,
    Empty,
    NotApplicable,
}

impl ValueClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ResultBearing => "result_bearing",
            Self::Empty => "empty",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// Closed, content-free classification for a foreground result observation.
///
/// The action is checked against the invocation's recorded operation before it
/// can influence estimates, so a show, blame, or unrelated result cannot be
/// mislabeled as a search by a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultObservationAction {
    Search,
    OpenSession,
    OpenEvent,
    Locate,
    Sources,
    Sql,
    Blame,
}

impl ResultObservationAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::OpenSession => "open_session",
            Self::OpenEvent => "open_event",
            Self::Locate => "locate",
            Self::Sources => "sources",
            Self::Sql => "sql",
            Self::Blame => "blame",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContextUsage {
    context_searches: u64,
    context_found: u64,
    context_opened: u64,
    context_cited: u64,
    validated_discoveries: u64,
}

impl ContextUsage {
    fn saturating_add(&mut self, other: Self) {
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

const CONTEXT_CORRELATION_MAX_RECORDS: usize = 1_024;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetType {
    File,
    Commit,
    PullRequest,
    NotApplicable,
}

impl TargetType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Commit => "commit",
            Self::PullRequest => "pull_request",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProOutcome {
    Produced,
    Possible,
    None,
    Error,
    NotApplicable,
}

impl ProOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Produced => "produced",
            Self::Possible => "possible",
            Self::None => "none",
            Self::Error => "error",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationBucket {
    Under10Ms,
    Ms10To49,
    Ms50To249,
    Ms250To999,
    Sec1To4,
    Sec5To29,
    Sec30Plus,
}

impl DurationBucket {
    fn from_duration(duration: Duration) -> Self {
        match duration.as_millis() {
            0..=9 => Self::Under10Ms,
            10..=49 => Self::Ms10To49,
            50..=249 => Self::Ms50To249,
            250..=999 => Self::Ms250To999,
            1_000..=4_999 => Self::Sec1To4,
            5_000..=29_999 => Self::Sec5To29,
            _ => Self::Sec30Plus,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Under10Ms => "under_10_ms",
            Self::Ms10To49 => "10_to_49_ms",
            Self::Ms50To249 => "50_to_249_ms",
            Self::Ms250To999 => "250_to_999_ms",
            Self::Sec1To4 => "1_to_4_s",
            Self::Sec5To29 => "5_to_29_s",
            Self::Sec30Plus => "30_s_or_more",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompletedOperation {
    surface: Surface,
    operation: &'static str,
    outcome: Outcome,
    value_class: ValueClass,
    duration: DurationBucket,
    target_type: TargetType,
    pro_outcome: ProOutcome,
    result_count: u64,
    citation_count: u64,
    result_action: Option<ResultObservationAction>,
    latency_ms: u64,
    latency_samples: u64,
    response_bytes: u64,
    response_byte_samples: u64,
    output_bytes: u64,
    output_byte_samples: u64,
    context_bytes: u64,
    context_byte_samples: u64,
    search_result_bytes: u64,
    search_result_byte_samples: u64,
    context: ContextUsage,
}

impl CompletedOperation {
    pub(crate) fn cli(operation: &'static str, success: bool, duration: Duration) -> Self {
        Self {
            surface: Surface::Cli,
            operation,
            outcome: if success {
                Outcome::Success
            } else {
                Outcome::Failure
            },
            value_class: ValueClass::NotApplicable,
            duration: DurationBucket::from_duration(duration),
            target_type: TargetType::NotApplicable,
            pro_outcome: ProOutcome::NotApplicable,
            result_count: 0,
            citation_count: 0,
            result_action: None,
            latency_ms: duration_millis(duration),
            latency_samples: 1,
            response_bytes: 0,
            response_byte_samples: 0,
            output_bytes: 0,
            output_byte_samples: 0,
            context_bytes: 0,
            context_byte_samples: 0,
            search_result_bytes: 0,
            search_result_byte_samples: 0,
            context: ContextUsage::default(),
        }
    }

    pub(crate) fn with_value(mut self, value_class: ValueClass) -> Self {
        self.value_class = value_class;
        self
    }

    #[cfg(test)]
    pub(crate) const fn result_metadata_for_test(self) -> (ValueClass, u64, u64) {
        (self.value_class, self.result_count, self.citation_count)
    }

    #[cfg(test)]
    pub(crate) fn target_type_for_test(self) -> TargetType {
        self.target_type
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CliUsage {
    operation: Option<&'static str>,
    target_type: TargetType,
    pro_outcome: ProOutcome,
    result_count: usize,
    citation_count: usize,
    semantic_context_bytes: usize,
    semantic_context_bytes_measured: bool,
    output_bytes: usize,
    output_bytes_measured: bool,
    result_action: Option<ResultObservationAction>,
    context: ContextUsage,
    value_class: ValueClass,
}

impl CliUsage {
    pub(crate) fn from_command(command: &CommandRoot) -> Self {
        let (operation, target_type) = match command {
            CommandRoot::Setup(_) => (Some("setup"), TargetType::NotApplicable),
            CommandRoot::Status(_) => (None, TargetType::NotApplicable),
            CommandRoot::Stats(_) => (None, TargetType::NotApplicable),
            CommandRoot::Index(_) => (Some("index"), TargetType::NotApplicable),
            CommandRoot::Sources(_) => (Some("sources"), TargetType::NotApplicable),
            CommandRoot::Import(_) => (Some("import"), TargetType::NotApplicable),
            CommandRoot::Show(_) => (Some("show"), TargetType::NotApplicable),
            CommandRoot::Locate(_) => (Some("locate"), TargetType::NotApplicable),
            CommandRoot::Search(_) => (Some("search"), TargetType::NotApplicable),
            CommandRoot::Pro(args) => (
                Some(args.local_usage_operation()),
                TargetType::NotApplicable,
            ),
            CommandRoot::Referral(_) => (None, TargetType::NotApplicable),
            CommandRoot::Blame(_) => (Some("blame"), TargetType::NotApplicable),
            CommandRoot::Sql(_) => (Some("sql"), TargetType::NotApplicable),
            CommandRoot::Docs(_) => (Some("docs"), TargetType::NotApplicable),
            CommandRoot::Integrations(_) => (Some("integrations"), TargetType::NotApplicable),
            CommandRoot::Mcp(_) => (None, TargetType::NotApplicable),
            CommandRoot::Daemon(args) => match &args.command {
                DaemonCommand::Status(_) => (Some("daemon_status"), TargetType::NotApplicable),
                DaemonCommand::Enable(_) => (Some("daemon_enable"), TargetType::NotApplicable),
                DaemonCommand::Disable(_) => (Some("daemon_disable"), TargetType::NotApplicable),
                DaemonCommand::Run(_) => (None, TargetType::NotApplicable),
            },
            CommandRoot::Upgrade(args) if !args.replacement_helper => {
                (Some("upgrade"), TargetType::NotApplicable)
            }
            CommandRoot::Upgrade(_) => (None, TargetType::NotApplicable),
            CommandRoot::Doctor(_) => (Some("doctor"), TargetType::NotApplicable),
        };
        Self {
            operation,
            target_type,
            pro_outcome: ProOutcome::NotApplicable,
            result_count: 0,
            citation_count: 0,
            semantic_context_bytes: 0,
            semantic_context_bytes_measured: false,
            output_bytes: 0,
            output_bytes_measured: false,
            result_action: None,
            context: ContextUsage::default(),
            value_class: ValueClass::NotApplicable,
        }
    }

    /// Explicitly represents a foreground command that must not be recorded.
    ///
    /// The stats command lane can use this once it owns `CommandRoot::Stats`;
    /// no storage DTO can be produced from this value.
    pub(crate) fn excluded() -> Self {
        Self {
            operation: None,
            target_type: TargetType::NotApplicable,
            pro_outcome: ProOutcome::NotApplicable,
            result_count: 0,
            citation_count: 0,
            semantic_context_bytes: 0,
            semantic_context_bytes_measured: false,
            output_bytes: 0,
            output_bytes_measured: false,
            result_action: None,
            context: ContextUsage::default(),
            value_class: ValueClass::NotApplicable,
        }
    }

    /// Adds only bounded numeric observations from a final foreground result.
    ///
    /// No query, path, content, result identity, or caller identity is accepted
    /// by this boundary. Background work has no `CliUsage` completion boundary
    /// and therefore cannot call this method through the supported API.
    pub(crate) fn set_result_observation(
        &mut self,
        action: ResultObservationAction,
        result_count: usize,
        citation_count: usize,
        content_bytes: usize,
    ) {
        self.result_count = result_count;
        self.citation_count = citation_count;
        self.semantic_context_bytes = content_bytes;
        self.semantic_context_bytes_measured = true;
        self.result_action = Some(action);
        self.value_class = if result_count == 0 {
            ValueClass::Empty
        } else {
            ValueClass::ResultBearing
        };
    }

    /// Records exact bytes written by the foreground CLI command.
    ///
    /// This is deliberately separate from `content_bytes` on
    /// `set_result_observation`: rendered output can contain framing and other
    /// fields that are not semantic search-result content.
    pub(crate) fn set_measured_output_bytes(&mut self, output_bytes: usize) {
        self.output_bytes = output_bytes;
        self.output_bytes_measured = true;
    }

    /// Records one exact, non-duplicated semantic payload byte count.
    ///
    /// Callers may use exact output bytes when output is itself the semantic
    /// payload, or provide a separately measured semantic representation.
    pub(crate) fn set_semantic_context_bytes(&mut self, context_bytes: usize) {
        self.semantic_context_bytes = context_bytes;
        self.semantic_context_bytes_measured = true;
    }

    pub(crate) fn add_context_usage(&mut self, context: ContextUsage) {
        self.context.saturating_add(context);
    }

    pub(crate) fn set_blame_result(&mut self, result: &BlameResult) {
        let semantic_context_bytes_measured = self.semantic_context_bytes_measured;
        self.set_result_observation(
            ResultObservationAction::Blame,
            result.matches.len(),
            result.evidence.len(),
            self.semantic_context_bytes,
        );
        self.semantic_context_bytes_measured = semantic_context_bytes_measured;
        self.pro_outcome = classify_blame(result);
    }

    pub(crate) fn bind_blame_target(&mut self, target: &BlameTarget) {
        self.target_type = match target {
            BlameTarget::File { .. } => TargetType::File,
            BlameTarget::Commit { .. } => TargetType::Commit,
            BlameTarget::PullRequest { .. } => TargetType::PullRequest,
        };
    }

    pub(crate) fn completed(self, success: bool, duration: Duration) -> Option<CompletedOperation> {
        let operation = self.operation?;
        let mut completed =
            CompletedOperation::cli(operation, success, duration).with_value(if success {
                self.value_class
            } else {
                ValueClass::NotApplicable
            });
        completed.target_type = self.target_type;
        if operation == "blame" {
            completed.pro_outcome = if success {
                self.pro_outcome
            } else {
                ProOutcome::Error
            };
            if success {
                completed.result_count = u64::try_from(self.result_count).unwrap_or(u64::MAX);
                completed.citation_count = u64::try_from(self.citation_count).unwrap_or(u64::MAX);
            }
        }
        if success && self.result_action.is_some() {
            completed.result_count = u64::try_from(self.result_count).unwrap_or(u64::MAX);
            completed.citation_count = u64::try_from(self.citation_count).unwrap_or(u64::MAX);
            completed.result_action = self.result_action;
        }
        completed.output_bytes = u64::try_from(self.output_bytes).unwrap_or(u64::MAX);
        completed.output_byte_samples = u64::from(self.output_bytes_measured);
        if success {
            completed.context = self.context;
            completed.context_bytes =
                u64::try_from(self.semantic_context_bytes).unwrap_or(u64::MAX);
            completed.context_byte_samples = u64::from(self.semantic_context_bytes_measured);
        }
        if success
            && self.result_action == Some(ResultObservationAction::Search)
            && operation == "search"
            && self.value_class == ValueClass::ResultBearing
        {
            completed.search_result_bytes =
                u64::try_from(self.semantic_context_bytes).unwrap_or(u64::MAX);
            completed.search_result_byte_samples = u64::from(self.semantic_context_bytes_measured);
        }
        Some(completed)
    }
}

pub(crate) fn record_best_effort(data_root: &Path, enabled: bool, operation: CompletedOperation) {
    if enabled {
        let _ = store::record(data_root, operation);
    }
}

#[cfg(test)]
fn record_best_effort_with_ctx_version_for_test(
    data_root: &Path,
    operation: CompletedOperation,
    ctx_version: &str,
) {
    let _ = store::record_with_ctx_version_for_test(data_root, operation, ctx_version);
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
    fn correlate_delivered(
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum McpContextTarget {
    Session(String),
    Event(String),
}

#[derive(Debug, Clone)]
pub(crate) struct McpInvocation {
    operation: &'static str,
    target_type: TargetType,
    context_target: Option<McpContextTarget>,
}

impl McpInvocation {
    pub(crate) fn recognized(name: &str) -> Option<Self> {
        let operation = match name {
            "status" => "status",
            "sources" => "sources",
            "search" => "search",
            "sql" => "sql",
            "show_session" => "show_session",
            "show_event" => "show_event",
            "pro_status" => "pro_status",
            "blame" => "blame",
            _ => return None,
        };
        Some(Self {
            operation,
            target_type: TargetType::NotApplicable,
            context_target: None,
        })
    }

    pub(crate) fn bind_context_target(&mut self, stable_result_id: String) {
        self.context_target = match self.operation {
            "show_session" => Some(McpContextTarget::Session(stable_result_id)),
            "show_event" => Some(McpContextTarget::Event(stable_result_id)),
            _ => None,
        };
    }

    fn participates_in_correlation(&self) -> bool {
        self.operation == "search" || self.context_target.is_some()
    }

    pub(crate) fn bind_blame_target(&mut self, target: &BlameTarget) {
        self.target_type = match target {
            BlameTarget::File { .. } => TargetType::File,
            BlameTarget::Commit { .. } => TargetType::Commit,
            BlameTarget::PullRequest { .. } => TargetType::PullRequest,
        };
    }

    #[cfg(test)]
    pub(crate) fn target_type_for_test(self) -> TargetType {
        self.target_type
    }

    pub(crate) fn completed(
        &self,
        response: &Value,
        duration: Duration,
        response_bytes: usize,
    ) -> CompletedOperation {
        let failed = response.get("error").is_some()
            || response.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
        let mut operation = CompletedOperation {
            surface: Surface::Mcp,
            operation: self.operation,
            outcome: if failed {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            value_class: ValueClass::NotApplicable,
            duration: DurationBucket::from_duration(duration),
            target_type: self.target_type,
            pro_outcome: if self.operation == "blame" && failed {
                ProOutcome::Error
            } else {
                ProOutcome::NotApplicable
            },
            result_count: 0,
            citation_count: 0,
            result_action: None,
            latency_ms: duration_millis(duration),
            latency_samples: 1,
            response_bytes: u64::try_from(response_bytes).unwrap_or(u64::MAX),
            response_byte_samples: 1,
            output_bytes: 0,
            output_byte_samples: 0,
            context_bytes: 0,
            context_byte_samples: 0,
            search_result_bytes: 0,
            search_result_byte_samples: 0,
            context: ContextUsage::default(),
        };
        if !failed {
            let structured = response.pointer("/result/structuredContent");
            operation.result_action = result_observation_action(self.operation);
            if operation.result_action.is_some() {
                if let Some(context_bytes) = mcp_semantic_context_bytes(response) {
                    operation.context_bytes = context_bytes;
                    operation.context_byte_samples = 1;
                }
            }
            let count = mcp_result_count(self.operation, structured);
            if let Some(count) = count {
                operation.value_class = if count == 0 {
                    ValueClass::Empty
                } else {
                    ValueClass::ResultBearing
                };
                operation.result_count = u64::try_from(count).unwrap_or(u64::MAX);
                if self.operation == "search" && count > 0 {
                    if let Some(search_result_bytes) = mcp_search_content_bytes(structured) {
                        operation.search_result_bytes = search_result_bytes;
                        operation.search_result_byte_samples = 1;
                    }
                }
            }
            if self.operation == "blame" {
                operation.citation_count = structured
                    .and_then(|value| value.get("evidence"))
                    .and_then(Value::as_array)
                    .map_or(0, |values| u64::try_from(values.len()).unwrap_or(u64::MAX));
                operation.pro_outcome = classify_blame_json(structured);
            }
        }
        operation
    }
}

fn mcp_search_context_targets(response: &Value) -> Vec<McpContextTarget> {
    response
        .pointer("/result/structuredContent/results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|result| {
                    match (
                        result.get("result_scope").and_then(Value::as_str),
                        result.get("result_type").and_then(Value::as_str),
                    ) {
                        (Some("session"), Some("session_result")) => result
                            .get("ctx_session_id")
                            .and_then(Value::as_str)
                            .map(|id| McpContextTarget::Session(id.to_owned())),
                        (Some("event"), Some("event")) => result
                            .get("ctx_event_id")
                            .and_then(Value::as_str)
                            .map(|id| McpContextTarget::Event(id.to_owned())),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn result_observation_action(operation: &str) -> Option<ResultObservationAction> {
    match operation {
        "search" => Some(ResultObservationAction::Search),
        "show_session" => Some(ResultObservationAction::OpenSession),
        "show_event" => Some(ResultObservationAction::OpenEvent),
        "sources" => Some(ResultObservationAction::Sources),
        "sql" => Some(ResultObservationAction::Sql),
        "blame" => Some(ResultObservationAction::Blame),
        _ => None,
    }
}

fn mcp_search_content_bytes(structured: Option<&Value>) -> Option<u64> {
    structured
        .and_then(|value| value.get("results"))
        .and_then(|results| serde_json::to_vec(results).ok())
        .and_then(|bytes| u64::try_from(bytes.len()).ok())
}

fn mcp_semantic_context_bytes(response: &Value) -> Option<u64> {
    let payload = response
        .pointer("/result/structuredContent")
        .or_else(|| response.get("result"))?;
    serde_json::to_vec(payload)
        .ok()
        .and_then(|bytes| u64::try_from(bytes.len()).ok())
}

fn mcp_result_count(operation: &str, structured: Option<&Value>) -> Option<usize> {
    let structured = structured?;
    let field = match operation {
        "sources" => "sources",
        "search" => "results",
        "sql" => "rows",
        "show_session" | "show_event" => "events",
        "blame" => "matches",
        _ => return None,
    };
    structured
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::len)
}

pub(crate) fn classify_blame(result: &BlameResult) -> ProOutcome {
    let mut possible = false;
    for item in &result.matches {
        match item {
            BlameMatch::File(value) => {
                for production in &value.production {
                    match classify_production(production.relationship, production.state) {
                        ProOutcome::Produced => return ProOutcome::Produced,
                        ProOutcome::Possible => possible = true,
                        ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                    }
                }
            }
            BlameMatch::Commit(value) => {
                match classify_commit_predicate(value.predicate, value.state) {
                    ProOutcome::Produced => return ProOutcome::Produced,
                    ProOutcome::Possible => possible = true,
                    ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                }
            }
            BlameMatch::PullRequest(value) => match &value.relationship {
                PullRequestBlameRelationship::Commit(commit) => {
                    // Commit membership is a reference-only result even when no
                    // asserted production attribution is present.
                    possible = true;
                    for production in &commit.production {
                        match classify_production(production.relationship, production.state) {
                            ProOutcome::Produced => return ProOutcome::Produced,
                            ProOutcome::Possible => possible = true,
                            ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                        }
                    }
                }
                PullRequestBlameRelationship::Activity(activity)
                    if matches!(activity.state, FactState::Asserted | FactState::Ambiguous) =>
                {
                    possible = true;
                }
                PullRequestBlameRelationship::Activity(_) => {}
            },
        }
    }
    if possible {
        ProOutcome::Possible
    } else {
        ProOutcome::None
    }
}

fn classify_blame_json(structured: Option<&Value>) -> ProOutcome {
    let Some(matches) = structured
        .and_then(|value| value.get("matches"))
        .and_then(Value::as_array)
    else {
        return ProOutcome::None;
    };
    let mut possible = false;
    for item in matches {
        match item.get("kind").and_then(Value::as_str) {
            Some("file") => {
                if production_outcome(item.pointer("/value/production"), &mut possible) {
                    return ProOutcome::Produced;
                }
            }
            Some("commit") => {
                let predicate = item
                    .pointer("/value/predicate")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<CommitPredicate>(value).ok());
                let state = item
                    .pointer("/value/state")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<FactState>(value).ok());
                if let (Some(predicate), Some(state)) = (predicate, state) {
                    match classify_commit_predicate(predicate, state) {
                        ProOutcome::Produced => return ProOutcome::Produced,
                        ProOutcome::Possible => possible = true,
                        ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
                    }
                }
            }
            Some("pull_request") => {
                let relationship = item.pointer("/value/relationship");
                match relationship
                    .and_then(|value| value.get("kind"))
                    .and_then(Value::as_str)
                {
                    Some("commit") => {
                        possible = true;
                        if production_outcome(
                            relationship.and_then(|value| value.pointer("/value/production")),
                            &mut possible,
                        ) {
                            return ProOutcome::Produced;
                        }
                    }
                    Some("activity")
                        if relationship
                            .and_then(|value| value.pointer("/value/state"))
                            .and_then(Value::as_str)
                            .is_some_and(|state| matches!(state, "asserted" | "ambiguous")) =>
                    {
                        possible = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if possible {
        ProOutcome::Possible
    } else {
        ProOutcome::None
    }
}

fn classify_commit_predicate(predicate: CommitPredicate, state: FactState) -> ProOutcome {
    match state {
        FactState::Asserted if predicate == CommitPredicate::ProducedBy => ProOutcome::Produced,
        FactState::Asserted | FactState::Ambiguous => ProOutcome::Possible,
        FactState::Contradicted | FactState::Superseded => ProOutcome::None,
    }
}

fn production_outcome(production: Option<&Value>, possible: &mut bool) -> bool {
    let Some(production) = production.and_then(Value::as_array) else {
        return false;
    };
    for attribution in production {
        let relationship = attribution
            .get("relationship")
            .cloned()
            .and_then(|value| serde_json::from_value::<ProductionRelationship>(value).ok());
        let state = attribution
            .get("state")
            .cloned()
            .and_then(|value| serde_json::from_value::<FactState>(value).ok());
        if let (Some(relationship), Some(state)) = (relationship, state) {
            match classify_production(relationship, state) {
                ProOutcome::Produced => return true,
                ProOutcome::Possible => *possible = true,
                ProOutcome::None | ProOutcome::Error | ProOutcome::NotApplicable => {}
            }
        }
    }
    false
}

fn classify_production(relationship: ProductionRelationship, state: FactState) -> ProOutcome {
    match (relationship, state) {
        (ProductionRelationship::ProducedBy, FactState::Asserted) => ProOutcome::Produced,
        (
            ProductionRelationship::ProducedBy | ProductionRelationship::PossiblyProducedBy,
            FactState::Asserted | FactState::Ambiguous,
        ) => ProOutcome::Possible,
        (
            ProductionRelationship::ProducedBy | ProductionRelationship::PossiblyProducedBy,
            FactState::Contradicted | FactState::Superseded,
        ) => ProOutcome::None,
    }
}
