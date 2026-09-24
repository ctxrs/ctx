use super::*;

pub(super) fn read_payloads<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    sql: &str,
) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    stmt.query_map([], |r| r.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect()
}

pub(super) fn connect(path: &Path) -> Result<Connection> {
    connect_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
}

pub(super) fn ensure_wal(conn: &Connection) -> Result<()> {
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    ensure!(
        mode.eq_ignore_ascii_case("wal"),
        "Graf write connection requires WAL mode (journal mode remained {mode})"
    );
    Ok(())
}

pub(super) fn connect_with_flags(path: &Path, flags: OpenFlags) -> Result<Connection> {
    connect_with_setup(path, flags, |_| Ok(()))
}

pub(super) fn connect_with_setup(
    path: &Path,
    flags: OpenFlags,
    configure: impl FnOnce(&Connection) -> Result<()>,
) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .with_context(|| format!("cannot open Graf database {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    configure(&conn)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

pub(super) fn validate(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let conn = &tx;
    let app: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
    ensure!(
        app == APPLICATION_ID,
        "not a Graf database; refusing unrelated database"
    );
    let layout = storage_layout(conn)?;
    let kind: String = conn.query_row("SELECT kind FROM metadata WHERE singleton=1", [], |r| {
        r.get(0)
    })?;
    ensure!(
        matches!(kind.as_str(), "empty" | "native" | "imported"),
        "invalid Graf database kind"
    );
    // Prepare without scanning or writing. An incomplete schema is not usable.
    conn.prepare(match layout {
        StorageLayout::Legacy => "SELECT n.payload,n.owner_file,e.payload,e.source,e.target,e.owner_file,e.ref_id,r.payload,r.source,r.owner_file,r.resolved_target,k.ref_id,k.priority,f.hash FROM nodes n,edges e,refs r,ref_keys k,files f LIMIT 0",
        StorageLayout::Compact => "SELECT n.nkey,n.payload,n.owner_key,e.payload,e.source_key,e.target_key,e.owner_key,e.ref_key,r.rkey,r.id,r.payload,r.source_key,r.owner_key,r.resolved_target_key,k.ref_key,k.priority,f.fkey,f.hash FROM nodes n,edges e,refs r,ref_keys k,files f LIMIT 0",
    })?;
    conn.prepare("SELECT rowid FROM node_search LIMIT 0")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= 5 {
        conn.prepare(
            "SELECT files,nodes,edges,unresolved_references FROM storage_counts WHERE singleton=1 LIMIT 1",
        )?;
        let count_triggers: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name IN (
                'storage_count_files_insert','storage_count_files_delete',
                'storage_count_nodes_insert','storage_count_nodes_delete',
                'storage_count_edges_insert','storage_count_edges_delete',
                'storage_count_refs_insert','storage_count_refs_delete',
                'storage_count_refs_update')",
            [],
            |row| row.get(0),
        )?;
        ensure!(count_triggers == 9, "incomplete Graf storage counters");
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn validate_facts(changed: &[FileFacts], deleted: &[String]) -> Result<()> {
    let mut paths = BTreeSet::new();
    for facts in changed {
        ensure!(
            !facts.path.is_empty() && paths.insert(&facts.path),
            "duplicate or empty changed file path"
        );
        let ids: BTreeSet<_> = facts.nodes.iter().map(|n| n.id.as_str()).collect();
        for node in &facts.nodes {
            ensure!(!node.id.is_empty(), "node ID cannot be empty");
            binding_aliases(node)?;
            ensure!(
                node.file == facts.path,
                "node file does not match its owning file"
            );
        }
        for edge in &facts.edges {
            ensure!(!edge.id.is_empty(), "edge ID cannot be empty");
            ensure!(
                ids.contains(edge.source.as_str()),
                "native edge source must belong to its file"
            );
            ensure!(
                edge.file.as_ref().is_none_or(|f| f == &facts.path),
                "edge file does not match its owner"
            );
        }
        for reference in &facts.references {
            ensure!(
                reference.file == facts.path && ids.contains(reference.source.as_str()),
                "reference source/file does not match its owner"
            );
        }
    }
    for path in deleted {
        ensure!(paths.insert(path), "duplicate file path in delta: {path}");
    }
    Ok(())
}

pub(super) fn binding_aliases(node: &Node) -> Result<Vec<&str>> {
    let Some(value) = node.metadata.get("binding_aliases") else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .context("binding_aliases must be an array")?
        .iter()
        .map(|alias| {
            let alias = alias.as_str().context("binding aliases must be strings")?;
            ensure!(!alias.is_empty(), "binding alias cannot be empty");
            Ok(alias)
        })
        .collect()
}

pub(super) fn ensure_storage_indices(tx: &Transaction<'_>) -> Result<()> {
    for name in OBSOLETE_STORAGE_INDICES {
        tx.execute_batch(&format!("DROP INDEX IF EXISTS {name};"))?;
    }
    for &(name, sql) in STORAGE_INDICES {
        let current: Option<String> = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if current.as_deref() != Some(sql) {
            // Names and definitions are static, never user-provided SQL.
            tx.execute_batch(&format!("DROP INDEX IF EXISTS {name}; {sql};"))?;
        }
    }
    Ok(())
}

pub(super) fn ensure_storage_counts(tx: &Transaction<'_>) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='storage_counts')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        tx.execute_batch(STORAGE_COUNTS)?;
    }
    rebuild_storage_counts(tx)?;
    tx.execute_batch(DROP_STORAGE_COUNT_TRIGGERS)?;
    tx.execute_batch(STORAGE_COUNT_TRIGGERS)?;
    Ok(())
}

pub(super) fn rebuild_storage_counts(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "UPDATE storage_counts SET
            files=(SELECT count(*) FROM files),
            nodes=(SELECT count(*) FROM nodes),
            edges=(SELECT count(*) FROM edges),
            unresolved_references=(SELECT count(*) FROM refs WHERE resolved_target_key IS NULL)
         WHERE singleton=1",
        [],
    )?;
    Ok(())
}

pub(super) fn ensure_compact_storage(
    tx: &Transaction<'_>,
    keys: &mut BTreeSet<String>,
) -> Result<bool> {
    let layout = storage_layout(tx)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 3 {
        // A projection must preserve the old public identity exactly, not
        // silently substitute a missing, coerced, or different payload value.
        let invalid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM refs WHERE typeof(id)!='text'
             OR json_type(payload,'$.id') IS NOT 'text'
             OR id IS NOT json_extract(payload,'$.id'))",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            !invalid,
            "storage upgrade reference payload identity mismatch"
        );
    }
    // Legacy replacement builds FTS once, after the copied nodes are published.
    let search_changed = ensure_search(tx, layout == StorageLayout::Compact)?;
    if layout == StorageLayout::Legacy {
        let aliases: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='node_aliases')",
            [],
            |row| row.get(0),
        )?;
        tx.execute_batch(COMPACT_TABLES)?;
        tx.execute_batch(COMPACT_REFERENCE_TABLES)?;
        tx.execute_batch(
            "INSERT INTO compact_files(fkey,path,hash,module,diagnostics)
                 SELECT rowid,path,hash,module,diagnostics FROM files;
             INSERT INTO compact_nodes(nkey,id,label,qualified_name,binding_key,file,owner_key,payload,search)
                 SELECT rowid,id,label,qualified_name,binding_key,file,
                     (SELECT fkey FROM compact_files WHERE path=nodes.owner_file),payload,search FROM nodes;
             INSERT INTO compact_refs(rkey,source_key,owner_key,relation,payload,resolved_target_key,resolution_reason)
                 SELECT rowid,(SELECT nkey FROM compact_nodes WHERE id=refs.source),
                     (SELECT fkey FROM compact_files WHERE path=refs.owner_file),relation,payload,
                     (SELECT nkey FROM compact_nodes WHERE id=refs.resolved_target),resolution_reason FROM refs;
             INSERT INTO compact_ref_keys(ref_key,priority,binding_key)
                 SELECT (SELECT rkey FROM compact_refs WHERE id=ref_keys.ref_id),priority,binding_key FROM ref_keys;
             INSERT INTO compact_edges(id,source_key,target_key,relation,directed,owner_key,ref_key,payload)
                 SELECT id,(SELECT nkey FROM compact_nodes WHERE id=edges.source),
                     (SELECT nkey FROM compact_nodes WHERE id=edges.target),relation,directed,
                     (SELECT fkey FROM compact_files WHERE path=edges.owner_file),
                     (SELECT rkey FROM compact_refs WHERE id=edges.ref_id),payload FROM edges;",
        )?;
        if aliases {
            tx.execute_batch(
                "INSERT INTO compact_node_aliases(node_key,binding_key)
                     SELECT (SELECT nkey FROM compact_nodes WHERE id=node_aliases.node_id),binding_key FROM node_aliases;",
            )?;
        }
        for table in [
            "files",
            "nodes",
            "refs",
            "ref_keys",
            "edges",
            "node_aliases",
        ] {
            if table == "node_aliases" && !aliases {
                continue;
            }
            let equal: bool = tx.query_row(
                &format!(
                    "SELECT (SELECT count(*) FROM {table})=(SELECT count(*) FROM compact_{table})"
                ),
                [],
                |row| row.get(0),
            )?;
            ensure!(equal, "storage upgrade row count mismatch for {table}");
        }
        // NOT NULL/FK constraints reject missing mandatory links. Optional
        // links must distinguish a legitimate NULL from a failed lookup too.
        for sql in [
            "SELECT EXISTS(SELECT 1 FROM nodes o JOIN compact_nodes n ON n.nkey=o.rowid WHERE o.owner_file IS NOT NULL AND n.owner_key IS NULL)",
            "SELECT EXISTS(SELECT 1 FROM refs o JOIN compact_refs r ON r.rkey=o.rowid WHERE o.resolved_target IS NOT NULL AND r.resolved_target_key IS NULL)",
            "SELECT EXISTS(SELECT 1 FROM edges o JOIN compact_edges e ON e.id=o.id WHERE (o.owner_file IS NOT NULL AND e.owner_key IS NULL) OR (o.ref_id IS NOT NULL AND e.ref_key IS NULL))",
        ] {
            let missing: bool = tx.query_row(sql, [], |row| row.get(0))?;
            ensure!(!missing, "storage upgrade cannot map an existing identity");
        }
        tx.execute_batch(
            "DROP TRIGGER nodes_insert;
             DROP TRIGGER nodes_delete;
             DROP TABLE node_search;
             DROP TABLE edges;
             DROP TABLE ref_keys;
             DROP TABLE IF EXISTS node_aliases;
             DROP TABLE refs;
             DROP TABLE nodes;
             DROP TABLE files;",
        )?;
        tx.execute_batch(COMPACT_PUBLISH)?;
        tx.execute_batch(COMPACT_REFERENCE_PUBLISH)?;
        if !aliases {
            backfill_aliases(tx, keys)?;
        }
    } else if version == 2 {
        // Reuse the same reference schema with the already-published parents.
        // Copy FK children before dropping them; foreign_keys stays enabled.
        tx.execute_batch(
            &COMPACT_REFERENCE_TABLES
                .replace("compact_nodes", "nodes")
                .replace("compact_files", "files"),
        )?;
        tx.execute_batch(
            "INSERT INTO compact_refs(rkey,source_key,owner_key,relation,payload,resolved_target_key,resolution_reason)
                 SELECT rkey,source_key,owner_key,relation,payload,resolved_target_key,resolution_reason FROM refs;
             INSERT INTO compact_ref_keys(ref_key,priority,binding_key)
                 SELECT ref_key,priority,binding_key FROM ref_keys;
             INSERT INTO compact_edges(rowid,id,source_key,target_key,relation,directed,owner_key,ref_key,payload)
                 SELECT rowid,id,source_key,target_key,relation,directed,owner_key,ref_key,payload FROM edges;",
        )?;
        for table in ["refs", "ref_keys", "edges"] {
            let equal: bool = tx.query_row(
                &format!(
                    "SELECT (SELECT count(*) FROM {table})=(SELECT count(*) FROM compact_{table})"
                ),
                [],
                |row| row.get(0),
            )?;
            ensure!(equal, "storage upgrade row count mismatch for {table}");
        }
        tx.execute_batch("DROP TABLE edges; DROP TABLE ref_keys; DROP TABLE refs;")?;
        tx.execute_batch(COMPACT_REFERENCE_PUBLISH)?;
    }
    if version < 3 {
        let violations: i64 =
            tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        ensure!(violations == 0, "storage upgrade foreign key check failed");
        tx.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        normalize_storage_v4(tx)?;
    }
    ensure_aliases(tx, keys)?;
    ensure_storage_indices(tx)?;
    if version < 5 {
        ensure_storage_counts(tx)?;
        tx.pragma_update(None, "user_version", 5)?;
    }
    Ok(search_changed)
}

