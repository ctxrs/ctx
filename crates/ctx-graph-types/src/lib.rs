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
    pub semantic_usage: Option<SemanticUsage>,
    /// Provider-reported usage per attempted call; absent counters are unknown.
    /// These are distinct from the maximum-token reservations above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_usage: Option<Vec<ProviderUsage>>,
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SemanticUsage {
    /// Reserved generations: one per HTTP/generic adapter attempt; native
    /// Claude reserves every permitted turn before starting the invocation.
    pub calls: usize,
    /// Sum of per-generation maximum output allowances, never billed usage.
    pub reserved_output_tokens: u64,
}

/// Provider-reported counters for one attempted request. Missing counters remain
/// unknown; these are not reservations or a claim about the provider's invoice.
/// Input/output retain the provider's native definitions. Cache and reasoning
/// counters may be subsets: never sum these fields to infer a token total.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: Provider,
    pub requested_model: String,
    pub reported_model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    OpenAi,
    Anthropic,
    Gemini,
    Ollama,
    Azure,
    Cli,
    Bedrock,
    ClaudeCli,
}

mod shebang;
mod source_io;
pub mod syntax;
pub use shebang::shebang_language;
pub use source_io::read_source;
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

pub const EXTRACTOR_REVISION: u32 = 12;
