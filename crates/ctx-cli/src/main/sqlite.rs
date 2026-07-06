#[allow(unused_imports)]
use super::*;

pub(crate) fn error_summary(error: &anyhow::Error) -> String {
    let top = error.to_string();
    let root = error
        .chain()
        .last()
        .map(ToString::to_string)
        .unwrap_or_else(|| top.clone());
    if is_sqlite_busy_text(&top) || is_sqlite_busy_text(&root) {
        return "ctx index is busy because another ctx import or search refresh is writing to the local database; retry in a moment, or rerun the search with `--refresh off` to use the existing index".to_owned();
    }
    if root == top || top.contains(&root) {
        top
    } else {
        format!("{top}: {root}")
    }
}

pub(crate) fn is_sqlite_busy_text(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("database is locked") || lower.contains("database table is locked")
}