pub(super) fn normalize_storage_v4(tx: &Transaction<'_>) -> Result<()> {
    let invalid: bool = tx.query_row(
        "SELECT
          EXISTS(SELECT 1 FROM nodes n WHERE
            n.id IS NOT json_extract(n.payload,'$.id') OR
            n.label IS NOT json_extract(n.payload,'$.label') OR
            n.file IS NOT json_extract(n.payload,'$.file') OR
            n.qualified_name IS NOT json_extract(n.payload,'$.qualified_name') OR
            n.binding_key IS NOT json_extract(n.payload,'$.binding_key')) OR
          EXISTS(SELECT 1 FROM refs r JOIN nodes s ON s.nkey=r.source_key WHERE
            r.id IS NOT json_extract(r.payload,'$.id') OR
            s.id IS NOT json_extract(r.payload,'$.source') OR
            r.relation IS NOT json_extract(r.payload,'$.relation')) OR
          EXISTS(SELECT 1 FROM edges e JOIN nodes s ON s.nkey=e.source_key JOIN nodes t ON t.nkey=e.target_key WHERE
            e.id IS NOT json_extract(e.payload,'$.id') OR
            s.id IS NOT json_extract(e.payload,'$.source') OR
            t.id IS NOT json_extract(e.payload,'$.target') OR
            e.relation IS NOT json_extract(e.payload,'$.relation') OR
            e.directed IS NOT json_extract(e.payload,'$.directed'))",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        !invalid,
        "storage normalization payload projection mismatch"
    );

    tx.execute_batch(NORMALIZED_TABLES)?;
    tx.execute_batch(NORMALIZED_REFERENCE_TABLES)?;
    tx.execute_batch(
        "INSERT INTO normalized_files(fkey,path,hash,module,diagnostics)
             SELECT fkey,path,hash,module,diagnostics FROM files;
         INSERT INTO normalized_nodes(
             nkey,id,label,kind,file,line,end_line,qualified_name,binding_key,
             metadata,owner_key,search)
             SELECT nkey,id,label,json_extract(payload,'$.kind'),file,
                    json_extract(payload,'$.line'),json_extract(payload,'$.end_line'),
                    qualified_name,binding_key,payload -> '$.metadata',owner_key,search
             FROM nodes;
         INSERT INTO normalized_node_aliases(node_key,binding_key)
             SELECT node_key,binding_key FROM node_aliases;
         INSERT INTO normalized_refs(
             rkey,id,source,source_key,owner_key,label,relation,file,line,
             candidate_keys,reason,resolved_target_key,resolution_reason)
             SELECT rkey,id,json_extract(payload,'$.source'),source_key,owner_key,
                    json_extract(payload,'$.label'),relation,
                    json_extract(payload,'$.file'),json_extract(payload,'$.line'),
                    payload -> '$.candidate_keys',json_extract(payload,'$.reason'),
                    resolved_target_key,resolution_reason
             FROM refs;
         INSERT INTO normalized_ref_keys(ref_key,priority,binding_key)
             SELECT ref_key,priority,binding_key FROM ref_keys;
         INSERT INTO normalized_edges(
             id,source,target,source_key,target_key,relation,directed,file,line,
             confidence,metadata,owner_key,ref_key)
             SELECT e.id,s.id,t.id,e.source_key,e.target_key,e.relation,e.directed,
                    json_extract(e.payload,'$.file'),json_extract(e.payload,'$.line'),
                    json_extract(e.payload,'$.confidence'),e.payload -> '$.metadata',
                    e.owner_key,e.ref_key
             FROM edges e JOIN nodes s ON s.nkey=e.source_key
                          JOIN nodes t ON t.nkey=e.target_key;",
    )?;
    for table in [
        "files",
        "nodes",
        "node_aliases",
        "refs",
        "ref_keys",
        "edges",
    ] {
        let equal: bool = tx.query_row(
            &format!(
                "SELECT (SELECT count(*) FROM {table})=(SELECT count(*) FROM normalized_{table})"
            ),
            [],
            |row| row.get(0),
        )?;
        ensure!(
            equal,
            "storage normalization row count mismatch for {table}"
        );
    }
    tx.execute_batch(
        "DROP TRIGGER nodes_insert;
         DROP TRIGGER nodes_delete;
         DROP TABLE edges;
         DROP TABLE ref_keys;
         DROP TABLE refs;
         DROP TABLE node_aliases;
         DROP TABLE nodes;
         DROP TABLE files;
         ALTER TABLE normalized_files RENAME TO files;
         ALTER TABLE normalized_nodes RENAME TO nodes;
         ALTER TABLE normalized_node_aliases RENAME TO node_aliases;
         ALTER TABLE normalized_refs RENAME TO refs;
         ALTER TABLE normalized_ref_keys RENAME TO ref_keys;
         ALTER TABLE normalized_edges RENAME TO edges;",
    )?;
    tx.execute_batch(
        "CREATE INDEX nodes_label ON nodes(label, id);
         CREATE INDEX nodes_file ON nodes(file, id);
         CREATE INDEX node_aliases_binding ON node_aliases(binding_key, node_key);
         CREATE INDEX refs_owner ON refs(owner_key);
         CREATE INDEX refs_unresolved_source ON refs(source_key, id) WHERE resolved_target_key IS NULL;
         CREATE INDEX refs_unresolved_relation ON refs(source_key, relation, id) WHERE resolved_target_key IS NULL;
         CREATE INDEX ref_keys_binding ON ref_keys(binding_key, ref_key);
         CREATE INDEX edges_source ON edges(source_key, id);
         CREATE INDEX edges_target ON edges(target_key, id);
         CREATE INDEX edges_source_relation ON edges(source_key, relation, id);
         CREATE INDEX edges_target_relation ON edges(target_key, relation, id);
         CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
             INSERT INTO node_search(rowid,text) VALUES(new.nkey,new.search);
         END;
         CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
             DELETE FROM node_search WHERE rowid=old.nkey;
         END;",
    )?;
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    ensure!(
        violations == 0,
        "storage normalization foreign key check failed"
    );
    tx.pragma_update(None, "user_version", 4)?;
    Ok(())
}

