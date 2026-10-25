use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;

pub fn collect() -> Result<Vec<(PathBuf, Value)>> {
    Ok(
        crate::daemon_autostart::registered_installation_daemon_roots()?
            .into_iter()
            .map(|root| {
                let report = crate::paths_status::daemon_report_read_only(&root);
                (root, report)
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "registered_tests.rs"]
mod tests;
