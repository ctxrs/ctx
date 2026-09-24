use super::*;

#[test]
fn endpoint_fts_requires_bounded_global_prose_and_known_projection() -> Result<()> {
    use crate::store::Store;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    // Same endpoint and scope: small/just-under-cap input uses FTS, while
    // excess prose must bypass it even when the prose belongs to another file.
    for (version, repetitions, prose_file, fast) in [
        (5, 0, "near.rs", true),
        (5, 838_840, "near.rs", true),
        (5, 840_000, "near.rs", false),
        (5, 840_000, "far.rs", false),
        (6, 0, "near.rs", false),
    ] {
        let make_node = |id: &str, label: &str, file: &str, metadata| Node {
            id: id.into(),
            label: label.into(),
            kind: "function".into(),
            file: file.into(),
            line: None,
            end_line: None,
            qualified_name: None,
            binding_key: None,
            metadata,
        };
        let dir = tempfile::tempdir()?;
        let mut store = Store::create(&dir.path().join("graph.db"))?;
        store.import_graph(ImportedGraph {
            nodes: vec![
                make_node("a", "Coder", "near.rs", serde_json::Value::Null),
                make_node(
                    "b",
                    "Other",
                    prose_file,
                    serde_json::json!({"description": "code ".repeat(repetitions)}),
                ),
            ],
            edges: vec![],
            metadata: serde_json::Value::Null,
        })?;
        store
            .conn
            .execute("UPDATE metadata SET search_version=?", [version])?;
        let source_bytes: i64 = store.conn.query_row(
            "SELECT sum(length(CAST(search AS BLOB))) FROM nodes",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(source_bytes <= 8 * 1024 * 1024, repetitions < 840_000);
        let accessed = Arc::new(AtomicBool::new(false));
        let observed = accessed.clone();
        store
            .conn
            .authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "node_search",
                        ..
                    }
                ) {
                    observed.store(true, Ordering::Relaxed);
                    if !fast {
                        return Authorization::Deny;
                    }
                }
                Authorization::Allow
            }))?;
        assert_eq!(
            store
                .resolve_endpoint("near.rs::cod", &SearchOptions::default())?
                .id,
            "a"
        );
        assert_eq!(accessed.load(Ordering::Relaxed), fast);
        assert_eq!(
            store.resolve_endpoint("a", &SearchOptions::default())?.id,
            "a"
        );
        assert_eq!(store.stats()?.nodes, 2);
    }
    Ok(())
}

#[test]
fn endpoint_fts_global_node_cap_includes_every_record() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE nodes(search TEXT NOT NULL);
             WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<50000)
             INSERT INTO nodes SELECT 'x' FROM n;",
    )?;
    assert!(within_endpoint_fts_bounds(&conn)?);
    conn.execute("INSERT INTO nodes VALUES('x')", [])?;
    assert!(!within_endpoint_fts_bounds(&conn)?);
    Ok(())
}

#[test]
fn common_term_search_does_not_invoke_corpus_ranking() -> Result<()> {
    use crate::store::Store;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    let dir = tempfile::tempdir()?;
    let mut store = Store::create(&dir.path().join("graph.db"))?;
    store.import_graph(ImportedGraph {
        nodes: (0..1_024)
            .map(|i| Node {
                id: format!("n{i:04}"),
                label: format!("Common{i:04}"),
                kind: "function".into(),
                file: "file.rs".into(),
                line: None,
                end_line: None,
                qualified_name: None,
                binding_key: None,
                metadata: serde_json::Value::Null,
            })
            .collect(),
        edges: vec![],
        metadata: serde_json::Value::Null,
    })?;
    // A result cap cannot catch BM25's hidden posting-list scan. Reject
    // both explicit BM25 and FTS5's implicit rank column at preparation.
    store
        .conn
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Function { function_name }
                if function_name.eq_ignore_ascii_case("bm25") =>
            {
                Authorization::Deny
            }
            AuthAction::Read {
                table_name: "node_search",
                column_name: "rank",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))?;
    assert!(store.conn.prepare(
            "SELECT bm25(node_search) FROM node_search WHERE node_search MATCH 'Common*' LIMIT 1"
        ).is_err());
    assert!(
        store
            .conn
            .prepare("SELECT rank FROM node_search WHERE node_search MATCH 'Common*' LIMIT 1")
            .is_err()
    );
    let result = store.query_extended(
        "Common",
        &SearchOptions {
            graph: QueryOptions {
                depth: 0,
                limit: 2,
                ..QueryOptions::default()
            },
            ..SearchOptions::default()
        },
    )?;
    assert_eq!(
        result
            .graph
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>(),
        ["n0000", "n0001"]
    );
    assert!(result.truncation_reasons.iter().any(|s| s == "node_limit"));
    Ok(())
}

#[test]
fn interrupted_query_removes_its_handler() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let sql = "WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<1000000) SELECT sum(n) FROM numbers";
    let error = budgeted(&conn, || {
        Ok(conn.query_row(sql, [], |row| row.get::<_, i64>(0))?)
    })
    .unwrap_err();
    assert!(error.to_string().contains("work/time budget"));
    // The identical operation succeeds after the request guard drops.
    let sum: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    assert_eq!(sum, 500000500000);
    Ok(())
}
