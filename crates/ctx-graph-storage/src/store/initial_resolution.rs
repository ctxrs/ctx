use super::*;

pub(super) fn initial_resolution(
    reference: &Reference,
    candidate_keys: &[String],
    bindings: &HashMap<&str, InitialBinding>,
) -> (Option<i64>, String) {
    let mut reason = if reference.reason.is_empty() {
        "no matching binding".to_owned()
    } else {
        reference.reason.clone()
    };
    for key in candidate_keys {
        match bindings.get(key.as_str()) {
            None => continue,
            Some(InitialBinding::Unique(target)) => return (Some(*target), String::new()),
            Some(InitialBinding::Ambiguous) => {
                reason = format!("ambiguous binding: {key}");
                break;
            }
        }
    }
    (None, reason)
}

pub(super) fn reference_context<'a>(source: &'a Node, reference_id: &str) -> Option<&'a str> {
    source.metadata["python_references"]
        .as_array()?
        .iter()
        .find(|item| item["reference_id"] == reference_id)?["context"]
        .as_str()
}

pub(super) fn resolved_reference_edge(reference: &Reference, source: &Node, target: &str) -> Edge {
    let mut metadata = serde_json::json!({"reference_id": reference.id});
    if let Some(context) = reference_context(source, &reference.id) {
        metadata["context"] = serde_json::Value::String(context.to_owned());
    }
    Edge {
        id: format!("reference:{}", reference.id),
        source: reference.source.clone(),
        target: target.to_owned(),
        relation: reference.relation.clone(),
        directed: true,
        file: Some(reference.file.clone()),
        line: Some(reference.line),
        confidence: "statically_resolved".to_owned(),
        metadata,
    }
}

