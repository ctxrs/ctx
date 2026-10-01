use super::*;
use ctx_client_observability::analytics as wire;
use std::io::{self, Write};

struct Writer {
    bytes: Vec<u8>,
    write_failed: bool,
    flush_failed: bool,
    flushed: bool,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.write_failed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushed = true;
        if self.flush_failed {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn mixed_scope_success_does_not_erase_graph_failure_and_flush_is_observed() {
    let result = ScopedResults {
        schema_version: 1,
        scope: SearchScope::All,
        limit_per_scope: 10,
        partial: true,
        history: Some(ScopeResult::Ok {
            result: serde_json::json!({"results": []}),
        }),
        graph: ScopeResult::Unavailable {
            error: "graph unavailable".into(),
            next_action: "index graph",
        },
    };
    let ui = crate::ui::Ui::stdio(crate::ui::ColorMode::Never);
    for (write_failed, flush_failed) in [(false, false), (true, false), (false, true)] {
        let mut facts =
            GraphObservation::new(GraphOperation::Search, GraphInvocation::ScopedSearch);
        facts.phase = GraphPhase::Open;
        facts.fail(GraphFailureKind::StoreOpen);
        facts.duration = Some(std::time::Duration::ZERO);
        let mut out = Writer {
            bytes: vec![],
            write_failed,
            flush_failed,
            flushed: false,
        };
        let output = write_results(
            &mut out,
            &result,
            true,
            false,
            ui.stdout_context(),
            &mut facts,
        );
        assert!(out.flushed);
        assert_eq!(output.is_err(), write_failed || flush_failed);
        let event = crate::engine_telemetry::graph_completed(facts).unwrap();
        assert_eq!(
            event.details.failure.unwrap().kind,
            wire::GraphFailureKind::StoreOpen
        );
        assert_eq!(
            event.completion.delivery,
            if write_failed || flush_failed {
                wire::DeliveryEvidence::Failed
            } else {
                wire::DeliveryEvidence::KnownComplete
            }
        );
        assert!(event.completion.execution.is_err());
    }
}

#[test]
fn graph_query_captures_empty_typed_result_before_projection_without_history() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("graph.db");
    drop(ctx_graph::ctx_graph_core::store::Store::create(&db).unwrap());
    let mut facts = GraphObservation::new(GraphOperation::Search, GraphInvocation::ScopedSearch);
    let value = graph_query("synthetic-missing-symbol", 10, Some(&db), &mut facts).unwrap();
    assert_eq!(facts.execution_succeeded, Some(true));
    assert_eq!(facts.nodes, Some(0));
    assert_eq!(facts.output_served, None);
    assert_eq!(value["graph"]["nodes"], serde_json::json!([]));
    assert!(!temp.path().join("history").exists());
}
