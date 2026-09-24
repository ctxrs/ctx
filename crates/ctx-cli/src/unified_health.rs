//! Final-executable health composition for engines independent of history.
//!
//! The graph adapter selects the database; this module only opens that selection
//! read-only. Existing history reports, diagnostics and exit status remain owned
//! by status/doctor. Append these components before serializing or rendering.

use std::path::Path;

use anyhow::{Context, Result};
use ctx_graph::ctx_graph_core::store::Store;
use serde_json::{json, Value};

use crate::ui::{
    diagnostic, fields, section, Diagnostic, DiagnosticLevel, Document, Field, RenderContext, Ui,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphAvailability {
    NotIndexed,
    Readable,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct UnifiedHealth {
    graph: GraphAvailability,
}

impl UnifiedHealth {
    /// Graph discovery failed before selecting a database. Keep the raw cause
    /// out of health output while reporting the component as unavailable.
    pub(crate) fn unavailable() -> Self {
        Self {
            graph: GraphAvailability::Unavailable,
        }
    }

    /// None means graph discovery found no database. This never discovers
    /// history, creates a database, reads source files, or performs a refresh.
    pub(crate) fn inspect(graph_db: Option<&Path>) -> Self {
        let graph = match graph_db {
            None => GraphAvailability::NotIndexed,
            Some(path) => match path.try_exists() {
                Ok(false) => GraphAvailability::NotIndexed,
                Ok(true) => match Store::open_read_only(path) {
                    Ok(_) => GraphAvailability::Readable,
                    Err(_) => GraphAvailability::Unavailable,
                },
                Err(_) => GraphAvailability::Unavailable,
            },
        };
        Self { graph }
    }

    /// Add to either an ordinary report or its existing sanitized error object.
    /// History's schema version, readiness, findings and usage fields are intact.
    pub(crate) fn append_json(&self, report: &mut Value) -> Result<()> {
        let report = report
            .as_object_mut()
            .context("health report must be a JSON object")?;
        let (status, detail, next_action) = self.graph_description();
        report.insert(
            "graph".to_owned(),
            json!({
                "status": status,
                "detail": detail,
                "freshness": "not_checked",
                "next_action": next_action,
                "requires_history": false,
                "read_only": true,
            }),
        );
        report.insert(
            "output".to_owned(),
            json!({
                "status": "available",
                "built_in": true,
                "requires_history": false,
                "next_action": "ctx docs show unified-context",
            }),
        );
        Ok(())
    }

    /// Append to the existing status/doctor document using its stream context.
    pub(crate) fn append_human(&self, context: &RenderContext, document: &mut Document) {
        let (status, detail, next_action) = self.graph_description();
        if !document.is_empty() {
            document.push_blank();
        }
        document.append(section(
            "Graph",
            fields(
                context,
                &[
                    Field::new("Snapshot", status),
                    Field::new("Detail", detail),
                    Field::new("Next", next_action),
                ],
            ),
        ));
        document.push_blank();
        document.append(section(
            "Command output",
            fields(
                context,
                &[
                    Field::new("Engine", "available in ctx"),
                    Field::new("History setup", "not required"),
                    Field::new("Next", "ctx docs show unified-context"),
                ],
            ),
        ));
    }

    fn graph_description(&self) -> (&'static str, &'static str, &'static str) {
        match self.graph {
            GraphAvailability::NotIndexed => (
                "not_indexed",
                "No graph snapshot selected. History and command output remain independent.",
                "ctx graph index .",
            ),
            GraphAvailability::Readable => (
                "readable",
                "The saved snapshot opens read-only. Source freshness was not checked.",
                "ctx graph stats",
            ),
            GraphAvailability::Unavailable => (
                "unavailable",
                "The selected graph could not be read. Inspect it before updating or replacing it.",
                "ctx graph stats",
            ),
        }
    }
}

/// Emit one sanitized history error with independent engine health. The caller
/// supplies the existing diagnostic; no history error text is reparsed or echoed.
pub(crate) fn emit_history_failure(
    json_output: bool,
    mut report: Value,
    mut document: Document,
    components: Option<&UnifiedHealth>,
    ui: &mut Ui,
) -> Result<()> {
    if let Some(components) = components {
        report["history"] = json!({"status": "unavailable"});
        components.append_json(&mut report)?;
        document.push_blank();
        document.append(section(
            "History",
            fields(ui.stderr_context(), &[Field::new("Health", "unavailable")]),
        ));
        components.append_human(ui.stderr_context(), &mut document);
    }
    if json_output {
        let mut bytes = serde_json::to_vec(&report)?;
        bytes.push(b'\n');
        ui.write_stderr_bytes(&bytes)?;
    } else {
        ui.write_stderr(&document)?;
    }
    Err(crate::dispatch::rendered_cli_error())
}

pub(crate) fn history_health_failure(
    schema_version: u32,
    json_output: bool,
    components: &UnifiedHealth,
    ui: &mut Ui,
) -> Result<()> {
    let code = "history_health_unavailable";
    let message =
        "History health could not be read. Check history configuration and the retained index.";
    let report = json!({
        "schema_version": schema_version,
        "ok": false,
        "error": {"code": code, "message": message},
        "findings": [message],
        "read_only": true,
    });
    let document = diagnostic(
        ui.stderr_context(),
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: "History health is unavailable",
            detail: Some(message),
            fields: &[Field::new("Code", code)],
            action: None,
        },
    );
    emit_history_failure(json_output, report, document, Some(components), ui)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::ui::{StreamKind, TestContext};

    #[test]
    fn absent_graph_does_not_initialize_storage_or_change_history_health() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join(".graf/index.db");
        for selected in [None, Some(database.as_path())] {
            let health = UnifiedHealth::inspect(selected);
            let original = json!({
                "schema_version": 2,
                "initialized": true,
                "lexical": {"status": "ready"},
                "read_only": true,
            });
            let mut report = original.clone();
            health.append_json(&mut report).unwrap();
            assert_eq!(report["graph"]["status"], "not_indexed");
            assert_eq!(report["output"]["status"], "available");
            report.as_object_mut().unwrap().remove("graph");
            report.as_object_mut().unwrap().remove("output");
            assert_eq!(report, original);
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn discovery_failure_is_distinct_from_an_absent_graph() {
        let mut report = json!({"schema_version": 1, "ok": true});
        UnifiedHealth::unavailable()
            .append_json(&mut report)
            .unwrap();
        assert_eq!(report["graph"]["status"], "unavailable");
        assert_eq!(report["graph"]["freshness"], "not_checked");
        assert_eq!(report["output"]["status"], "available");
        assert_eq!(report["ok"], true);
    }

    #[test]
    fn readable_graph_preserves_history_failure_and_does_not_claim_freshness() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("graph.db");
        drop(Store::create(&database).unwrap());
        let before = fs::read(&database).unwrap();
        let health = UnifiedHealth::inspect(Some(&database));
        let mut report = json!({
            "schema_version": 1,
            "ok": false,
            "findings": ["history unavailable"],
            "source_epoch": {"lexical": {"status": "unavailable"}},
        });
        health.append_json(&mut report).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["findings"], json!(["history unavailable"]));
        assert_eq!(report["graph"]["status"], "readable");
        assert_eq!(report["graph"]["freshness"], "not_checked");
        assert_eq!(fs::read(&database).unwrap(), before);
    }

    #[test]
    fn invalid_graph_is_reported_without_leaking_bytes_or_hiding_other_components() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("graph.db");
        let canary = b"invalid graph fixture marker 871b9";
        fs::write(&database, canary).unwrap();
        let health = UnifiedHealth::inspect(Some(&database));
        let mut report = ctx_cli_presentation::commands::malformed_status_config_json();
        let prior_usage = report["local_usage"].clone();
        health.append_json(&mut report).unwrap();
        assert_eq!(report["local_usage"], prior_usage);
        assert_eq!(report["graph"]["status"], "unavailable");
        assert_eq!(report["output"]["requires_history"], false);
        let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
        let mut document = Document::new();
        health.append_human(&context, &mut document);
        let text = document.render_plain();
        assert!(text.contains("unavailable"));
        assert!(text.contains("History setup"));
        assert!(text.contains("not required"));
        assert!(!text.contains("871b9"));
        assert!(!report.to_string().contains("871b9"));
        assert_eq!(fs::read(&database).unwrap(), canary);
    }
}
