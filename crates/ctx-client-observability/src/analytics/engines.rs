//! Closed engine observations. Exact measurements stay local until serialization.
use super::{duration_bucket, Outcome, PublicEventV1, Surface};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;

mod summary;
mod wire;
pub use summary::*;

macro_rules! vocabulary {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }
        impl $name {
            pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
        }
    };
}
vocabulary!(GraphOperation {
    Index => "index", Update => "update", CheckUpdate => "check_update", Compact => "compact",
    Watch => "watch", Add => "add", Import => "import", Clone => "clone", Search => "search",
    Show => "show", Callers => "callers", Callees => "callees", Impact => "impact", Path => "path",
    Stats => "stats", Analyze => "analyze", Communities => "communities", Hubs => "hubs",
    Diagnose => "diagnose", Benchmark => "benchmark", Label => "label", Tree => "tree",
    Report => "report", Export => "export", Merge => "merge", Global => "global",
    SaveResult => "save_result", Reflect => "reflect", Prs => "prs", Provider => "provider",
    Cache => "cache", Install => "install", Uninstall => "uninstall", Hook => "hook",
    HookGuard => "hook_guard", Switch => "switch", Introspect => "introspect", Push => "push",
    InvalidRequest => "invalid_request", Parse => "parse", GlobalAdd => "global_add", GlobalRemove => "global_remove", GlobalList => "global_list", GlobalRefresh => "global_refresh", GlobalSearch => "global_search", GlobalPath => "global_path", ProviderList => "provider_list", ProviderDetect => "provider_detect", ProviderTemplate => "provider_template", ProviderSetup => "provider_setup", ProviderShow => "provider_show", ProviderAdd => "provider_add", ProviderRemove => "provider_remove", CacheInspect => "cache_inspect", CacheRemove => "cache_remove", IntrospectPostgres => "introspect_postgres", PushNeo4j => "push_neo4j", PushFalkorDb => "push_falkor_db", Serve => "serve", QueryGraph => "query_graph", GetNode => "get_node", GetNeighbors => "get_neighbors", ShortestPath => "shortest_path", GraphStats => "graph_stats", GodNodes => "god_nodes", GetCommunity => "get_community", ListPrs => "list_prs", GetPrImpact => "get_pr_impact", TriagePrs => "triage_prs", ResourceStats => "resource_stats", ResourceGraph => "resource_graph", ResourceReport => "resource_report", ResourceHubs => "resource_hubs", ResourceCommunities => "resource_communities", ResourceComputedCommunities => "resource_computed_communities", ResourceSurprises => "resource_surprises", ResourceAudit => "resource_audit", ResourceQuestions => "resource_questions", Initialize => "initialize", ToolsList => "tools_list", ResourcesList => "resources_list", Ping => "ping", Protocol => "protocol"
});
vocabulary!(SiftOperation {
    Compact => "compact", Restore => "restore", Run => "run", Proxy => "proxy", Filter => "filter",
    Read => "read", Json => "json", Summary => "summary", Err => "err", Test => "test",
    Gain => "gain", Config => "config", Semantic => "semantic", Discover => "discover",
    Ccusage => "ccusage", Hook => "hook", Rewrite => "rewrite", Recall => "recall",
    Unknown => "unknown", Help => "help", Version => "version", Errors => "errors", Usage => "usage"
});
vocabulary!(SiftMode {
    Lossless => "lossless", Presentation => "presentation", Raw => "raw", Streaming => "streaming",
    Semantic => "semantic", Restore => "restore", NotApplicable => "not_applicable",
    Automatic => "automatic", Capture => "capture", ExplicitView => "explicit_view", Rewrite => "rewrite", Control => "control"
});
vocabulary!(SiftHost {
    Unknown => "unknown", Standalone => "standalone", Claude => "claude", Codex => "codex",
    Cursor => "cursor", Copilot => "copilot", Gemini => "gemini", Hermes => "hermes",
    Vscode => "vscode", Droid => "droid", Vibe => "vibe", Pi => "pi",
    Omp => "omp", OpenCode => "open_code", Kilo => "kilo"
});
vocabulary!(SiftOutcome { Success => "success", Failure => "failure", Mixed => "mixed" });
vocabulary!(ServerOperation {
    Enroll => "enroll", Principals => "principals", Credentials => "credentials",
    RevokePrincipal => "revoke_principal", RevokeCredential => "revoke_credential", Whoami => "whoami",
    Invite => "invite", Grants => "grants", Revoke => "revoke", BeginUpload => "begin_upload",
    UploadStatus => "upload_status", UploadChunk => "upload_chunk", Publish => "publish",
    CancelPublish => "cancel_publish", Withdraw => "withdraw", Remove => "remove", Receipt => "receipt",
    Publications => "publications", PublicationState => "publication_state", Status => "status",
    Search => "search", Event => "event", Session => "session", Unmatched => "unmatched",
    IndexerHealth => "indexer_health", Health => "health", RevokeMember => "revoke_member", Publication => "publication", Unknown => "unknown"
});
vocabulary!(ProductFailureClass {
    InvalidRequest => "invalid_request", NotFound => "not_found", Permission => "permission",
    Unauthorized => "unauthorized", Forbidden => "forbidden", Conflict => "conflict",
    Capacity => "capacity", Timeout => "timeout", Io => "io", Store => "store",
    Unsupported => "unsupported", Cancelled => "cancelled", Other => "other"
});
vocabulary!(ProductFailureStage {
    Parse => "parse", Prepare => "prepare", Execute => "execute", Render => "render", Output => "output"
});
vocabulary!(DeliveryEvidence {
    KnownComplete => "known_complete", Failed => "failed", Unknown => "unknown", NotAttempted => "not_attempted"
});
vocabulary!(ProductOutput {
    Human => "human", Json => "json", Bytes => "bytes", Mcp => "mcp", Http => "http", None => "none"
});
vocabulary!(ResponseClass { Unavailable => "unavailable", Other => "other",
    Success => "2xx", Redirect => "3xx", ClientError => "4xx", ServerError => "5xx", NotProduced => "not_produced"
});
vocabulary!(ProductRuntimeKind { Server => "server", GraphServe => "graph_serve", GraphWatch => "graph_watch" });
vocabulary!(ProductRuntimePhase { ShuttingDown => "shutting_down", Recovered => "recovered", Ready => "ready", Liveness => "liveness", Stopped => "stopped", Failed => "failed" });

