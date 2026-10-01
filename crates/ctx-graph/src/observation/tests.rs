use super::*;
use clap::Parser;
use std::sync::Mutex;

struct FailingWriter {
    write: bool,
    flush: bool,
}
impl io::Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.write {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "private-writer-sentinel",
            ))
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.flush {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "private-flush-sentinel",
            ))
        } else {
            Ok(())
        }
    }
}
fn facts() -> GraphObservation {
    GraphObservation::new(GraphOperation::Index, GraphInvocation::Cli)
}

#[test]
fn delivery_failure_does_not_erase_committed_index_or_execution_failure() {
    let mut observation = facts();
    observation.index = Some(GraphIndexDisposition::Committed);
    observation.parsed_files = Some(1);
    let result = Ok(());
    assert_eq!(
        finish_cli(
            &mut observation,
            &result,
            true,
            &mut FailingWriter {
                write: false,
                flush: true
            },
            &mut Vec::new()
        ),
        1
    );
    assert_eq!(observation.execution_succeeded, Some(true));
    assert_eq!(observation.index, Some(GraphIndexDisposition::Committed));
    assert_eq!(observation.output_served, Some(false));
    assert_eq!(
        observation.output_failure,
        Some(GraphFailureKind::BrokenPipe)
    );
    assert_eq!(observation.failure, None);
    assert!(!format!("{observation:?}").contains("sentinel"));

    observation.phase = GraphPhase::PostCommit;
    observation.fail(GraphFailureKind::StoreBusy);
    let result = Err(anyhow::anyhow!("private-engine-sentinel"));
    finish_cli(
        &mut observation,
        &result,
        true,
        &mut Vec::new(),
        &mut FailingWriter {
            write: true,
            flush: false,
        },
    );
    assert_eq!(observation.execution_succeeded, Some(false));
    assert_eq!(
        observation.failure.unwrap().kind,
        GraphFailureKind::StoreBusy
    );
    assert_eq!(
        observation.output_failure,
        Some(GraphFailureKind::BrokenPipe)
    );
}

#[test]
fn served_error_is_not_product_success_and_mid_command_write_is_not_inferred_success() {
    let mut observation = facts();
    let result = Err(anyhow::anyhow!("private-error-sentinel"));
    let mut stderr = Vec::new();
    assert_eq!(
        finish_cli(
            &mut observation,
            &result,
            true,
            &mut Vec::new(),
            &mut stderr
        ),
        1
    );
    assert_eq!(observation.output_served, Some(true));
    assert_eq!(observation.execution_succeeded, Some(false));
    assert!(
        serde_json::from_slice::<serde_json::Value>(&stderr)
            .unwrap()
            .get("error")
            .is_some()
    );
    assert!(!format!("{observation:?}").contains("sentinel"));

    let mut observation = facts();
    let result =
        output::output_result::<()>(Err(
            io::Error::new(io::ErrorKind::BrokenPipe, "closed").into()
        ));
    finish_cli(
        &mut observation,
        &result,
        false,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(observation.execution_succeeded, None);
    assert_eq!(observation.output_served, Some(false));
    assert_eq!(
        observation.output_failure,
        Some(GraphFailureKind::BrokenPipe)
    );
}

#[test]
fn parsed_aliases_have_closed_operations() {
    for (args, expected) in [
        (
            vec!["query", "private-query-sentinel"],
            GraphOperation::Search,
        ),
        (
            vec!["search", "private-query-sentinel"],
            GraphOperation::Search,
        ),
        (vec!["stats"], GraphOperation::Stats),
        (vec!["serve"], GraphOperation::Serve),
        (vec!["provider", "list"], GraphOperation::ProviderList),
    ] {
        let parsed = cli::Cli::try_parse_from(std::iter::once("ctx graph").chain(args)).unwrap();
        assert_eq!(operation(&parsed.graph.command), expected);
    }
}

#[test]
fn optional_callback_and_observed_search_do_not_change_saved_graph() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join("sample.py"),
        "def visible():\n    return 1\n",
    )?;
    let db = temp.path().join(".graf/index.db");
    index::run_with_options(
        temp.path(),
        &db,
        &index::IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )?;
    let options = ctx_graph_core::query::SearchOptions::default();
    let ordinary = crate::search(&db, "visible", &options)?;
    // A saved-graph query must not follow the changed source or start extraction.
    std::fs::write(
        temp.path().join("sample.py"),
        "def replacement():\n    return 2\n",
    )?;
    let mut observation =
        GraphObservation::new(GraphOperation::Search, GraphInvocation::ScopedSearch);
    let observed = search_observed(&db, "visible", &options, &mut observation)?;
    assert_eq!(
        serde_json::to_value(ordinary)?,
        serde_json::to_value(observed)?
    );
    assert_eq!(observation.execution_succeeded, Some(true));
    assert_eq!(observation.output_served, None);
    assert_eq!(observation.semantic.receipts, None);
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let observer: GraphObserver = Arc::new(move |facts| captured.lock().unwrap().push(facts));
    emit(&None, observation);
    assert!(events.lock().unwrap().is_empty());
    emit(&Some(observer), observation);
    assert_eq!(events.lock().unwrap().len(), 1);
    // The host's full/closed bounded queue remains fail-open.
    let (sender, receiver) = std::sync::mpsc::sync_channel(0);
    drop(receiver);
    let observer: GraphObserver = Arc::new(move |facts| {
        let _ = sender.try_send(facts);
    });
    emit(&Some(observer), observation);
    Ok(())
}

