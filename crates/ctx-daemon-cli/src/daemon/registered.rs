use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub fn collect() -> Result<Vec<Value>> {
    let (_, registrations) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
    let mut roots = BTreeSet::new();
    let mut errors = Vec::new();
    for record in ctx_daemon_runtime::observe_installation_registrations(&registrations)? {
        if let Some(error) = record.error {
            errors.push(json!({
                "data_root": record.data_root,
                "registration_path": record.registration_path.display().to_string(),
                "daemon": null,
                "error": format!("{error:#}"),
            }));
        } else if let Some(root) = record.data_root {
            roots.insert(root);
        }
    }
    let mut rows = roots
        .into_iter()
        .map(|root| {
            json!({
                "daemon": crate::paths_status::daemon_report_read_only(&root),
                "data_root": root,
            })
        })
        .collect::<Vec<_>>();
    rows.extend(errors);
    Ok(rows)
}

#[cfg(test)]
#[path = "registered_tests.rs"]
mod tests;
