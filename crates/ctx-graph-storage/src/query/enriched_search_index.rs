use super::*;

pub(super) fn enriched_search_index(conn: &Connection) -> Result<bool> {
    let has_version: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('metadata') WHERE name='search_version')",
        [],
        |row| row.get(0),
    )?;
    if !has_version {
        return Ok(false);
    }
    Ok(conn.query_row(
        // Only v5 promises the explicit NFKD/accent-folded companion. Older
        // unicode61 postings miss compound diacritics; future formats need review.
        "SELECT search_version = 5 FROM metadata WHERE singleton=1",
        [],
        |row| row.get(0),
    )?)
}

pub(super) fn within_endpoint_fts_bounds(conn: &Connection) -> Result<bool> {
    // nodes.search is the complete input maintained in node_search by writes.
    // Do not apply endpoint filters: prefix setup visits global postings, and
    // named prose/attributes can dwarf the ID/label/qualified-name fields.
    // Read lengths only and stop at the first exceeded cap. A large SQLite
    // value or historical FTS segments can still cost I/O outside these caps.
    let mut stmt = conn.prepare("SELECT length(CAST(search AS BLOB)) FROM nodes LIMIT ?")?;
    let mut rows = stmt.query([(MAX_ENDPOINT_FTS_NODES + 1) as i64])?;
    let mut remaining = MAX_ENDPOINT_FTS_BYTES;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let bytes = usize::try_from(row.get::<_, i64>(0)?)?;
        if count == MAX_ENDPOINT_FTS_NODES || bytes > remaining {
            return Ok(false);
        }
        count += 1;
        remaining -= bytes;
    }
    Ok(true)
}

pub(super) fn within_endpoint_bounds(
    conn: &Connection,
    filters: &str,
    values: &[rusqlite::types::Value],
) -> Result<bool> {
    let record_bytes = "length(CAST(n.id AS BLOB)) + length(CAST(n.label AS BLOB)) + COALESCE(length(CAST(n.qualified_name AS BLOB)), 0)";
    let sql = format!(
        "SELECT COUNT(*) <= {MAX_RANK_POSTINGS}
                AND COALESCE(SUM({record_bytes}), 0) <= {MAX_RANK_BYTES}
                AND COALESCE(MAX({record_bytes}), 0) <= {MAX_SEARCH_BYTES}
         FROM nodes n WHERE 1=1{filters}"
    );
    let mut stmt = conn.prepare(&sql)?;
    Ok(
        stmt.query_row(rusqlite::params_from_iter(values.iter().cloned()), |row| {
            row.get(0)
        })?,
    )
}

pub(super) fn endpoint_tier(
    id: &str,
    label: &str,
    qualified: Option<&str>,
    term: &str,
    callable: &str,
    qualified_tail: bool,
) -> Option<usize> {
    let normalized = [
        normalize(id),
        normalize(label),
        normalize(qualified.unwrap_or("")),
    ];
    if normalized.iter().any(|value| value == term) || normalized[1] == callable {
        Some(0)
    } else if qualified_tail
        && normalized[2].strip_suffix(term).is_some_and(|prefix| {
            prefix.is_empty()
                || prefix
                    .chars()
                    .next_back()
                    .is_some_and(|c| c == '.' || c.is_whitespace())
        })
    {
        Some(1)
    } else if normalized.iter().any(|value| value.starts_with(term)) {
        Some(2)
    } else if normalized.iter().any(|value| value.contains(term)) {
        Some(3)
    } else {
        None
    }
}

pub(super) fn scan_endpoint_rows(
    conn: &Connection,
    sql: &str,
    values: &[rusqlite::types::Value],
    term: &str,
    callable: &str,
    qualified_tail: bool,
) -> Result<[Vec<String>; 4]> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(values.iter().cloned()))?;
    let mut tiers: [Vec<String>; 4] = Default::default();
    let start = Instant::now();
    let mut bytes = 0;
    let mut examined = 0;
    while let Some(row) = rows.next()? {
        ensure!(
            examined < MAX_RANK_POSTINGS && start.elapsed() < Duration::from_secs(2),
            "endpoint lookup exceeded its work/time budget; use an exact ID or a smaller file/kind scope"
        );
        examined += 1;
        let id: &str = row.get_ref(0)?.as_str()?;
        let label: &str = row.get_ref(1)?.as_str()?;
        let qualified: Option<&str> = row.get_ref(2)?.as_str_or_null()?;
        // Normalization runs outside SQLite's VM guard. Bound its input too,
        // before allocating strings for arbitrarily long imported identifiers.
        let record_bytes = id.len() + label.len() + qualified.map_or(0, str::len);
        bytes += record_bytes;
        ensure!(
            record_bytes <= MAX_SEARCH_BYTES && bytes <= MAX_RANK_BYTES,
            "endpoint lookup exceeded its normalization byte budget; use an exact ID or a smaller file/kind scope"
        );
        if let Some(tier) = endpoint_tier(id, label, qualified, term, callable, qualified_tail)
            && tiers[tier].len() < 2
        {
            tiers[tier].push(id.to_owned());
        }
        if tiers[0].len() == 2 {
            break;
        }
    }
    Ok(tiers)
}