pub(super) fn ensure_aliases(tx: &Transaction<'_>, keys: &mut BTreeSet<String>) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='node_aliases')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        tx.execute_batch(
            "CREATE TABLE node_aliases (
                node_key INTEGER NOT NULL REFERENCES nodes(nkey) ON DELETE CASCADE,
                binding_key TEXT NOT NULL, PRIMARY KEY(node_key,binding_key)
             ) WITHOUT ROWID;
             CREATE INDEX node_aliases_binding ON node_aliases(binding_key,node_key);",
        )?;
        backfill_aliases(tx, keys)?;
    }
    Ok(())
}

pub(super) fn backfill_aliases(tx: &Transaction<'_>, keys: &mut BTreeSet<String>) -> Result<()> {
    let mut stmt = tx.prepare("SELECT payload FROM nodes WHERE owner_key IS NOT NULL")?;
    for payload in stmt.query_map([], |row| row.get::<_, String>(0))? {
        let node: Node = serde_json::from_str(&payload?)?;
        for alias in binding_aliases(&node)? {
            keys.insert(alias.to_owned());
            tx.execute(
                "INSERT INTO node_aliases(node_key,binding_key) VALUES((SELECT nkey FROM nodes WHERE id=?1),?2) ON CONFLICT(node_key,binding_key) DO NOTHING",
                params![node.id, alias],
            )?;
        }
    }
    Ok(())
}