pub(super) fn publish_initial_native(tx: &Transaction<'_>, facts: &[FileFacts]) -> Result<()> {
    tx.execute_batch(DROP_STORAGE_COUNT_TRIGGERS)?;
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS nodes_insert;
         DROP TRIGGER IF EXISTS nodes_delete;
         DROP TABLE IF EXISTS node_search;
         DROP INDEX IF EXISTS nodes_label;
         DROP INDEX IF EXISTS nodes_file;
         DROP INDEX IF EXISTS node_aliases_binding;
         DROP INDEX IF EXISTS refs_owner;
         DROP INDEX IF EXISTS refs_unresolved_source;
         DROP INDEX IF EXISTS refs_unresolved_relation;
         DROP INDEX IF EXISTS ref_keys_binding;
         DROP INDEX IF EXISTS edges_source;
         DROP INDEX IF EXISTS edges_target;
         DROP INDEX IF EXISTS edges_source_relation;
         DROP INDEX IF EXISTS edges_target_relation;
         DROP INDEX IF EXISTS refs_source;
         DROP INDEX IF EXISTS nodes_qualified;
         DROP INDEX IF EXISTS nodes_binding;
         DROP INDEX IF EXISTS nodes_owner;
         DROP INDEX IF EXISTS edges_source_direction;
         DROP INDEX IF EXISTS edges_target_direction;
         DROP INDEX IF EXISTS edges_source_direction_relation;
         DROP INDEX IF EXISTS edges_target_direction_relation;
         DROP INDEX IF EXISTS edges_owner;",
    )?;

    let total_nodes = facts.iter().map(|file| file.nodes.len()).sum();
    let mut file_keys = HashMap::with_capacity(facts.len());
    let mut node_keys = HashMap::with_capacity(total_nodes);
    let mut node_ids = HashMap::with_capacity(total_nodes);
    let mut source_nodes = HashMap::with_capacity(total_nodes);
    let mut bindings = HashMap::new();
    let ruby_nodes: Vec<_> = facts
        .iter()
        .flat_map(|file| &file.nodes)
        .filter(|node| node.metadata["language"] == "ruby")
        .cloned()
        .collect();
    let ruby_context = ctx_graph_languages::scripted::RubyContext::from_nodes(&ruby_nodes);

    {
        let mut insert_file = tx.prepare(
            "INSERT INTO files(path,hash,module,diagnostics,facts_hash) VALUES(?1,?2,?3,?4,?5)",
        )?;
        for file in facts {
            insert_file.execute(params![
                file.path,
                file.hash,
                file.module,
                serde_json::to_string(&file.diagnostics)?,
                facts_hash(file)?
            ])?;
            file_keys.insert(file.path.as_str(), tx.last_insert_rowid());
        }
    }
    {
        let mut insert_node = tx.prepare(
            "INSERT INTO nodes(id,label,kind,file,line,end_line,qualified_name,binding_key,metadata,owner_key,search) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        )?;
        let mut insert_alias = tx.prepare(
            "INSERT INTO node_aliases(node_key,binding_key) VALUES(?1,?2) ON CONFLICT(node_key,binding_key) DO NOTHING",
        )?;
        for file in facts {
            let owner = *file_keys
                .get(file.path.as_str())
                .context("missing initial file owner")?;
            for node in &file.nodes {
                insert_node.execute(params![
                    node.id,
                    node.label,
                    node.kind,
                    node.file,
                    node.line,
                    node.end_line,
                    node.qualified_name,
                    node.binding_key,
                    serde_json::to_string(&node.metadata)?,
                    owner,
                    search_text(node)
                ])?;
                let node_key = tx.last_insert_rowid();
                node_keys.insert(node.id.as_str(), node_key);
                node_ids.insert(node_key, node.id.as_str());
                source_nodes.insert(node.id.as_str(), node);
                if let Some(binding) = node.binding_key.as_deref() {
                    record_initial_binding(&mut bindings, binding, node_key);
                }
                for alias in binding_aliases(node)? {
                    insert_alias.execute(params![node_key, alias])?;
                    record_initial_binding(&mut bindings, alias, node_key);
                }
            }
        }
    }
    {
        let mut insert_ref = tx.prepare(
            "INSERT INTO refs(id,source,source_key,owner_key,label,relation,file,line,candidate_keys,reason,resolved_target_key,resolution_reason) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )?;
        let mut insert_key =
            tx.prepare("INSERT INTO ref_keys(ref_key,priority,binding_key) VALUES(?1,?2,?3)")?;
        let mut insert_edge = tx.prepare(
            "INSERT INTO edges(id,source,target,source_key,target_key,relation,directed,file,line,confidence,metadata,owner_key,ref_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        )?;
        for file in facts {
            let owner = *file_keys
                .get(file.path.as_str())
                .context("missing initial reference owner")?;
            for reference in &file.references {
                let source_key = *node_keys
                    .get(reference.source.as_str())
                    .context("missing initial reference source")?;
                let source = source_nodes
                    .get(reference.source.as_str())
                    .context("missing initial reference source payload")?;
                let ruby_keys = (source.metadata["language"] == "ruby")
                    .then(|| ruby_context.inherited_keys(reference))
                    .flatten();
                let candidate_keys = ruby_keys.as_deref().unwrap_or(&reference.candidate_keys);
                let (target_key, reason) = initial_resolution(reference, candidate_keys, &bindings);
                insert_ref.execute(params![
                    reference.id,
                    reference.source,
                    source_key,
                    owner,
                    reference.label,
                    reference.relation,
                    reference.file,
                    reference.line,
                    serde_json::to_string(&reference.candidate_keys)?,
                    reference.reason,
                    target_key,
                    reason
                ])?;
                let ref_key = tx.last_insert_rowid();
                for (priority, binding) in candidate_keys.iter().enumerate() {
                    insert_key.execute(params![ref_key, priority as i64, binding])?;
                }
                if let Some(target_key) = target_key {
                    let target = node_ids
                        .get(&target_key)
                        .context("missing initial resolved target")?;
                    let edge = resolved_reference_edge(reference, source, target);
                    insert_edge.execute(params![
                        edge.id,
                        edge.source,
                        edge.target,
                        source_key,
                        target_key,
                        edge.relation,
                        edge.directed,
                        edge.file,
                        edge.line,
                        edge.confidence,
                        serde_json::to_string(&edge.metadata)?,
                        owner,
                        ref_key
                    ])?;
                }
            }
        }
        for file in facts {
            let owner = *file_keys
                .get(file.path.as_str())
                .context("missing initial edge owner")?;
            for edge in &file.edges {
                let source_key = *node_keys
                    .get(edge.source.as_str())
                    .context("missing initial edge source")?;
                let target_key = *node_keys
                    .get(edge.target.as_str())
                    .context("missing initial edge target")?;
                insert_edge.execute(params![
                    edge.id,
                    edge.source,
                    edge.target,
                    source_key,
                    target_key,
                    edge.relation,
                    edge.directed,
                    edge.file,
                    edge.line,
                    edge.confidence,
                    serde_json::to_string(&edge.metadata)?,
                    owner,
                    Option::<i64>::None
                ])?;
            }
        }
    }

    tx.execute_batch(
        "CREATE VIRTUAL TABLE node_search USING fts5(text, content='', contentless_delete=1);
         INSERT INTO node_search(rowid,text) SELECT nkey,search FROM nodes;
         CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
             INSERT INTO node_search(rowid,text) VALUES(new.nkey,new.search);
         END;
         CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
             DELETE FROM node_search WHERE rowid=old.nkey;
         END;",
    )?;
    ensure_storage_indices(tx)?;
    rebuild_storage_counts(tx)?;
    tx.execute_batch(STORAGE_COUNT_TRIGGERS)?;
    Ok(())
}

pub(super) fn insert_node(conn: &Connection, node: &Node, owner: Option<&str>) -> Result<()> {
    ensure!(!node.id.is_empty(), "node ID cannot be empty");
    let owner = owner
        .map(|path| {
            conn.query_row("SELECT fkey FROM files WHERE path=?1", [path], |row| {
                row.get::<_, i64>(0)
            })
        })
        .transpose()
        .context("missing node owner")?;
    conn.execute("INSERT INTO nodes(id,label,kind,file,line,end_line,qualified_name,binding_key,metadata,owner_key,search) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![node.id,node.label,node.kind,node.file,node.line,node.end_line,node.qualified_name,node.binding_key,serde_json::to_string(&node.metadata)?,owner,search_text(node)])?;
    Ok(())
}

pub(super) fn insert_edge(
    conn: &Connection,
    edge: &Edge,
    owner: Option<&str>,
    reference: Option<&str>,
) -> Result<()> {
    ensure!(!edge.id.is_empty(), "edge ID cannot be empty");
    let owner = owner
        .map(|path| {
            conn.query_row("SELECT fkey FROM files WHERE path=?1", [path], |row| {
                row.get::<_, i64>(0)
            })
        })
        .transpose()
        .context("missing edge owner")?;
    let reference = reference
        .map(|id| {
            conn.query_row("SELECT rkey FROM refs WHERE id=?1", [id], |row| {
                row.get::<_, i64>(0)
            })
        })
        .transpose()
        .context("missing edge reference")?;
    conn.execute("INSERT INTO edges(id,source,target,source_key,target_key,relation,directed,file,line,confidence,metadata,owner_key,ref_key) VALUES(?1,?2,?3,(SELECT nkey FROM nodes WHERE id=?2),(SELECT nkey FROM nodes WHERE id=?3),?4,?5,?6,?7,?8,?9,?10,?11)",
        params![edge.id,edge.source,edge.target,edge.relation,edge.directed,edge.file,edge.line,edge.confidence,serde_json::to_string(&edge.metadata)?,owner,reference])?;
    Ok(())
}

pub(super) fn resolve_reference(
    tx: &Transaction<'_>,
    id: &str,
    payload_statement: &mut Statement<'_>,
    delete_edges: &mut Statement<'_>,
    candidate_keys: &mut Statement<'_>,
    update_resolution: &mut Statement<'_>,
) -> Result<()> {
    let payload: String = payload_statement.query_row([id], |r| r.get(0))?;
    let reference: Reference = serde_json::from_str(&payload)?;
    delete_edges.execute([id])?;
    let mut target = None;
    let mut reason = if reference.reason.is_empty() {
        "no matching binding".to_owned()
    } else {
        reference.reason.clone()
    };
    let keys = candidate_keys
        .query_map([id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for key in &keys {
        let mut stmt = tx.prepare(
            "SELECT id FROM nodes WHERE binding_key=?1
                        UNION SELECT n.id FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key WHERE a.binding_key=?1
                        ORDER BY 1 LIMIT 2",
        )?;
        let ids = stmt
            .query_map([key], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        match ids.as_slice() {
            [] => continue,
            [id] => {
                target = Some(id.clone());
                reason.clear();
                break;
            }
            _ => {
                reason = format!("ambiguous binding: {key}");
                break;
            }
        }
    }
    update_resolution.execute(params![target, reason, id])?;
    if let Some(target) = target {
        let source_payload: String = tx.query_row(
            "SELECT payload FROM nodes WHERE id=?1",
            [&reference.source],
            |row| row.get(0),
        )?;
        let source: Node = serde_json::from_str(&source_payload)?;
        let edge = resolved_reference_edge(&reference, &source, &target);
        insert_edge(tx, &edge, Some(&reference.file), Some(id))?;
    }
    Ok(())
}

pub(super) fn read_stats(conn: &Connection) -> Result<Stats> {
    let (generation, kind, root, coverage): (i64, String, Option<String>, String) = conn
        .query_row(
            "SELECT generation,kind,root,coverage FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let layout = storage_layout(conn)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let mut diagnostics = Vec::new();
    let mut stmt = conn.prepare("SELECT diagnostics FROM files ORDER BY path")?;
    for json in stmt.query_map([], |r| r.get::<_, String>(0))? {
        diagnostics.extend(serde_json::from_str::<Vec<Diagnostic>>(&json?)?);
    }
    let count = |sql| -> Result<usize> {
        let value: i64 = conn.query_row(sql, [], |r| r.get(0))?;
        Ok(usize::try_from(value)?)
    };
    let stored_counts = if version >= 5 {
        let counts: (i64, i64, i64, i64) = conn.query_row(
            "SELECT files,nodes,edges,unresolved_references FROM storage_counts WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Some((
            usize::try_from(counts.0)?,
            usize::try_from(counts.1)?,
            usize::try_from(counts.2)?,
            usize::try_from(counts.3)?,
        ))
    } else {
        None
    };
    Ok(Stats {
        schema_version: SCHEMA_VERSION,
        generation: u64::try_from(generation)?,
        root,
        nodes: stored_counts.as_ref().map_or_else(
            || count("SELECT count(*) FROM nodes"),
            |counts| Ok(counts.1),
        )?,
        edges: stored_counts.as_ref().map_or_else(
            || count("SELECT count(*) FROM edges"),
            |counts| Ok(counts.2),
        )?,
        files: if kind == "imported" {
            count("SELECT count(DISTINCT file) FROM nodes WHERE file<>''")?
        } else {
            stored_counts.as_ref().map_or_else(
                || count("SELECT count(*) FROM files"),
                |counts| Ok(counts.0),
            )?
        },
        unresolved_references: stored_counts.as_ref().map_or_else(
            || {
                count(match layout {
                    StorageLayout::Legacy => {
                        "SELECT count(*) FROM refs WHERE resolved_target IS NULL"
                    }
                    StorageLayout::Compact => {
                        "SELECT count(*) FROM refs WHERE resolved_target_key IS NULL"
                    }
                })
            },
            |counts| Ok(counts.3),
        )?,
        kind,
        coverage: serde_json::from_str(&coverage)?,
        diagnostics,
    })
}
