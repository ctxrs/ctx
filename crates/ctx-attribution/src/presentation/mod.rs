//! Human, JSON and MCP presentation of bounded attribution results.
//!
//! This leaf owns human terminal rendering, stable JSON projection, and MCP
//! text projection. Query execution, repository inference, and application
//! policy remain with their existing authorities.

mod blame_summary;
pub mod mcp_text;
mod render;

pub use render::{
    BlameOutput, blame_result_json, print_blame_result, print_blame_result_with_evidence_preview,
};

/// Quotes one trusted command argument for copyable shell presentation.
#[must_use]
pub fn shell_quote_arg(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | '/' | ':' | '@')
        })
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}