mod remote;
pub use remote::*;
mod graph_facts;
mod sift_cohorts;
mod windows;
pub use graph_facts::*;
pub use sift_cohorts::*;
pub use windows::*;

/// Separate from the legacy envelope's coarse duration vocabulary.
pub const NATIVE_DURATION_BUCKETS: [&str; 14] = [
    "lt_1ms",
    "1ms-5ms",
    "5ms-10ms",
    "10ms-25ms",
    "25ms-50ms",
    "50ms-100ms",
    "100ms-250ms",
    "250ms-1s",
    "1s-5s",
    "5s-30s",
    "30s-2m",
    "2m-10m",
    "10m-1h",
    "1h+",
];
pub fn native_duration_index(duration: Duration) -> usize {
    [
        1, 5, 10, 25, 50, 100, 250, 1000, 5000, 30000, 120000, 600000, 3600000,
    ]
    .into_iter()
    .position(|ms| duration < Duration::from_millis(ms))
    .unwrap_or(13)
}
pub fn native_duration_bucket(duration: Duration) -> &'static str {
    NATIVE_DURATION_BUCKETS[native_duration_index(duration)]
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductFailure {
    pub stage: ProductFailureStage,
    pub class: ProductFailureClass,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProductTimings {
    pub prepare: Option<Duration>,
    pub work: Option<Duration>,
    pub output: Option<Duration>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductCompletion {
    pub elapsed: Duration,
    pub execution: Result<(), ProductFailure>,
    pub delivery: DeliveryEvidence,
    pub output: ProductOutput,
    pub timings: ProductTimings,
}
impl ProductCompletion {
    pub fn new(
        elapsed: Duration,
        execution: Result<(), ProductFailure>,
        delivery: DeliveryEvidence,
        output: ProductOutput,
    ) -> Self {
        Self {
            elapsed,
            execution,
            delivery,
            output,
            timings: ProductTimings::default(),
        }
    }
    pub fn outcome(self) -> Outcome {
        if self.execution.is_err() || self.delivery == DeliveryEvidence::Failed {
            Outcome::Failure
        } else {
            Outcome::Success
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductResultFacts {
    pub count: u64,
    pub truncated: Option<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSurface {
    Cli,
    Mcp,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphCompletedV1 {
    pub operation: GraphOperation,
    pub surface: GraphSurface,
    pub completion: ProductCompletion,
    pub result: Option<ProductResultFacts>,
    pub nodes: Option<u64>,
    pub edges: Option<u64>,
    pub files_processed: Option<u64>,
    pub details: GraphDetails,
}
impl GraphCompletedV1 {
    pub fn new(
        operation: GraphOperation,
        surface: GraphSurface,
        completion: ProductCompletion,
    ) -> Self {
        Self {
            operation,
            surface,
            completion,
            result: None,
            nodes: None,
            edges: None,
            files_processed: None,
            details: GraphDetails::default(),
        }
    }
    pub fn into_event(self) -> PublicEventV1 {
        PublicEventV1::GraphCompleted(self)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerRequestCompletedV1 {
    pub operation: ServerOperation,
    pub completion: ProductCompletion,
    pub response_class: ResponseClass,
    pub response_handoff: DeliveryEvidence,
    pub result: Option<ProductResultFacts>,
}
impl ServerRequestCompletedV1 {
    /// Returning a Response does not prove transport handoff: default is unknown.
    pub fn new(
        operation: ServerOperation,
        mut completion: ProductCompletion,
        response_class: ResponseClass,
    ) -> Self {
        completion.output = ProductOutput::Http;
        completion.delivery = DeliveryEvidence::Unknown;
        Self {
            operation,
            completion,
            response_class,
            response_handoff: DeliveryEvidence::Unknown,
            result: None,
        }
    }
    pub fn into_event(mut self) -> PublicEventV1 {
        self.completion.delivery = self.response_handoff;
        // An HTTP error response is not successful product execution.
        if self.completion.execution.is_ok()
            && matches!(
                self.response_class,
                ResponseClass::ClientError
                    | ResponseClass::ServerError
                    | ResponseClass::NotProduced
                    | ResponseClass::Unavailable
            )
        {
            self.completion.execution = Err(ProductFailure {
                stage: ProductFailureStage::Execute,
                class: ProductFailureClass::Other,
            });
        }
        PublicEventV1::ServerRequestCompleted(self)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductRuntimeV1 {
    pub kind: ProductRuntimeKind,
    pub phase: ProductRuntimePhase,
    pub uptime: Duration,
    pub failure: Option<ProductFailure>,
    pub active_requests: Option<u64>,
    pub pending_work: Option<u64>,
}
impl ProductRuntimeV1 {
    pub fn new(kind: ProductRuntimeKind, phase: ProductRuntimePhase, uptime: Duration) -> Self {
        Self {
            kind,
            phase,
            uptime,
            failure: None,
            active_requests: None,
            pending_work: None,
        }
    }
    pub fn into_event(mut self) -> PublicEventV1 {
        if self.phase == ProductRuntimePhase::Failed {
            self.failure.get_or_insert(ProductFailure {
                stage: ProductFailureStage::Execute,
                class: ProductFailureClass::Other,
            });
        } else {
            self.failure = None;
        }
        PublicEventV1::ProductRuntime(self)
    }
}

pub(super) fn insert_hosted_measurements(
    p: &mut Map<String, Value>,
    facts: super::HostedMeasurements,
) {
    wire::timings(p, facts.elapsed, facts.timings);
    p.insert(
        "output_delivery".into(),
        serde_json::json!(facts.delivery.as_str()),
    );
    wire::result(p, facts.result);
}

pub(super) type EngineWire = (
    &'static str,
    Surface,
    &'static str,
    Outcome,
    super::DurationBucket,
    Map<String, Value>,
);
pub(super) fn graph_wire(event: &GraphCompletedV1) -> EngineWire {
    wire::graph(event)
}
pub(super) fn server_wire(event: &ServerRequestCompletedV1) -> EngineWire {
    wire::server(event)
}
pub(super) fn runtime_wire(event: &ProductRuntimeV1) -> EngineWire {
    wire::runtime(event)
}
pub(super) fn summary_wire(event: &SiftSummaryV1) -> EngineWire {
    (
        "runtime_observation",
        if event.cohort.entry == Some(SiftEntry::Mcp) {
            Surface::Mcp
        } else {
            Surface::Cli
        },
        "sift_summary",
        Outcome::Success,
        duration_bucket(event.window),
        event.properties(),
    )
}
#[cfg(test)]
mod tests;

pub(super) fn server_summary_wire(e: &ServerSummaryV1) -> EngineWire {
    (
        "runtime_observation",
        Surface::Server,
        "server_summary",
        Outcome::Success,
        duration_bucket(e.window),
        e.properties(),
    )
}
pub(super) fn sharing_summary_wire(e: &SharingSummaryV1) -> EngineWire {
    (
        "runtime_observation",
        Surface::Daemon,
        "sharing_summary",
        Outcome::Success,
        duration_bucket(e.window),
        e.properties(),
    )
}

pub(super) fn remote_wire(e: &RemoteCompletedV1) -> EngineWire {
    remote::wire(e)
}
