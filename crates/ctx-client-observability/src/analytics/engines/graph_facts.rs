use super::wire::count;
use super::*;
use serde_json::json;
vocabulary!(GraphInvocation {
    Cli => "cli", ScopedSearch => "scoped_search", UnifiedMcp => "unified_mcp", NativeStdio => "native_stdio", NativeHttp => "native_http", Library => "library"
});
vocabulary!(GraphPhase {
    Parse => "parse", Prepare => "prepare", Discover => "discover", Open => "open", Capture => "capture", Detect => "detect", Extract => "extract", Commit => "commit", PostCommit => "post_commit", Query => "query", Snapshot => "snapshot", Analysis => "analysis", Render => "render", ArtifactWrite => "artifact_write", OutputWrite => "output_write", OutputFlush => "output_flush", Registration => "registration", Bind => "bind", Protocol => "protocol", Admission => "admission", Worker => "worker", Shutdown => "shutdown"
});
vocabulary!(GraphFailureKind {
    InvalidInput => "invalid_input", MissingIndex => "missing_index", NotFound => "not_found", Permission => "permission", Io => "io", StoreOpen => "store_open", StoreBusy => "store_busy", InvalidStore => "invalid_store", UnsupportedStore => "unsupported_store", ConcurrentChange => "concurrent_change", EndpointNotFound => "endpoint_not_found", EndpointAmbiguous => "endpoint_ambiguous", WorkLimit => "work_limit", ResponseLimit => "response_limit", InputRejected => "input_rejected", UnknownProject => "unknown_project", Capacity => "capacity", Worker => "worker", Serialize => "serialize", BrokenPipe => "broken_pipe", Authentication => "authentication", Protocol => "protocol", Unknown => "unknown"
});
vocabulary!(GraphPathDisposition {
    Found => "found", NotFoundWithinScope => "not_found_within_scope", Incomplete => "incomplete"
});
vocabulary!(GraphIndexDisposition {
    NoOp => "no_op", Committed => "committed"
});
vocabulary!(GraphConvergence {
    Converged => "converged", NotConverged => "not_converged", Unknown => "unknown"
});
vocabulary!(GraphAlgorithm {
    Leiden => "leiden", Louvain => "louvain"
});
vocabulary!(GraphOutputBoundary {
    Unobserved => "unobserved", CliFlush => "cli_flush", StdioFlush => "stdio_flush", HttpBody => "http_body"
});
vocabulary!(GraphExportFormat {
    SnapshotJson => "snapshot_json", GraphifyJson => "graphify_json", GraphMl => "graph_ml", Cypher => "cypher", Mermaid => "mermaid", Svg => "svg", Html => "html", Markdown => "markdown", Canvas => "canvas", CallflowHtml => "callflow_html", TreeHtml => "tree_html", Wiki => "wiki", Obsidian => "obsidian"
});

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphFailureFacts {
    pub phase: GraphPhase,
    pub kind: GraphFailureKind,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphQueryFacts {
    pub bounds: Option<GraphBounds>,
    pub path: Option<GraphPathDisposition>,
    pub unresolved: Option<u64>,
    pub seeds: Option<u64>,
    pub duration: Option<Duration>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphIndexFacts {
    pub disposition: Option<GraphIndexDisposition>,
    pub fresh: Option<bool>,
    pub parsed: Option<u64>,
    pub rejected: Option<u64>,
    pub unchanged: Option<u64>,
    pub deleted: Option<u64>,
    pub diagnostics: Option<u64>,
    pub capture: Option<Duration>,
    pub detect: Option<Duration>,
    pub extract: Option<Duration>,
    pub commit: Option<Duration>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphAnalysisFacts {
    pub algorithm: Option<GraphAlgorithm>,
    pub communities: Option<u64>,
    pub pagerank_converged: Option<bool>,
    pub convergence: Option<GraphConvergence>,
    pub passes: Option<u64>,
    pub unsatisfied_constraints: Option<u64>,
    pub duration: Option<Duration>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphArtifactFacts {
    pub snapshot_cache_hit: Option<bool>,
    pub analysis_cache_hit: Option<bool>,
    pub committed: Option<bool>,
    pub bytes: Option<u64>,
    pub format: Option<GraphExportFormat>,
}
/// A known subtotal and the number of receipts reporting it; absent is not zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphTokenUsage {
    pub known_sum: Option<u64>,
    pub reporting_receipts: u64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphSemanticFacts {
    pub configured: Option<bool>,
    pub reserved_generations: Option<u64>,
    pub reserved_output_tokens: Option<u64>,
    pub receipts: Option<u64>,
    pub usage_unavailable: bool,
    pub input: GraphTokenUsage,
    pub output: GraphTokenUsage,
    pub total: GraphTokenUsage,
    pub cache_read: GraphTokenUsage,
    pub cache_create: GraphTokenUsage,
    pub reasoning: GraphTokenUsage,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphDetails {
    pub invocation: Option<GraphInvocation>,
    pub phase: Option<GraphPhase>,
    pub failure: Option<GraphFailureFacts>,
    pub output_boundary: Option<GraphOutputBoundary>,
    pub query: Option<GraphQueryFacts>,
    pub index: Option<GraphIndexFacts>,
    pub analysis: Option<GraphAnalysisFacts>,
    pub artifact: Option<GraphArtifactFacts>,
    pub semantic: Option<GraphSemanticFacts>,
    pub polls: Option<u64>,
    pub retries: Option<u64>,
}
impl GraphDetails {
    pub(super) fn insert(self, p: &mut Map<String, Value>) {
        if let Some(v) = self.invocation {
            p.insert("graph_invocation".into(), json!(v.as_str()));
        }
        if let Some(v) = self.phase {
            p.insert("graph_phase".into(), json!(v.as_str()));
        }
        if let Some(v) = self.output_boundary {
            p.insert("graph_output_boundary".into(), json!(v.as_str()));
        }
        if let Some(f) = self.failure {
            p.insert("graph_failure_phase".into(), json!(f.phase.as_str()));
            p.insert("graph_failure_kind".into(), json!(f.kind.as_str()));
        }
        count(p, "graph_polls_count_bucket", self.polls);
        count(p, "graph_retries_count_bucket", self.retries);
        if let Some(f) = self.query {
            if let Some(v) = f.path {
                p.insert("graph_query_path".into(), json!(v.as_str()));
            }
            count(p, "graph_query_unresolved_count_bucket", f.unresolved);
            count(p, "graph_query_seeds_count_bucket", f.seeds);
            if let Some(v) = f.duration {
                p.insert(
                    "graph_query_duration_bucket".into(),
                    json!(native_duration_bucket(v)),
                );
            }
            if let Some(b) = f.bounds {
                p.insert("graph_bound_seed".into(), json!(b.seed));
                p.insert("graph_bound_node".into(), json!(b.node));
                p.insert("graph_bound_work".into(), json!(b.work));
                p.insert("graph_bound_depth".into(), json!(b.depth));
                p.insert("graph_bound_unresolved".into(), json!(b.unresolved));
                p.insert("graph_bound_token".into(), json!(b.token));
                p.insert("graph_bound_other".into(), json!(b.other));
            }
        }
        if let Some(f) = self.index {
            if let Some(v) = f.disposition {
                p.insert("graph_index_disposition".into(), json!(v.as_str()));
            }
            if let Some(v) = f.fresh {
                p.insert("graph_index_fresh".into(), json!(v));
            }
            count(p, "graph_index_parsed_count_bucket", f.parsed);
            count(p, "graph_index_rejected_count_bucket", f.rejected);
            count(p, "graph_index_unchanged_count_bucket", f.unchanged);
            count(p, "graph_index_deleted_count_bucket", f.deleted);
            count(p, "graph_index_diagnostics_count_bucket", f.diagnostics);
            if let Some(v) = f.capture {
                p.insert(
                    "graph_index_capture_duration_bucket".into(),
                    json!(native_duration_bucket(v)),
                );
            }
            if let Some(v) = f.detect {
                p.insert(
                    "graph_index_detect_duration_bucket".into(),
                    json!(native_duration_bucket(v)),
                );
            }
            if let Some(v) = f.extract {
                p.insert(
                    "graph_index_extract_duration_bucket".into(),
                    json!(native_duration_bucket(v)),
                );
            }
            if let Some(v) = f.commit {
                p.insert(
                    "graph_index_commit_duration_bucket".into(),
                    json!(native_duration_bucket(v)),
                );
            }
        }
        if let Some(f) = self.analysis {
            if let Some(v) = f.algorithm {
                p.insert("graph_analysis_algorithm".into(), json!(v.as_str()));
            }
            count(p, "graph_analysis_communities_count_bucket", f.communities);
            if let Some(v) = f.pagerank_converged {
                p.insert("graph_analysis_pagerank_converged".into(), json!(v));
            }
            if let Some(v) = f.convergence {
                p.insert("graph_analysis_convergence".into(), json!(v.as_str()));
            }
            count(p, "graph_analysis_passes_count_bucket", f.passes);
            count(
                p,
                "graph_analysis_unsatisfied_constraints_count_bucket",
                f.unsatisfied_constraints,
            );
            if let Some(v) = f.duration {
                p.insert(
                    "graph_analysis_duration_bucket".into(),
                    json!(native_duration_bucket(v)),
                );
            }
        }
        if let Some(f) = self.artifact {
            if let Some(v) = f.snapshot_cache_hit {
                p.insert("graph_artifact_snapshot_cache_hit".into(), json!(v));
            }
            if let Some(v) = f.analysis_cache_hit {
                p.insert("graph_artifact_analysis_cache_hit".into(), json!(v));
            }
            if let Some(v) = f.committed {
                p.insert("graph_artifact_committed".into(), json!(v));
            }
            if let Some(v) = f.bytes {
                p.insert(
                    "graph_artifact_bytes_bucket".into(),
                    json!(crate::analytics::bytes_bucket(v).as_str()),
                );
            }
            if let Some(v) = f.format {
                p.insert("graph_artifact_format".into(), json!(v.as_str()));
            }
        }
        if let Some(f) = self.semantic {
            if let Some(v) = f.configured {
                p.insert("graph_semantic_configured".into(), json!(v));
            }
            count(
                p,
                "graph_semantic_reserved_generations_count_bucket",
                f.reserved_generations,
            );
            count(
                p,
                "graph_semantic_reserved_output_tokens_count_bucket",
                f.reserved_output_tokens,
            );
            count(p, "graph_semantic_receipts_count_bucket", f.receipts);
            p.insert(
                "graph_semantic_usage_unavailable".into(),
                json!(f.usage_unavailable),
            );
            count(
                p,
                "graph_semantic_input_known_count_bucket",
                f.input.known_sum,
            );
            count(
                p,
                "graph_semantic_input_reporters_count_bucket",
                Some(f.input.reporting_receipts),
            );
            count(
                p,
                "graph_semantic_output_known_count_bucket",
                f.output.known_sum,
            );
            count(
                p,
                "graph_semantic_output_reporters_count_bucket",
                Some(f.output.reporting_receipts),
            );
            count(
                p,
                "graph_semantic_total_known_count_bucket",
                f.total.known_sum,
            );
            count(
                p,
                "graph_semantic_total_reporters_count_bucket",
                Some(f.total.reporting_receipts),
            );
            count(
                p,
                "graph_semantic_cache_read_known_count_bucket",
                f.cache_read.known_sum,
            );
            count(
                p,
                "graph_semantic_cache_read_reporters_count_bucket",
                Some(f.cache_read.reporting_receipts),
            );
            count(
                p,
                "graph_semantic_cache_create_known_count_bucket",
                f.cache_create.known_sum,
            );
            count(
                p,
                "graph_semantic_cache_create_reporters_count_bucket",
                Some(f.cache_create.reporting_receipts),
            );
            count(
                p,
                "graph_semantic_reasoning_known_count_bucket",
                f.reasoning.known_sum,
            );
            count(
                p,
                "graph_semantic_reasoning_reporters_count_bucket",
                Some(f.reasoning.reporting_receipts),
            );
        }
    }
}