pub(super) fn exact_filtered(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
    include_file: bool,
) -> Result<Vec<Node>> {
    if let Some(node) = node(conn, text)? {
        return Ok(if node_matches(&node, options) {
            vec![node]
        } else {
            Vec::new()
        });
    }
    let mut matches = BTreeMap::new();
    let columns: &[&str] = if include_file {
        &["label", "qualified_name", "file"]
    } else {
        &["label", "qualified_name"]
    };
    for column in columns {
        let mut values: Vec<rusqlite::types::Value> = vec![text.to_owned().into()];
        let filters = filter_sql(conn, options, &mut values)?;
        let sql = format!(
            "SELECT n.payload FROM nodes n WHERE n.{column}=?{filters} ORDER BY n.id LIMIT {}",
            MAX_SEEDS + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        for row in stmt.query_map(rusqlite::params_from_iter(values), |r| {
            r.get::<_, String>(0)
        })? {
            let node: Node = serde_json::from_str(&row?)?;
            matches.insert(node.id.clone(), node);
        }
    }
    if !matches.is_empty() {
        return Ok(matches.into_values().take(MAX_SEEDS + 1).collect());
    }
    // A literal ID/label containing :: wins. Otherwise this is a strict file
    // scope: an absent match must not drift to a similarly named foreign file.
    if let Some((file, symbol)) = text.split_once("::") {
        ensure!(
            !file.is_empty() && !symbol.is_empty(),
            "scoped endpoint requires file::symbol"
        );
        // Normalize only the scope, after raw whole-input matches. Exact symbol
        // spelling must win for ./ and in-root absolute paths before accent
        // folding; rebuilding the whole input could select an unrelated ID.
        let file = source_path(conn, file)?;
        for column in ["id", "label", "qualified_name"] {
            let mut values: Vec<rusqlite::types::Value> =
                vec![file.to_owned().into(), symbol.to_owned().into()];
            let filters = filter_sql(conn, options, &mut values)?;
            let sql = format!(
                "SELECT n.payload FROM nodes n WHERE n.file=? AND n.{column}=?{filters} ORDER BY n.id LIMIT {}",
                MAX_SEEDS + 1
            );
            let mut stmt = conn.prepare(&sql)?;
            for row in stmt.query_map(rusqlite::params_from_iter(values), |r| {
                r.get::<_, String>(0)
            })? {
                let node: Node = serde_json::from_str(&row?)?;
                matches.insert(node.id.clone(), node);
            }
        }
    }
    Ok(matches.into_values().take(MAX_SEEDS + 1).collect())
}

pub(super) fn unique_filtered(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
) -> Result<Node> {
    let nodes = exact_filtered(conn, validate_text(text)?, options, false)?;
    require_unique(nodes, text)
}

pub(super) fn require_unique(nodes: Vec<Node>, text: &str) -> Result<Node> {
    match nodes.len() {
        0 => bail!("no symbol matches {text:?} within the requested scope"),
        1 => Ok(nodes.into_iter().next().unwrap()),
        _ => bail!(
            "ambiguous symbol {text:?}; use an exact ID or file::symbol: {}{}",
            nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if nodes.len() > MAX_SEEDS {
                " (additional matches may exist)"
            } else {
                ""
            }
        ),
    }
}

pub(super) fn resolve_relation(
    conn: &Connection,
    id: &str,
    text: &str,
    options: &QueryOptions,
) -> Result<String> {
    let layout = storage_layout(conn)?;
    let identity = node_identity(conn, layout, id)?;
    let mut streams = Vec::new();
    let columns = match layout {
        StorageLayout::Legacy => ["source", "target"],
        StorageLayout::Compact => ["source_key", "target_key"],
    };
    for (column, outgoing) in [(columns[0], true), (columns[1], false)] {
        let undirected_only = matches!(
            (options.direction, outgoing),
            (Direction::Incoming, true) | (Direction::Outgoing, false)
        );
        streams.push(format!(
            "edges INDEXED BY edges_{}{}_relation WHERE {column}=?1{}",
            if outgoing { "source" } else { "target" },
            if undirected_only { "_direction" } else { "" },
            if undirected_only {
                " AND directed=0"
            } else {
                ""
            }
        ));
    }
    if options.direction != Direction::Incoming {
        streams.push(
            match layout {
                StorageLayout::Legacy => "refs INDEXED BY refs_unresolved_relation WHERE source=?1 AND resolved_target IS NULL",
                StorageLayout::Compact => {
                    "refs INDEXED BY refs_unresolved_relation WHERE source_key=?1 AND resolved_target_key IS NULL"
                }
            }
            .into(),
        );
    }
    // Exact relation probes use directional relation indexes even for huge hubs.
    for stream in &streams {
        if conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {stream} AND relation=?2)"),
            params![identity, text],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(text.to_owned());
        }
    }
    let term = normalize(text.trim());
    ensure!(
        !term.is_empty(),
        "relation has no searchable normalized spelling"
    );
    let mut tiers: [BTreeSet<String>; 3] = Default::default();
    let start = Instant::now();
    let mut examined = 0;
    let mut bytes = 0;
    for stream in &streams {
        // Seek to the next distinct indexed name. SELECT DISTINCT would still
        // walk every edge at a high-degree hub before discovering a second name.
        let mut first = conn.prepare(&format!(
            "SELECT relation FROM {stream} ORDER BY relation LIMIT 1"
        ))?;
        let mut next = conn.prepare(&format!(
            "SELECT relation FROM {stream} AND relation>?2 ORDER BY relation LIMIT 1"
        ))?;
        let mut relation: Option<String> = first.query_row([&identity], |r| r.get(0)).optional()?;
        while let Some(name) = relation {
            ensure!(
                examined < MAX_EXAMINED && start.elapsed() < Duration::from_secs(2),
                "relation lookup exceeded its work/time budget; use an exact relation"
            );
            examined += 1;
            bytes += name.len();
            ensure!(
                bytes <= MAX_SEARCH_BYTES,
                "relation lookup exceeded its byte budget; use an exact relation"
            );
            let folded = normalize(&name);
            let tier = if folded == term {
                Some(0)
            } else if folded.starts_with(&term) {
                Some(1)
            } else if folded.contains(&term) {
                Some(2)
            } else {
                None
            };
            if let Some(tier) = tier {
                // Keep the lexically first two distinct names for a stable error.
                tiers[tier].insert(name.clone());
                if tiers[tier].len() > 2 {
                    tiers[tier].pop_last();
                }
            }
            relation = next
                .query_row(params![identity, name], |r| r.get(0))
                .optional()?;
        }
    }
    let matches = tiers
        .into_iter()
        .find(|names| !names.is_empty())
        .unwrap_or_default();
    ensure!(
        matches.len() <= 1,
        "ambiguous relation {text:?}; use an exact relation: {}",
        matches.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    // An unmatched filter must stay restrictive, never become all relations.
    Ok(matches
        .into_iter()
        .next()
        .unwrap_or_else(|| text.to_owned()))
}

