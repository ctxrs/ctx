use super::*;

pub(super) fn budgeted<T>(conn: &Connection, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let _budget = QueryBudget::install(conn)?;
    query_errors(work())
}

pub(super) fn catalog_budgeted<T>(
    conn: &Connection,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _budget = QueryBudget::with_steps(conn, 20_000)?;
    query_errors(work())
}

pub(super) fn query_errors<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| {
        if matches!(error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::OperationInterrupted) {
            error.context("query exceeded its SQLite work/time budget; use a more specific symbol, fewer search terms, or a smaller depth/limit")
        } else { error }
    })
}

pub(super) fn validate(options: &QueryOptions) -> Result<()> {
    ensure!(options.depth <= 6, "depth must be between 0 and 6");
    ensure!(
        (1..=500).contains(&options.limit),
        "limit must be between 1 and 500"
    );
    Ok(())
}

pub(super) fn empty(conn: &Connection) -> Result<GraphResult> {
    let generation = generation(conn)?;
    // Generation pins the read snapshot before selecting physical-format SQL.
    // A Store handle may observe an upgrade in its next transaction.
    storage_layout(conn)?;
    Ok(GraphResult {
        schema_version: SCHEMA_VERSION,
        generation,
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        truncated: false,
    })
}

pub(super) fn node_identity(
    conn: &Connection,
    layout: StorageLayout,
    id: &str,
) -> Result<rusqlite::types::Value> {
    Ok(match layout {
        StorageLayout::Legacy => id.to_owned().into(),
        StorageLayout::Compact => conn
            .query_row("SELECT nkey FROM nodes WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })?
            .into(),
    })
}

pub(super) fn node(conn: &Connection, id: &str) -> Result<Option<Node>> {
    let json: Option<String> = conn
        .query_row("SELECT payload FROM nodes WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    json.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

pub(super) fn exact(conn: &Connection, text: &str, include_file: bool) -> Result<Vec<Node>> {
    if let Some(node) = node(conn, text)? {
        return Ok(vec![node]);
    }
    let mut matches = BTreeMap::new();
    let columns: &[&str] = if include_file {
        &["label", "qualified_name", "file"]
    } else {
        &["label", "qualified_name"]
    };
    for column in columns {
        // Each index range is limited before merging, including common labels.
        let sql = format!("SELECT payload FROM nodes WHERE {column}=?1 ORDER BY id LIMIT ?2");
        let mut stmt = conn.prepare(&sql)?;
        for json in stmt.query_map(params![text, (MAX_SEEDS + 1) as i64], |r| {
            r.get::<_, String>(0)
        })? {
            let n: Node = serde_json::from_str(&json?)?;
            matches.insert(n.id.clone(), n);
        }
    }
    Ok(matches.into_values().take(MAX_SEEDS + 1).collect())
}

pub(super) fn unique(conn: &Connection, text: &str) -> Result<Node> {
    let nodes = exact(conn, text, false)?;
    match nodes.len() {
        0 => bail!("no symbol matches {text:?}"),
        1 => Ok(nodes.into_iter().next().unwrap()),
        _ => {
            let ids: Vec<_> = nodes.iter().map(|n| n.id.as_str()).collect();
            bail!(
                "ambiguous symbol {text:?}; use an exact ID: {}{}",
                ids.join(", "),
                if nodes.len() > MAX_SEEDS {
                    " (additional matches may exist)"
                } else {
                    ""
                }
            )
        }
    }
}

pub(super) fn query_snapshot(
    conn: &Connection,
    text: &str,
    options: &QueryOptions,
) -> Result<GraphResult> {
    validate(options)?;
    let text = text.trim();
    ensure!(!text.is_empty(), "query cannot be empty");
    ensure!(text.len() <= 1024, "query exceeds 1024 bytes");
    let tx = conn.unchecked_transaction()?;
    // Reading generation pins the snapshot before any seeds are selected.
    let mut result = empty(&tx)?;
    let mut seeds = exact(&tx, text, true)?;
    if seeds.is_empty() {
        let tokens: Vec<_> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .take(9)
            .collect();
        ensure!(!tokens.is_empty(), "query must contain a searchable word");
        ensure!(
            tokens.len() <= 8,
            "query must contain at most 8 search terms"
        );
        // Literal prefix tokens only: callers cannot inject FTS operators.
        // Rowid order can stop at the candidate cap; no global relevance sort.
        let expression = tokens
            .iter()
            .map(|t| format!("\"{t}\"*"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut stmt = tx.prepare("SELECT n.payload FROM node_search s JOIN nodes n ON n.rowid=s.rowid WHERE node_search MATCH ?1 ORDER BY s.rowid LIMIT ?2")?;
        seeds = stmt
            .query_map(params![expression, (MAX_SEEDS + 1) as i64], |r| {
                r.get::<_, String>(0)
            })?
            .map(|json| Ok(serde_json::from_str(&json?)?))
            .collect::<Result<Vec<_>>>()?;
    }
    if seeds.len() > MAX_SEEDS {
        result.truncated = true;
        seeds.truncate(MAX_SEEDS);
    }
    traverse(&tx, seeds, options, &mut result)?;
    tx.commit()?;
    Ok(result)
}

pub(super) fn neighbors_snapshot(
    conn: &Connection,
    symbol: &str,
    options: &QueryOptions,
) -> Result<GraphResult> {
    validate(options)?;
    let tx = conn.unchecked_transaction()?;
    let mut result = empty(&tx)?;
    let seed = unique(&tx, symbol)?;
    traverse(&tx, vec![seed], options, &mut result)?;
    tx.commit()?;
    Ok(result)
}

pub(super) fn adjacency(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    budget: usize,
) -> Result<Vec<Edge>> {
    adjacency_filtered(conn, id, options, budget, &[])
}

pub(super) fn adjacency_filtered(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    budget: usize,
    relations: &[String],
) -> Result<Vec<Edge>> {
    let layout = storage_layout(conn)?;
    let identity = node_identity(conn, layout, id)?;
    let mut edges = Vec::new();
    let selected_relations = relation_selection(options.relation.as_deref(), relations);
    let streams = if options.direction == Direction::Incoming {
        [false, true]
    } else {
        [true, false]
    };
    for outgoing in streams {
        let remaining = budget - edges.len();
        if remaining == 0 {
            break;
        }
        let undirected_only = matches!(
            (options.direction, outgoing),
            (Direction::Incoming, true) | (Direction::Outgoing, false)
        );
        // Do not filter self-loops in SQL: even rejected rows could make a
        // LIMIT scan an entire hub. Duplicates consume the budget and are
        // removed only after bounded retrieval.
        edges.extend(edge_relation_rows(
            conn,
            layout,
            &identity,
            outgoing,
            undirected_only,
            selected_relations.as_deref(),
            remaining,
        )?);
    }
    Ok(edges)
}

pub(super) fn relation_selection(
    explicit: Option<&str>,
    relations: &[String],
) -> Option<Vec<String>> {
    match explicit {
        Some(relation) => Some(
            if relations.is_empty() || relations.iter().any(|candidate| candidate == relation) {
                vec![relation.to_owned()]
            } else {
                Vec::new()
            },
        ),
        None if relations.is_empty() => None,
        None => Some(
            relations
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        ),
    }
}

pub(super) fn stream_relations(
    conn: &Connection,
    stream: &str,
    identity: &rusqlite::types::Value,
) -> Result<Vec<String>> {
    let mut relations = Vec::new();
    let mut previous: Option<String> = None;
    loop {
        let relation = if let Some(previous) = &previous {
            conn.query_row(
                &format!("SELECT relation FROM {stream} AND relation>?2 ORDER BY relation LIMIT 1"),
                params![identity.clone(), previous],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        } else {
            conn.query_row(
                &format!("SELECT relation FROM {stream} ORDER BY relation LIMIT 1"),
                [identity.clone()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        };
        let Some(relation) = relation else {
            break;
        };
        previous = Some(relation.clone());
        relations.push(relation);
    }
    Ok(relations)
}

pub(super) fn edge_relation_rows(
    conn: &Connection,
    layout: StorageLayout,
    identity: &rusqlite::types::Value,
    outgoing: bool,
    undirected_only: bool,
    selected_relations: Option<&[String]>,
    limit: usize,
) -> Result<Vec<Edge>> {
    if limit == 0 || selected_relations.is_some_and(<[String]>::is_empty) {
        return Ok(Vec::new());
    }
    let endpoint = if outgoing { "source" } else { "target" };
    let column = match (layout, outgoing) {
        (StorageLayout::Legacy, true) => "source",
        (StorageLayout::Legacy, false) => "target",
        (StorageLayout::Compact, true) => "source_key",
        (StorageLayout::Compact, false) => "target_key",
    };
    let index = format!(
        "edges_{endpoint}{}_relation",
        if undirected_only { "_direction" } else { "" }
    );
    let stream = format!(
        "edges INDEXED BY {index} WHERE {column}=?1{}",
        if undirected_only {
            " AND directed=0"
        } else {
            ""
        }
    );
    let relations = match selected_relations {
        Some(relations) => relations.to_vec(),
        None => stream_relations(conn, &stream, identity)?,
    };
    let mut first = conn.prepare(&format!(
        "SELECT id,payload FROM {stream} AND relation=?2 ORDER BY id LIMIT 1"
    ))?;
    let mut next = conn.prepare(&format!(
        "SELECT id,payload FROM {stream} AND relation=?2 AND id>?3 ORDER BY id LIMIT 1"
    ))?;
    let mut pending = BinaryHeap::new();
    for relation in relations {
        if let Some((id, payload)) = first
            .query_row(params![identity.clone(), relation], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?
        {
            pending.push(Reverse((id, relation, payload)));
        }
    }
    let mut edges = Vec::new();
    while edges.len() < limit {
        let Some(Reverse((id, relation, payload))) = pending.pop() else {
            break;
        };
        edges.push(serde_json::from_str(&payload)?);
        if let Some((next_id, next_payload)) = next
            .query_row(params![identity.clone(), relation, id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?
        {
            pending.push(Reverse((next_id, relation, next_payload)));
        }
    }
    Ok(edges)
}

pub(super) fn unresolved_relation_rows(
    conn: &Connection,
    layout: StorageLayout,
    identity: &rusqlite::types::Value,
    relation: Option<&str>,
    limit: usize,
) -> Result<Vec<(Reference, String)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let stream = match layout {
        StorageLayout::Legacy => {
            "refs INDEXED BY refs_unresolved_relation WHERE source=?1 AND resolved_target IS NULL"
        }
        StorageLayout::Compact => {
            "refs INDEXED BY refs_unresolved_relation WHERE source_key=?1 AND resolved_target_key IS NULL"
        }
    };
    let relations = match relation {
        Some(relation) => vec![relation.to_owned()],
        None => stream_relations(conn, stream, identity)?,
    };
    let mut first = conn.prepare(&format!(
        "SELECT id,payload,resolution_reason FROM {stream} AND relation=?2 ORDER BY id LIMIT 1"
    ))?;
    let mut next = conn.prepare(&format!(
        "SELECT id,payload,resolution_reason FROM {stream} AND relation=?2 AND id>?3 ORDER BY id LIMIT 1"
    ))?;
    let mut pending = BinaryHeap::new();
    for relation in relations {
        if let Some((id, payload, reason)) = first
            .query_row(params![identity.clone(), relation], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?
        {
            pending.push(Reverse((id, relation, payload, reason)));
        }
    }
    let mut references = Vec::new();
    while references.len() < limit {
        let Some(Reverse((id, relation, payload, reason))) = pending.pop() else {
            break;
        };
        references.push((serde_json::from_str(&payload)?, reason));
        if let Some((next_id, next_payload, next_reason)) = next
            .query_row(params![identity.clone(), relation, id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?
        {
            pending.push(Reverse((next_id, relation, next_payload, next_reason)));
        }
    }
    Ok(references)
}

pub(super) fn opposite<'a>(edge: &'a Edge, id: &str) -> &'a str {
    if edge.source == id {
        &edge.target
    } else {
        &edge.source
    }
}

pub(super) fn unresolved(
    conn: &Connection,
    id: &str,
    options: &QueryOptions,
    result: &mut GraphResult,
) -> Result<()> {
    if options.direction == Direction::Incoming {
        return Ok(());
    }
    let remaining = options.limit - result.unresolved.len();
    let layout = storage_layout(conn)?;
    let identity = node_identity(conn, layout, id)?;
    let rows = unresolved_relation_rows(
        conn,
        layout,
        &identity,
        options.relation.as_deref(),
        remaining + 1,
    )?;
    for (seen, (reference, reason)) in rows.into_iter().enumerate() {
        if seen == remaining {
            result.truncated = true;
            break;
        }
        result.unresolved.push(UnresolvedReference {
            source: reference.source,
            label: reference.label,
            relation: reference.relation,
            file: reference.file,
            line: reference.line,
            reason,
        });
    }
    Ok(())
}

pub(super) fn traverse(
    conn: &Connection,
    seeds: Vec<Node>,
    options: &QueryOptions,
    result: &mut GraphResult,
) -> Result<()> {
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut edge_ids = BTreeSet::new();
    for n in seeds {
        if result.nodes.len() == options.limit {
            result.truncated = true;
            break;
        }
        if visited.insert(n.id.clone()) {
            queue.push_back((n.id.clone(), 0));
            result.nodes.push(n);
        }
    }
    let mut examined = 0;
    while let Some((id, depth)) = queue.pop_front() {
        unresolved(conn, &id, options, result)?;
        if depth >= options.depth {
            continue;
        }
        if examined == MAX_EXAMINED {
            result.truncated = true;
            break;
        }
        let edges = adjacency(conn, &id, options, MAX_EXAMINED - examined)?;
        examined += edges.len();
        if examined == MAX_EXAMINED {
            result.truncated = true;
        }
        for edge in edges {
            if edge_ids.contains(&edge.id) {
                continue;
            }
            let next = opposite(&edge, &id);
            if !visited.contains(next) {
                if result.nodes.len() == options.limit {
                    result.truncated = true;
                    continue;
                }
                let n = node(conn, next)?
                    .ok_or_else(|| anyhow::anyhow!("edge points to missing node {next}"))?;
                visited.insert(next.to_owned());
                queue.push_back((next.to_owned(), depth + 1));
                result.nodes.push(n);
            }
            edge_ids.insert(edge.id.clone());
            result.edges.push(edge);
        }
    }
    Ok(())
}

pub(super) fn path_snapshot(
    conn: &Connection,
    source: &str,
    target: &str,
    options: &QueryOptions,
) -> Result<PathResult> {
    validate(options)?;
    let tx = conn.unchecked_transaction()?;
    let mut result = empty(&tx)?;
    let start = unique(&tx, source)?;
    let end = unique(&tx, target)?;
    let mut visited = BTreeSet::from([start.id.clone()]);
    let mut queue = VecDeque::from([(start.id.clone(), 0)]);
    let mut parents: BTreeMap<String, (String, Edge)> = BTreeMap::new();
    let mut examined = 0;
    let mut found = start.id == end.id;
    'search: while !found {
        let Some((id, depth)) = queue.pop_front() else {
            break;
        };
        if examined == MAX_EXAMINED {
            result.truncated = true;
            break;
        }
        let edges = adjacency(&tx, &id, options, MAX_EXAMINED - examined)?;
        examined += edges.len();
        if examined == MAX_EXAMINED {
            result.truncated = true;
        }
        for edge in edges {
            let next = opposite(&edge, &id).to_owned();
            if visited.contains(&next) {
                continue;
            }
            if depth >= options.depth || visited.len() >= options.limit {
                result.truncated = true;
                continue;
            }
            visited.insert(next.clone());
            parents.insert(next.clone(), (id.clone(), edge));
            if next == end.id {
                found = true;
                break 'search;
            }
            queue.push_back((next, depth + 1));
        }
    }
    if found {
        let mut id = end.id.clone();
        let mut ids = vec![id.clone()];
        while id != start.id {
            let (previous, edge) = parents.remove(&id).expect("visited path has a predecessor");
            result.edges.push(edge);
            ids.push(previous.clone());
            id = previous;
        }
        result.edges.reverse();
        ids.reverse();
        for id in ids {
            result
                .nodes
                .push(node(&tx, &id)?.expect("path node exists in this snapshot"));
        }
    } else {
        // Include the resolved endpoints for an actionable unsuccessful result,
        // without implying they are connected or exceeding the node limit.
        result.nodes.push(start);
        if options.limit > 1 && result.nodes[0].id != end.id {
            result.nodes.push(end);
        }
    }
    tx.commit()?;
    Ok(PathResult {
        found,
        graph: result,
    })
}

pub(super) fn validate_search(options: &SearchOptions) -> Result<()> {
    validate(&options.graph)?;
    for filters in [&options.contexts, &options.files, &options.kinds] {
        ensure!(filters.len() <= 32, "at most 32 values per filter");
        ensure!(
            filters
                .iter()
                .all(|s| !s.trim().is_empty() && s.len() <= 1024),
            "filters must be nonempty and at most 1024 bytes each"
        );
    }
    ensure!(
        options
            .token_budget
            .is_none_or(|n| (1..=1_000_000).contains(&n)),
        "token budget must be between 1 and 1000000"
    );
    ensure!(
        options
            .graph
            .relation
            .as_ref()
            .is_none_or(|s| !s.is_empty() && s.len() <= 1024),
        "relation must be nonempty and at most 1024 bytes"
    );
    Ok(())
}

pub(super) fn validate_text(text: &str) -> Result<&str> {
    let text = text.trim();
    ensure!(
        !text.is_empty() && text.len() <= 1024,
        "query/endpoint must be nonempty and at most 1024 bytes"
    );
    Ok(text)
}

pub(super) fn normalize(text: &str) -> String {
    use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
    text.nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn context_alias(text: &str) -> String {
    let text = normalize(text.trim());
    match text.as_str() {
        "calls" | "called" | "caller" | "callers" | "invoke" | "invokes" | "invoked"
        | "invocation" => "call".into(),
        "imports" | "imported" | "module" | "modules" => "import".into(),
        "exports" | "exported" => "export".into(),
        "fields" | "property" | "properties" | "member" | "members" => "field".into(),
        "param" | "params" | "parameter" | "parameters" | "argument" | "arguments" | "arg"
        | "args" => "parameter_type".into(),
        "return" | "returns" | "returned" => "return_type".into(),
        "generic" | "generics" | "template" | "templates" => "generic_arg".into(),
        "annotation" | "annotations" | "decorator" | "decorators" => "attribute".into(),
        "references" | "referenced" => "reference".into(),
        _ => text,
    }
}

pub(super) fn contexts(text: &str, options: &SearchOptions) -> Vec<String> {
    let mut values: BTreeSet<_> = options.contexts.iter().map(|s| context_alias(s)).collect();
    if values.is_empty() && options.infer_context {
        for token in text.split(|c: char| !c.is_alphanumeric()) {
            let context = context_alias(token);
            if matches!(
                context.as_str(),
                "call"
                    | "import"
                    | "export"
                    | "field"
                    | "parameter_type"
                    | "return_type"
                    | "generic_arg"
                    | "attribute"
                    | "reference"
            ) {
                values.insert(context);
            }
        }
    }
    values.into_iter().collect()
}

pub(super) fn attributes(mut value: &serde_json::Value) -> &serde_json::Value {
    while let Some(original) = value.get("original_metadata") {
        value = original;
    }
    value
}

pub(super) fn context_matches(edge: &Edge, contexts: &[String]) -> bool {
    if contexts.is_empty() {
        return true;
    }
    let attrs = attributes(&edge.metadata);
    match attrs.get("context") {
        Some(serde_json::Value::String(value)) => contexts.contains(&context_alias(value)),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|v| contexts.contains(&context_alias(v))),
        Some(_) => false,
        // Native facts often have a relation but no redundant context attribute.
        None => contexts.contains(&context_alias(&edge.relation)),
    }
}

pub(super) fn node_matches(node: &Node, options: &SearchOptions) -> bool {
    (options.files.is_empty() || options.files.contains(&node.file))
        && (options.kinds.is_empty() || options.kinds.contains(&node.kind))
}

pub(super) fn filter_sql(
    conn: &Connection,
    options: &SearchOptions,
    values: &mut Vec<rusqlite::types::Value>,
) -> Result<String> {
    let mut sql = String::new();
    let kind = if normalized_storage(conn)? {
        "n.kind"
    } else {
        "json_extract(n.payload,'$.kind')"
    };
    for (column, filters) in [("n.file", &options.files), (kind, &options.kinds)] {
        if !filters.is_empty() {
            sql.push_str(&format!(
                " AND {column} IN ({})",
                vec!["?"; filters.len()].join(",")
            ));
            values.extend(filters.iter().cloned().map(Into::into));
        }
    }
    Ok(sql)
}
