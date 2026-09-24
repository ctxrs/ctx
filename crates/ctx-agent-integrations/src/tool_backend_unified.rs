//! Typed requests for optional local graph and output backends.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedToolKind {
    GraphQuery,
    GraphShow,
    GraphCallers,
    GraphCallees,
    GraphImpact,
    GraphPath,
    GraphStats,
    OutputCompact,
    OutputRestore,
}

impl UnifiedToolKind {
    pub const ALL: [Self; 9] = [
        Self::GraphQuery,
        Self::GraphShow,
        Self::GraphCallers,
        Self::GraphCallees,
        Self::GraphImpact,
        Self::GraphPath,
        Self::GraphStats,
        Self::OutputCompact,
        Self::OutputRestore,
    ];

    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::GraphQuery => "graph_query",
            Self::GraphShow => "graph_show",
            Self::GraphCallers => "graph_callers",
            Self::GraphCallees => "graph_callees",
            Self::GraphImpact => "graph_impact",
            Self::GraphPath => "graph_path",
            Self::GraphStats => "graph_stats",
            Self::OutputCompact => "output_compact",
            Self::OutputRestore => "output_restore",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedToolOperation {
    Graph(GraphOperation),
    OutputCompact { text: String },
    OutputRestore { text: String, encoding: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphOperation {
    Query {
        query: String,
        options: GraphOptions,
    },
    Show {
        symbol: String,
    },
    Callers {
        symbol: String,
        options: GraphOptions,
    },
    Callees {
        symbol: String,
        options: GraphOptions,
    },
    Impact {
        symbol: String,
        options: GraphOptions,
    },
    Path {
        source: String,
        target: String,
        options: GraphOptions,
    },
    Stats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    Incoming,
    Outgoing,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphOptions {
    pub depth: u32,
    pub limit: usize,
    pub direction: GraphDirection,
    pub relation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedErrorCode {
    Unsupported,
    GraphUnavailable,
    GraphQuery,
    OutputDecode,
    OutputLimit,
}

impl UnifiedErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported_tool",
            Self::GraphUnavailable => "graph_unavailable",
            Self::GraphQuery => "graph_query_failed",
            Self::OutputDecode => "output_decode_failed",
            Self::OutputLimit => "output_limit_exceeded",
        }
    }
}

pub const MAX_OUTPUT_INPUT_BYTES: usize = 256 * 1024;
pub const MAX_OUTPUT_TEXT_BYTES: usize = 1024 * 1024;
pub const OUTPUT_ENCODINGS: &[&str] = &[
    "raw",
    "json-v1",
    "json-rows-v1",
    "json-min-v1",
    "json-columns-v1",
    "text-runs-v1",
    "text-prefixes-v1",
    "text-refs-v1",
    "text-lines-v1",
    "text-symbols-v1",
];