pub(super) fn source_path(conn: &Connection, text: &str) -> Result<String> {
    use std::path::{Component, Path};
    let root: Option<String> =
        conn.query_row("SELECT root FROM metadata WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    let original = Path::new(text);
    let path = if original.is_absolute() {
        root.as_deref()
            .and_then(|root| original.strip_prefix(root).ok())
            .unwrap_or(original)
    } else {
        original
    };
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(value) => parts.push(value.to_string_lossy()),
            Component::CurDir => {}
            _ => return Ok(text.to_owned()),
        }
    }
    Ok(parts.join("/"))
}

pub(super) fn file_root(node: &Node, file: &str) -> bool {
    matches!(node.kind.as_str(), "file" | "module")
        || (node.line == Some(1)
            && std::path::Path::new(file)
                .file_name()
                .and_then(|s| s.to_str())
                == Some(node.label.as_str()))
}

pub(super) fn file_nodes(
    conn: &Connection,
    file: &str,
    options: &SearchOptions,
    limit: usize,
) -> Result<Vec<Node>> {
    let basename = std::path::Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    let mut values: Vec<rusqlite::types::Value> = vec![file.to_owned().into()];
    let filters = filter_sql(conn, options, &mut values)?;
    values.push(basename.to_owned().into());
    values.push((limit as i64).into());
    let root_order = if normalized_storage(conn)? {
        "n.kind IN ('file','module') OR (n.line=1 AND n.label=?)"
    } else {
        "json_extract(n.payload,'$.kind') IN ('file','module') OR (json_extract(n.payload,'$.line')=1 AND n.label=?)"
    };
    let sql = format!(
        "SELECT n.payload FROM nodes n WHERE n.file=?{filters}
        ORDER BY CASE WHEN {root_order} THEN 0 ELSE 1 END,n.id LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(rusqlite::params_from_iter(values), |r| {
        r.get::<_, String>(0)
    })?
    .map(|row| Ok(serde_json::from_str(&row?)?))
    .collect()
}

pub(super) fn resolve_endpoint_in(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
) -> Result<Node> {
    let exact = exact_filtered(conn, text, options, false)?;
    if !exact.is_empty() || node(conn, text)?.is_some() {
        return require_unique(exact, text);
    }
    let scope = text.split_once("::");
    if scope.is_none() {
        let file = source_path(conn, text)?;
        let mut candidates = file_nodes(conn, &file, options, 2)?;
        if !candidates.is_empty() {
            if file_root(&candidates[0], &file)
                && candidates.get(1).is_none_or(|n| !file_root(n, &file))
            {
                return Ok(candidates.remove(0));
            }
            return require_unique(candidates, text);
        }
    }
    let (file, term) = scope.map_or((None, text), |(file, term)| (Some(file), term));
    let term = normalize(term);
    // A combining-mark-only query must not become an empty prefix of all nodes.
    ensure!(
        !term.is_empty(),
        "endpoint has no searchable normalized spelling"
    );
    let callable = term
        .strip_suffix("()")
        .map_or_else(|| format!("{term}()"), str::to_owned);
    // A complete scoped Owner.member spelling is stronger than a partial
    // match in a longer name or descendant, but never outranks full equality.
    let qualified_tail = file.is_some()
        && term.contains('.')
        && term.split('.').all(|component| !component.is_empty());
    let mut values = Vec::new();
    let mut filters = filter_sql(conn, options, &mut values)?;
    if let Some(file) = file {
        filters.push_str(" AND n.file=?");
        values.push(source_path(conn, file)?.into());
    }
    // V5 stores NFKC spellings plus an explicit NFKD/accent-folded companion,
    // making ASCII exact/prefix candidates complete. First bound the global
    // FTS input, then keep the independent filtered endpoint-field budget.
    let fts_safe = term.len() >= 3
        && term.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && enriched_search_index(conn)?
        && within_endpoint_fts_bounds(conn)?
        && within_endpoint_bounds(conn, &filters, &values)?;
    let mut tiers = if fts_safe {
        let expression = format!("\"{term}\"*");
        let mut fts_values = vec![expression.into()];
        fts_values.extend(values.iter().cloned());
        let fts_sql = format!(
            "SELECT n.id,n.label,n.qualified_name
             FROM node_search CROSS JOIN nodes n ON n.rowid=node_search.rowid
             WHERE node_search MATCH ?{filters} ORDER BY n.id"
        );
        let fts_tiers = scan_endpoint_rows(
            conn,
            &fts_sql,
            &fts_values,
            &term,
            &callable,
            qualified_tail,
        )?;
        if fts_tiers[0].is_empty() && fts_tiers[2].is_empty() {
            // Tokenization cannot prove arbitrary literal substrings, so keep
            // the complete endpoint scan when no exact/prefix tier was found.
            Default::default()
        } else {
            fts_tiers
        }
    } else {
        Default::default()
    };
    if tiers[0].is_empty() && tiers[2].is_empty() {
        let sql = format!(
            "SELECT n.id,n.label,n.qualified_name FROM nodes n WHERE 1=1{filters}
            ORDER BY n.id LIMIT {}",
            MAX_RANK_POSTINGS + 1
        );
        tiers = scan_endpoint_rows(conn, &sql, &values, &term, &callable, qualified_tail)?;
    }
    let ids = tiers
        .into_iter()
        .find(|ids| !ids.is_empty())
        .unwrap_or_default();
    let nodes = ids
        .iter()
        .map(|id| node(conn, id)?.ok_or_else(|| anyhow::anyhow!("missing endpoint {id}")))
        .collect::<Result<Vec<_>>>()?;
    require_unique(nodes, text)
}

pub(super) fn impact_seeds(
    conn: &Connection,
    text: &str,
    options: &SearchOptions,
    output: &mut SearchResult,
) -> Result<(Vec<Node>, usize)> {
    let exact = exact_filtered(conn, text, options, false)?;
    let mut seeds = if !exact.is_empty() || node(conn, text)?.is_some() {
        vec![require_unique(exact, text)?]
    } else if !text.contains("::") {
        file_nodes(
            conn,
            &source_path(conn, text)?,
            options,
            options.graph.limit + 1,
        )?
    } else {
        Vec::new()
    };
    if seeds.is_empty() {
        seeds.push(resolve_endpoint_in(conn, text, options)?);
    }
    if seeds.len() == 1 && !seeds[0].file.is_empty() && file_root(&seeds[0], &seeds[0].file) {
        let root = seeds[0].id.clone();
        for member in file_nodes(conn, &seeds[0].file, options, options.graph.limit + 1)? {
            if member.id != root {
                seeds.push(member);
            }
        }
    }
    let mut examined = seeds.len();
    if seeds.len() > options.graph.limit {
        truncate(output, "seed_limit");
        seeds.truncate(options.graph.limit);
    }
    let mut seen: BTreeSet<_> = seeds.iter().map(|n| n.id.clone()).collect();
    let mut cursor = 0;
    let layout = storage_layout(conn)?;
    // Only descendants of the original seeds are added here. Dependencies
    // discovered by the later reverse walk never expand their own members.
    let membership_relations = [
        "contains".to_owned(),
        "defines".to_owned(),
        "method".to_owned(),
    ];
    'members: while cursor < seeds.len() && examined < MAX_EXAMINED {
        let identity = node_identity(conn, layout, &seeds[cursor].id)?;
        let rows = edge_relation_rows(
            conn,
            layout,
            &identity,
            true,
            false,
            Some(&membership_relations),
            MAX_EXAMINED - examined,
        )?;
        cursor += 1;
        for edge in rows {
            examined += 1;
            let id = edge.target;
            if seen.contains(&id) {
                continue;
            }
            let member = node(conn, &id)?
                .ok_or_else(|| anyhow::anyhow!("membership points to missing node {id}"))?;
            if !node_matches(&member, options) {
                continue;
            }
            if seeds.len() == options.graph.limit {
                truncate(output, "seed_limit");
                break 'members;
            }
            seen.insert(id);
            seeds.push(member);
        }
    }
    if examined == MAX_EXAMINED {
        truncate(output, "work_limit");
    }
    Ok((seeds, examined))
}

