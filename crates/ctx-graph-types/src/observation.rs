//! Content-free, run-local facts. Only the host decides whether/how to publish them.
//! Missing facts are unknown, not zero. These types intentionally are not serializable.
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphOperation {
    Parse,
    Index,
    Update,
    Watch,
    CheckUpdate,
    Add,
    Clone,
    Import,
    Compact,
    Search,
    Show,
    Callers,
    Callees,
    Impact,
    Path,
    Stats,
    Analyze,
    Communities,
    Hubs,
    Tree,
    Export,
    Report,
    Diagnose,
    Benchmark,
    Label,
    Merge,
    GlobalAdd,
    GlobalRemove,
    GlobalList,
    GlobalRefresh,
    GlobalSearch,
    GlobalPath,
    SaveResult,
    Reflect,
    Prs,
    Install,
    Uninstall,
    Hook,
    HookGuard,
    Switch,
    ProviderList,
    ProviderDetect,
    ProviderTemplate,
    ProviderSetup,
    ProviderShow,
    ProviderAdd,
    ProviderRemove,
    CacheInspect,
    CacheRemove,
    IntrospectPostgres,
    PushNeo4j,
    PushFalkorDb,
    Serve,
    QueryGraph,
    GetNode,
    GetNeighbors,
    ShortestPath,
    GraphStats,
    GodNodes,
    GetCommunity,
    ListPrs,
    GetPrImpact,
    TriagePrs,
    ResourceStats,
    ResourceGraph,
    ResourceReport,
    ResourceHubs,
    ResourceCommunities,
    ResourceComputedCommunities,
    ResourceSurprises,
    ResourceAudit,
    ResourceQuestions,
    Initialize,
    ToolsList,
    ResourcesList,
    Ping,
    Protocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphInvocation {
    Cli,
    ScopedSearch,
    UnifiedMcp,
    NativeStdio,
    NativeHttp,
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphPhase {
    Parse,
    Prepare,
    Discover,
    Open,
    Capture,
    Detect,
    Extract,
    Commit,
    PostCommit,
    Query,
    Snapshot,
    Analysis,
    Render,
    ArtifactWrite,
    OutputWrite,
    OutputFlush,
    Registration,
    Bind,
    Protocol,
    Admission,
    Worker,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFailureKind {
    InvalidInput,
    MissingIndex,
    NotFound,
    Permission,
    Io,
    StoreOpen,
    StoreBusy,
    InvalidStore,
    UnsupportedStore,
    ConcurrentChange,
    EndpointNotFound,
    EndpointAmbiguous,
    WorkLimit,
    ResponseLimit,
    InputRejected,
    UnknownProject,
    Capacity,
    Worker,
    Serialize,
    BrokenPipe,
    Authentication,
    Protocol,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphFailure {
    pub phase: GraphPhase,
    pub kind: GraphFailureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphPathDisposition {
    Found,
    NotFoundWithinScope,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphIndexDisposition {
    NoOp,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphConvergence {
    Converged,
    NotConverged,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphAlgorithm {
    Leiden,
    Louvain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphLifecycle {
    Ready,
    Stopped,
    StartFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphOutputBoundary {
    /// No writer completion was observed (including direct library callers).
    Unobserved,
    /// The CLI's stdout and stderr writes and final flushes completed.
    CliFlush,
    /// The SDK's framed stdio sink flushed this response to the OS.
    StdioFlush,
    /// The response body was fully yielded to the HTTP server. Not client receipt.
    HttpBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphExportFormat {
    SnapshotJson,
    GraphifyJson,
    GraphMl,
    Cypher,
    Mermaid,
    Svg,
    Html,
    Markdown,
    Canvas,
    CallflowHtml,
    TreeHtml,
    Wiki,
    Obsidian,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphBounds {
    pub seed: bool,
    pub node: bool,
    pub work: bool,
    pub depth: bool,
    pub unresolved: bool,
    pub token: bool,
    pub other: bool,
}

/// A known subtotal and its coverage, never a fabricated all-request total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphTokenUsage {
    pub known_sum: Option<u64>,
    pub reporting_receipts: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GraphSemanticFacts {
    pub configured: Option<bool>,
    /// Reserved generations, which may exceed actual invocations.
    pub reserved_generations: Option<u64>,
    pub reserved_output_tokens: Option<u64>,
    /// Recorded request attempts; absent when no recorder snapshot was obtained.
    pub receipts: Option<u64>,
    pub usage_unavailable: bool,
    pub input: GraphTokenUsage,
    pub output: GraphTokenUsage,
    pub total: GraphTokenUsage,
    pub cache_read: GraphTokenUsage,
    pub cache_create: GraphTokenUsage,
    pub reasoning: GraphTokenUsage,
    pub known_cost_usd: Option<f64>,
    pub cost_reporting_receipts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphObservation {
    pub operation: GraphOperation,
    pub invocation: GraphInvocation,
    pub phase: GraphPhase,
    pub failure: Option<GraphFailure>,
    /// Operation result before delivery; None if not observed to completion.
    pub execution_succeeded: Option<bool>,
    /// Includes a successfully delivered error envelope; not product success.
    pub output_served: Option<bool>,
    /// Writer/transport failure, independent of the engine failure.
    pub output_failure: Option<GraphFailureKind>,
    pub output_boundary: GraphOutputBoundary,
    /// Elapsed time for this invocation/ready milestone, absent until measured.
    pub duration: Option<Duration>,
    pub query_duration: Option<Duration>,
    pub detect_duration: Option<Duration>,
    pub extract_duration: Option<Duration>,
    pub commit_duration: Option<Duration>,
    pub capture_duration: Option<Duration>,
    pub analysis_duration: Option<Duration>,
    pub output_duration: Option<Duration>,
    pub nodes: Option<u64>,
    pub edges: Option<u64>,
    pub unresolved: Option<u64>,
    pub seeds: Option<u64>,
    pub estimated_json_tokens: Option<u64>,
    pub truncated: Option<bool>,
    pub bounds: Option<GraphBounds>,
    pub path: Option<GraphPathDisposition>,
    pub parsed_files: Option<u64>,
    pub rejected_files: Option<u64>,
    pub unchanged_files: Option<u64>,
    pub deleted_files: Option<u64>,
    pub diagnostics: Option<u64>,
    pub index: Option<GraphIndexDisposition>,
    pub fresh: Option<bool>,
    pub communities: Option<u64>,
    pub result_count: Option<u64>,
    pub algorithm: Option<GraphAlgorithm>,
    pub pagerank_converged: Option<bool>,
    pub community_convergence: Option<GraphConvergence>,
    pub community_passes: Option<u64>,
    pub unsatisfied_constraints: Option<u64>,
    pub snapshot_cache_hit: Option<bool>,
    pub analysis_cache_hit: Option<bool>,
    pub artifact_committed: Option<bool>,
    pub artifact_bytes: Option<u64>,
    pub export_format: Option<GraphExportFormat>,
    pub semantic: GraphSemanticFacts,
    pub lifecycle: Option<GraphLifecycle>,
    pub polls: Option<u64>,
    pub retries: Option<u64>,
}

impl GraphObservation {
    pub fn new(operation: GraphOperation, invocation: GraphInvocation) -> Self {
        Self {
            operation,
            invocation,
            phase: GraphPhase::Prepare,
            failure: None,
            execution_succeeded: None,
            output_served: None,
            output_failure: None,
            output_boundary: GraphOutputBoundary::Unobserved,
            duration: None,
            query_duration: None,
            detect_duration: None,
            extract_duration: None,
            commit_duration: None,
            capture_duration: None,
            analysis_duration: None,
            output_duration: None,
            nodes: None,
            edges: None,
            unresolved: None,
            seeds: None,
            estimated_json_tokens: None,
            truncated: None,
            bounds: None,
            path: None,
            parsed_files: None,
            rejected_files: None,
            unchanged_files: None,
            deleted_files: None,
            diagnostics: None,
            index: None,
            fresh: None,
            communities: None,
            result_count: None,
            algorithm: None,
            pagerank_converged: None,
            community_convergence: None,
            community_passes: None,
            unsatisfied_constraints: None,
            snapshot_cache_hit: None,
            analysis_cache_hit: None,
            artifact_committed: None,
            artifact_bytes: None,
            export_format: None,
            semantic: GraphSemanticFacts::default(),
            lifecycle: None,
            polls: None,
            retries: None,
        }
    }

    pub fn fail(&mut self, kind: GraphFailureKind) {
        self.failure = Some(GraphFailure {
            phase: self.phase,
            kind,
        });
        self.execution_succeeded = Some(false);
    }

    pub fn graph(&mut self, graph: &crate::GraphResult) {
        self.nodes = Some(graph.nodes.len() as u64);
        self.edges = Some(graph.edges.len() as u64);
        self.unresolved = Some(graph.unresolved.len() as u64);
        self.truncated = Some(graph.truncated);
    }

    pub fn stats(&mut self, stats: &crate::Stats) {
        self.nodes = Some(stats.nodes as u64);
        self.edges = Some(stats.edges as u64);
        self.unresolved = Some(stats.unresolved_references as u64);
        self.diagnostics = Some(stats.diagnostics.len() as u64);
    }

    pub fn index_report(&mut self, report: &crate::IndexReport) {
        self.parsed_files = Some(report.parsed_files as u64);
        self.rejected_files = Some(report.rejected_files as u64);
        self.unchanged_files = Some(report.unchanged_files as u64);
        self.deleted_files = Some(report.deleted_files as u64);
        self.nodes = Some(report.nodes as u64);
        self.edges = Some(report.edges as u64);
        self.diagnostics = Some(report.diagnostics.len() as u64);
        self.semantic
            .record(report.semantic_usage, report.provider_usage.as_deref());
    }
}

impl GraphSemanticFacts {
    pub fn record(
        &mut self,
        reserved: Option<crate::SemanticUsage>,
        receipts: Option<&[crate::ProviderUsage]>,
    ) {
        let configured = self.configured;
        let unavailable = self.usage_unavailable;
        *self = Self {
            configured,
            usage_unavailable: unavailable,
            ..Self::default()
        };
        self.reserved_generations = reserved.map(|value| value.calls as u64);
        self.reserved_output_tokens = reserved.map(|value| value.reserved_output_tokens);
        self.receipts = receipts.map(|values| values.len() as u64);
        let Some(receipts) = receipts else { return };
        let sum = |get: fn(&crate::ProviderUsage) -> Option<u64>| {
            let count = receipts.iter().filter_map(get).count() as u64;
            GraphTokenUsage {
                known_sum: (count > 0)
                    .then(|| {
                        receipts
                            .iter()
                            .filter_map(get)
                            .try_fold(0u64, u64::checked_add)
                    })
                    .flatten(),
                reporting_receipts: count,
            }
        };
        self.input = sum(|r| r.input_tokens);
        self.output = sum(|r| r.output_tokens);
        self.total = sum(|r| r.total_tokens);
        self.cache_read = sum(|r| r.cache_read_input_tokens);
        self.cache_create = sum(|r| r.cache_creation_input_tokens);
        self.reasoning = sum(|r| r.reasoning_tokens);
        let costs = || {
            receipts
                .iter()
                .filter_map(|r| r.cost_usd)
                .filter(|v| v.is_finite() && *v >= 0.0)
        };
        self.cost_reporting_receipts = costs().count() as u64;
        let total: f64 = costs().sum();
        self.known_cost_usd =
            (self.cost_reporting_receipts > 0 && total.is_finite()).then_some(total);
    }
}

#[cfg(test)]
pub(crate) mod tests;
