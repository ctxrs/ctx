mod scanner;
mod source_backed;

pub(crate) use source_backed::TraeReplacementTree;
#[cfg(test)]
pub(crate) use source_backed::{scan_trae_source_backed_explicit_v0, TraeSourceBackedErrorV0};