pub(super) fn search_terms(conn: &Connection, text: &str) -> Result<Vec<String>> {
    use unicode_normalization::UnicodeNormalization;
    // Compatibility composition agrees with the enriched write-side index,
    // without decomposing Hangul or dropping Greek accents before MATCH.
    let mut spelling: String = text.nfkc().collect();
    if spelling != text {
        let has_version: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('metadata') WHERE name='search_version')",
            [],
            |r| r.get(0),
        )?;
        let version: i64 = if has_version {
            conn.query_row(
                "SELECT search_version FROM metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )?
        } else {
            0
        };
        // Read-only queries must still find literal postings in old indexes.
        // Only an explicit write adds compatibility-normalized search text.
        if version < 2 {
            spelling = text.to_owned();
        }
    }
    let mut all = BTreeSet::new();
    for token in spelling
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        // Ranking's accent stripping is deliberately separate from FTS spelling.
        let token = token.to_owned();
        let chars: Vec<_> = token.chars().collect();
        if chars.len() > 2 && chars.iter().all(|c| crate::store::cjk(*c)) {
            for pair in chars.windows(2) {
                all.insert(pair.iter().collect::<String>());
            }
        } else {
            all.insert(token);
        }
    }
    let mut terms: Vec<_> = all
        .iter()
        .filter(|s| {
            !matches!(
                normalize(s).as_str(),
                "a" | "an"
                    | "and"
                    | "are"
                    | "does"
                    | "for"
                    | "how"
                    | "in"
                    | "is"
                    | "of"
                    | "or"
                    | "the"
                    | "to"
                    | "what"
                    | "where"
                    | "which"
                    | "who"
                    | "why"
            )
        })
        .cloned()
        .collect();
    if terms.is_empty() {
        terms.extend(all);
    }
    let intent = |s: &str| {
        matches!(
            normalize(s).as_str(),
            "call"
                | "calls"
                | "called"
                | "caller"
                | "callers"
                | "invoke"
                | "invokes"
                | "use"
                | "uses"
                | "used"
                | "using"
                | "import"
                | "imports"
                | "export"
                | "exports"
                | "extend"
                | "extends"
                | "implement"
                | "implements"
                | "depend"
                | "depends"
                | "reference"
                | "references"
        )
    };
    if terms.iter().any(|t| !intent(t)) {
        terms.retain(|t| !intent(t));
    }
    ensure!(
        !terms.is_empty() && terms.len() <= 8,
        "query must contain 1 to 8 distinct searchable terms after removing question words"
    );
    Ok(terms)
}
