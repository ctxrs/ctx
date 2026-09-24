//! Syntax-backed extraction. No source execution, filesystem probing, or network access.
mod common;
pub use ctx_graph_configs as configs;
pub use ctx_graph_ingest as ingest;
pub use ctx_graph_python::parser;
pub mod model {
    pub use ctx_graph_types::*;
}
pub mod compiled;
pub mod extended;
mod go;
mod javascript;
mod rust;
pub mod scripted;
pub mod templates;

use crate::model::FileFacts;
use anyhow::{Result, bail};

/// Whether a native extractor is available for this repository-relative path.
pub fn supports(path: &str) -> bool {
    configs::supports(path)
        || matches!(
            path.rsplit('.').next(),
            Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "rs" | "go")
        )
        || extended::supports(path)
        || compiled::supports(path)
        || scripted::supports(path)
        || templates::supports(path)
}

/// Recognize configured documents or extensionless scripts from bounded source text.
pub fn recognizes(path: &str, source: &str) -> bool {
    configs::recognizes(path, source) || shebang(path, source).is_some()
}

fn shebang(path: &str, source: &str) -> Option<&'static str> {
    if std::path::Path::new(path).extension().is_some() {
        return None;
    }
    scripted::shebang_language(source)
}

/// Extract facts from a normalized repository-relative path. Unsupported files return `None`.
/// Syntax errors produce diagnostics and no partial graph. Dynamic targets remain references.
pub fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    let configured = configs::recognizes(path, source);
    let script = shebang(path, source);
    if !supports(path) && !configured && script.is_none() {
        return Ok(None);
    }
    if path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        bail!("source path must be a normalized relative POSIX path");
    }
    if configured || configs::supports(path) {
        return configs::parse(path, source, hash);
    }
    if let Some(language) = script {
        return match language {
            "javascript" => javascript::parse(path, source, hash).map(Some),
            "python" => crate::parser::parse_python(path, source, hash).map(Some),
            "julia" => extended::parse_named(path, source, hash, language),
            _ => scripted::parse_named(path, source, hash, language),
        };
    }
    if templates::supports(path) {
        return templates::parse(path, source, hash);
    }
    if let Some(facts) = extended::parse(path, source, hash)? {
        return Ok(Some(facts));
    }
    let facts = match path.rsplit('.').next().unwrap() {
        "rs" => rust::parse(path, source, hash)?,
        "go" => go::parse(path, source, hash)?,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => {
            javascript::parse(path, source, hash)?
        }
        _ if compiled::supports(path) => return compiled::parse(path, source, hash),
        _ => return scripted::parse(path, source, hash),
    };
    Ok(Some(facts))
}

/// Changes whenever extraction or binding semantics change, for incremental cache invalidation.
pub fn revision() -> &'static str {
    "native-languages-19"
}
