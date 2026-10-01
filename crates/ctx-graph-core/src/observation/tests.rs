use super::*;
use crate::{
    index,
    model::*,
    query::{PathSearchResult, SearchResult},
    store::Store,
};
use std::{cell::RefCell, path::Path};

type AfterCommit = Box<dyn FnOnce(&Path)>;

thread_local! {
    static AFTER_COMMIT: RefCell<Option<AfterCommit>> = RefCell::new(None);
}
pub(crate) fn after_index_commit(path: &Path) {
    let action = AFTER_COMMIT.with(|slot| slot.borrow_mut().take());
    if let Some(action) = action {
        action(path);
    }
}

fn facts(operation: GraphOperation) -> GraphObservation {
    GraphObservation::new(operation, GraphInvocation::Library)
}
fn result(truncated: bool) -> SearchResult {
    SearchResult {
        graph: GraphResult {
            schema_version: 1,
            generation: 8123,
            nodes: vec![],
            edges: vec![],
            unresolved: vec![],
            truncated,
        },
        seeds: vec!["private-seed-sentinel".into()],
        estimated_tokens: 14,
        contexts: vec!["private-context-sentinel".into()],
        truncation_reasons: if truncated {
            vec!["token_budget".into(), "private-reason-sentinel".into()]
        } else {
            vec![]
        },
    }
}

#[test]
fn paths_distinguish_found_scoped_absence_and_incomplete() {
    for (found, truncated, expected) in [
        (true, false, GraphPathDisposition::Found),
        (false, false, GraphPathDisposition::NotFoundWithinScope),
        (false, true, GraphPathDisposition::Incomplete),
    ] {
        let mut observation = facts(GraphOperation::Path);
        path(
            &mut observation,
            &PathSearchResult {
                found,
                result: result(truncated),
            },
        );
        assert_eq!(observation.path, Some(expected));
        assert_eq!(observation.nodes, Some(0));
        assert_eq!(observation.seeds, Some(1));
        assert_eq!(observation.estimated_json_tokens, Some(14));
        assert_eq!(observation.bounds.unwrap().token, truncated);
        assert_eq!(observation.bounds.unwrap().other, truncated);
        let debug = format!("{observation:?}");
        assert!(!debug.contains("sentinel"));
        assert!(!debug.contains("8123"));
    }
}

#[test]
fn ordinary_index_noop_and_failed_prepare_have_different_evidence() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join("sample.py"),
        "def entry():\n    return 1\n",
    )?;
    let db = temp.path().join(".graf/index.db");
    std::fs::create_dir_all(db.parent().unwrap())?;
    let options = index::IndexOptions {
        code_only: true,
        ..Default::default()
    };
    let mut first = facts(GraphOperation::Index);
    index::run_with_options_observed(temp.path(), &db, &options, &mut first)?;
    assert_eq!(first.index, Some(GraphIndexDisposition::Committed));
    assert_eq!(first.parsed_files, Some(1));
    assert_eq!(first.execution_succeeded, Some(true));
    assert!(first.nodes.unwrap() > 0);
    assert!(first.detect_duration.is_some());
    assert!(first.extract_duration.is_some());
    assert!(first.commit_duration.is_some());
    assert_eq!(first.semantic.configured, Some(false));
    assert_eq!(first.semantic.receipts, None);
    assert_eq!(first.output_served, None);
    let mut second = facts(GraphOperation::Update);
    index::run_with_options_observed(temp.path(), &db, &options, &mut second)?;
    assert_eq!(second.index, Some(GraphIndexDisposition::NoOp));
    assert_eq!(second.parsed_files, Some(0));
    let mut failed = facts(GraphOperation::Index);
    let missing = temp.path().join("missing");
    index::run_with_options_observed(&missing, &db, &options, &mut failed).unwrap_err();
    assert_eq!(failed.index, None);
    assert_eq!(failed.parsed_files, None);
    assert_eq!(failed.failure.unwrap().kind, GraphFailureKind::NotFound);
    assert_eq!(failed.execution_succeeded, Some(false));
    Ok(())
}

