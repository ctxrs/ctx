// Keep archive extraction tests under a tests/ path so source-size checks apply only to production code.
#[cfg(unix)]
mod duplicate_targets;
mod hardlink;
mod helpers;
mod symlink;
mod traversal;