#[test]
fn bounded_watch_summarizes_quiet_polls_without_sending_one_event_per_poll() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join("sample.py"),
        "def visible():\n    return 1\n",
    )?;
    let db = temp.path().join(".graf/index.db");
    index::run_with_options(
        temp.path(),
        &db,
        &index::IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )?;
    let args = cli::Cli::try_parse_from([
        "ctx graph",
        "--db",
        db.to_str().unwrap(),
        "watch",
        "--iterations",
        "1",
    ])?
    .graph;
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let observer: GraphObserver = Arc::new(move |facts| captured.lock().unwrap().push(facts));
    let mut observation = GraphObservation::new(GraphOperation::Parse, GraphInvocation::Cli);
    run_parsed_observed(args, &mut observation, Some(observer))?;
    assert_eq!(observation.operation, GraphOperation::Watch);
    assert_eq!(observation.polls, Some(1));
    assert_eq!(observation.retries, Some(0));
    assert_eq!(observation.lifecycle, Some(GraphLifecycle::Stopped));
    assert_eq!(observation.execution_succeeded, Some(true));
    assert_eq!(events.lock().unwrap().len(), 1);
    assert_eq!(
        events.lock().unwrap()[0].lifecycle,
        Some(GraphLifecycle::Ready)
    );
    assert_eq!(observation.output_served, None);
    Ok(())
}

#[test]
fn exported_artifact_is_known_before_host_delivery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("graph.db");
    Store::create(&db)?.import_graph(ctx_graph_core::model::ImportedGraph {
        nodes: vec![],
        edges: vec![],
        metadata: serde_json::json!({}),
    })?;
    let path = temp.path().join("export.json");
    let args = cli::Cli::try_parse_from([
        "ctx graph",
        "--json",
        "--db",
        db.to_str().unwrap(),
        "export",
        "snapshot-json",
        "--output",
        path.to_str().unwrap(),
    ])?
    .graph;
    let mut observation = facts();
    run_parsed_observed(args, &mut observation, None)?;
    assert_eq!(observation.operation, GraphOperation::Export);
    assert_eq!(observation.artifact_committed, Some(true));
    assert_eq!(
        observation.artifact_bytes,
        Some(std::fs::metadata(path)?.len())
    );
    assert_eq!(observation.output_served, None);
    finish_cli(
        &mut observation,
        &Ok(()),
        true,
        &mut FailingWriter {
            write: false,
            flush: true,
        },
        &mut Vec::new(),
    );
    assert_eq!(observation.execution_succeeded, Some(true));
    assert_eq!(observation.artifact_committed, Some(true));
    assert_eq!(observation.output_served, Some(false));
    Ok(())
}

#[test]
fn cache_inspection_and_absent_removal_have_real_zero_counts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut observation = facts();
    extraction::cache_observed(
        &extraction::CacheArgs {
            command: extraction::CacheCommand::Inspect {
                directory: temp.path().into(),
                limit: 1,
                max_entry_bytes: 1024,
            },
        },
        &mut observation,
    )?;
    assert_eq!(observation.result_count, Some(0));
    assert_eq!(observation.truncated, Some(false));
    let key = "0".repeat(64);
    extraction::cache_observed(
        &extraction::CacheArgs {
            command: extraction::CacheCommand::Remove {
                directory: temp.path().into(),
                key,
            },
        },
        &mut observation,
    )?;
    assert_eq!(observation.result_count, Some(0));
    assert_eq!(observation.execution_succeeded, Some(true));
    Ok(())
}