pub(super) fn ensure_search(tx: &Transaction<'_>, rebuild_fts: bool) -> Result<bool> {
    let has_version: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('metadata') WHERE name='search_version')",
        [],
        |r| r.get(0),
    )?;
    if !has_version {
        tx.execute_batch(
            "ALTER TABLE metadata ADD COLUMN search_version INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    let version: i64 = tx.query_row(
        "SELECT search_version FROM metadata WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        version <= SEARCH_VERSION,
        "unsupported Graf search index version {version}"
    );
    let packed: bool = tx.query_row(
        "SELECT instr(sql,'contentless_delete=1')>0 FROM sqlite_master
         WHERE type='table' AND name='node_search'",
        [],
        |row| row.get(0),
    )?;
    if version == SEARCH_VERSION && (!rebuild_fts || packed) {
        return Ok(false);
    }
    let mut changed = false;
    if version != SEARCH_VERSION {
        let mut stmt = tx.prepare("SELECT id,payload,search FROM nodes ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let node: Node = serde_json::from_str(&row.get::<_, String>(1)?)?;
            let search = search_text(&node);
            if search != row.get::<_, String>(2)? {
                tx.execute(
                    "UPDATE nodes SET search=?1 WHERE id=?2",
                    params![search, node.id],
                )?;
                changed = true;
            }
        }
    }
    if rebuild_fts && !packed {
        // Contentless-delete FTS5 retains ordinary DELETE/INSERT semantics and
        // positions/docsize, without another copy of nodes.search. Queries read
        // only rowid/MATCH and fetch all public data from nodes.
        tx.execute_batch(
            "DROP TRIGGER nodes_insert;
             DROP TRIGGER nodes_delete;
             DROP TABLE node_search;
             CREATE VIRTUAL TABLE node_search USING fts5(text, content='', contentless_delete=1);
             CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
                 INSERT INTO node_search(rowid,text) VALUES(new.rowid,new.search);
             END;
             CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
                 DELETE FROM node_search WHERE rowid=old.rowid;
             END;",
        )?;
    }
    if rebuild_fts && (changed || !packed) {
        if packed {
            tx.execute("DELETE FROM node_search", [])?;
        }
        tx.execute(
            "INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes",
            [],
        )?;
    }
    tx.execute(
        "UPDATE metadata SET search_version=?1 WHERE singleton=1",
        [SEARCH_VERSION],
    )?;
    Ok(changed)
}