#[test]
fn committed_index_survives_wal_cleanup_failure() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join("sample.py"),
        "def entry():\n    return 1\n",
    )?;
    let db = temp.path().join(".graf/index.db");
    std::fs::create_dir_all(db.parent().unwrap())?;
    let reader = std::sync::Arc::new(std::sync::Mutex::new(None));
    let held = reader.clone();
    AFTER_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |db| {
            let connection = rusqlite::Connection::open(db).unwrap();
            connection.execute_batch("BEGIN").unwrap();
            connection
                .query_row("SELECT generation FROM metadata", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
            *held.lock().unwrap() = Some(connection);
        }))
    });
    let mut observation = facts(GraphOperation::Index);
    let result = index::run_with_options_observed(
        temp.path(),
        &db,
        &index::IndexOptions {
            code_only: true,
            ..Default::default()
        },
        &mut observation,
    );
    // Clear the lock before any assertion so a failing check cannot strand it.
    drop(reader.lock().unwrap().take());
    AFTER_COMMIT.with(|slot| slot.borrow_mut().take());
    assert!(result.is_err());
    assert_eq!(observation.index, Some(GraphIndexDisposition::Committed));
    assert_eq!(observation.parsed_files, Some(1));
    assert_eq!(observation.failure.unwrap().phase, GraphPhase::PostCommit);
    assert_eq!(observation.execution_succeeded, Some(false));
    assert!(Store::open_read_only(&db)?.stats()?.nodes > 0);
    Ok(())
}

#[test]
fn endpoint_failures_are_typed_and_do_not_depend_on_diagnostic_words() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("graph.db");
    let mut store = Store::create(&db)?;
    let node = |id: &str| Node {
        id: id.into(),
        label: "ambiguous-sentinel".into(),
        kind: "function".into(),
        file: "sentinel.py".into(),
        line: None,
        end_line: None,
        qualified_name: None,
        binding_key: None,
        metadata: serde_json::json!({}),
    };
    store.import_graph(ImportedGraph {
        nodes: vec![node("a"), node("b")],
        edges: vec![],
        metadata: serde_json::json!({}),
    })?;
    assert!(store.resolve_endpoint("a", &Default::default()).is_ok());
    let missing = store
        .resolve_endpoint("missing-sentinel", &Default::default())
        .unwrap_err();
    assert_eq!(failure_kind(&missing), GraphFailureKind::EndpointNotFound);
    let ambiguous = store
        .resolve_endpoint("ambiguous-sentinel", &Default::default())
        .unwrap_err();
    assert_eq!(
        failure_kind(&ambiguous),
        GraphFailureKind::EndpointAmbiguous
    );
    assert_eq!(
        failure_kind(&anyhow::anyhow!(
            "ambiguous symbol, busy, permission denied"
        )),
        GraphFailureKind::Unknown
    );
    let mut observation = facts(GraphOperation::Show);
    failed(&mut observation, &ambiguous);
    assert!(!format!("{observation:?}").contains("sentinel"));
    Ok(())
}

#[test]
fn unknown_community_convergence_is_not_failed_convergence() -> anyhow::Result<()> {
    let snapshot = GraphSnapshot {
        schema_version: 1,
        generation: 0,
        kind: "imported".into(),
        root: None,
        nodes: vec![],
        edges: vec![],
        metadata: serde_json::json!({}),
    };
    let options = crate::analysis::AnalysisOptions::default();
    let mut report = crate::analysis::analyze(&snapshot, &options)?;
    report.community_convergence_known = false;
    report.community_converged = false;
    let mut observation = facts(GraphOperation::Analyze);
    analysis_options(&mut observation, &options);
    analysis(&mut observation, &report);
    assert_eq!(observation.algorithm, Some(GraphAlgorithm::Leiden));
    assert_eq!(
        observation.community_convergence,
        Some(GraphConvergence::Unknown)
    );
    Ok(())
}
