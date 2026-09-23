use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub file: String,
    pub line: Option<u32>,
    pub end_line: Option<u32>,
    pub qualified_name: Option<String>,
    pub binding_key: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub relation: String,
    pub directed: bool,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub confidence: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub id: String,
    pub source: String,
    pub label: String,
    pub relation: String,
    pub file: String,
    pub line: u32,
    pub candidate_keys: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: String,
    pub line: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFacts {
    pub path: String,
    pub hash: String,
    pub module: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub references: Vec<Reference>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStamp {
    pub path: String,
    pub hash: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Coverage {
    pub supported_files: usize,
    pub unsupported_files: usize,
    pub unchanged_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexReport {
    pub schema_version: u32,
    pub generation: u64,
    pub parsed_files: usize,
    pub unchanged_files: usize,
    pub deleted_files: usize,
    pub nodes: usize,
    pub edges: usize,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_usage: Option<crate::ingest::SemanticUsage>,
    /// Provider-reported usage per attempted call; absent counters are unknown.
    /// These are distinct from the maximum-token reservations above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_usage: Option<Vec<crate::ingest::ProviderUsage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<IndexTimings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexTimings {
    pub detect_ms: f64,
    pub extract_ms: f64,
    pub commit_ms: f64,
    pub total_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub schema_version: u32,
    pub generation: u64,
    pub kind: String,
    pub root: Option<String>,
    pub nodes: usize,
    pub edges: usize,
    pub files: usize,
    pub unresolved_references: usize,
    pub coverage: Coverage,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Incoming,
    Outgoing,
    #[default]
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOptions {
    pub depth: u32,
    pub limit: usize,
    pub direction: Direction,
    pub relation: Option<String>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            depth: 1,
            limit: 100,
            direction: Direction::Both,
            relation: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnresolvedReference {
    pub source: String,
    pub label: String,
    pub relation: String,
    pub file: String,
    pub line: u32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphResult {
    pub schema_version: u32,
    pub generation: u64,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved: Vec<UnresolvedReference>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathResult {
    pub found: bool,
    pub graph: GraphResult,
}

#[derive(Debug, Clone)]
pub struct ImportedGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub metadata: Value,
}

/// A consistent, explicit full-graph read for exports and offline analysis.
/// Ordinary navigation continues to use bounded indexed queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub schema_version: u32,
    pub generation: u64,
    pub kind: String,
    pub root: Option<String>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub metadata: Value,
}