pub(super) fn search_text(node: &Node) -> String {
    let mut text = format!(
        "{} {} {} {}",
        node.id,
        node.label,
        node.qualified_name.as_deref().unwrap_or(""),
        node.file,
    );
    let mut attrs = &node.metadata;
    while let Some(original) = attrs.get("original_metadata") {
        attrs = original;
    }
    // Deliberately index named prose fields, never arbitrary metadata values.
    for field in [
        "rationale",
        "description",
        "summary",
        "text",
        "excerpt",
        "evidence",
    ] {
        if let Some(value) = attrs.get(field).and_then(serde_json::Value::as_str) {
            text.push(' ');
            text.push_str(value);
        }
    }
    // Extractors retain safe written attribute values here after redaction.
    // Index the preserved JSON (keys as well as literal leaves), never source
    // text or arbitrary metadata. Imported attributes use the same contract.
    if let Some(attributes) = attrs.get("attributes").filter(|v| v.is_object()) {
        let mut pending = vec![attributes];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        text.push(' ');
                        text.push_str(key);
                        pending.push(value);
                    }
                }
                serde_json::Value::Array(values) => pending.extend(values),
                serde_json::Value::String(value) => {
                    text.push(' ');
                    text.push_str(value);
                }
                value => {
                    text.push(' ');
                    text.push_str(&value.to_string());
                }
            }
        }
    }
    // Compatibility forms (ligatures, full-width letters) share searchable
    // tokens. NFKC retains composed Hangul and Greek spellings for unicode61;
    // the original text below still serves literal callers of the older API.
    let spelling: String = text.nfkc().collect();
    let mut search = String::with_capacity(spelling.len() * 2);
    let mut previous_lower = false;
    for c in spelling.chars() {
        if c.is_uppercase() && previous_lower {
            search.push(' ');
        }
        previous_lower = c.is_lowercase() || c.is_numeric();
        search.push(if c == '_' { ' ' } else { c });
    }
    if spelling
        .nfkd()
        .any(unicode_normalization::char::is_combining_mark)
    {
        let folded: String = spelling
            .nfkd()
            .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
            .flat_map(char::to_lowercase)
            .collect();
        search.push(' ');
        search.push_str(&folded);
    }
    // Useful CJK recall without a dictionary or a query-side scan: preserve
    // full strings and index individual ideographs plus adjacent bigrams.
    let mut previous = None;
    for c in spelling.chars() {
        if cjk(c) {
            search.push(' ');
            search.push(c);
            if let Some(before) = previous {
                search.push(' ');
                search.push(before);
                search.push(c);
            }
            previous = Some(c);
        } else {
            previous = None;
        }
    }
    // Retain the normalized spelling for exact identifier tokens, then the
    // transformed form for camel-case and CJK recall. Literal callers of the
    // original query API still need compatibility-form tokens when NFKC
    // changes their spelling; ordinary text needs no duplicate raw copy.
    if text == spelling {
        format!("{spelling} {search}")
    } else {
        format!("{text} {spelling} {search}")
    }
}

pub(super) fn facts_hash(facts: &FileFacts) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        &facts.path,
        &facts.module,
        &facts.nodes,
        &facts.edges,
        &facts.references,
        &facts.diagnostics,
    ))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(super) fn record_initial_binding<'a>(
    bindings: &mut HashMap<&'a str, InitialBinding>,
    key: &'a str,
    node_key: i64,
) {
    match bindings.get(key).copied() {
        None => {
            bindings.insert(key, InitialBinding::Unique(node_key));
        }
        Some(InitialBinding::Unique(existing)) if existing == node_key => {}
        Some(_) => {
            bindings.insert(key, InitialBinding::Ambiguous);
        }
    }
}
