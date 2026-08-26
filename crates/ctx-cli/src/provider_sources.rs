//! Compatibility adapters for final-binary callers that have not moved yet.

use std::path::Path;

use anyhow::Result;

pub(crate) use ctx_history_cli::{
    discovery_report_issues_json_with_provider_roots, enrich_sources_json_with_selection,
    sources_json,
};

pub(crate) fn discovered_plugin_sources_json(data_root: &Path) -> Result<Vec<serde_json::Value>> {
    ctx_history_cli::discovered_plugin_sources_json(data_root)
}
