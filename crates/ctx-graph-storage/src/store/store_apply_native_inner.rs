use super::*;

impl Store {
    pub(super) fn apply_native_inner(
        &mut self,
        root: &str,
        changed: Vec<FileFacts>,
        deleted: Vec<String>,
        coverage: Coverage,
        options: Option<serde_json::Value>,
    ) -> Result<IndexReport> {
        ensure!(!root.is_empty(), "native root cannot be empty");
        validate_facts(&changed, &deleted)?;
        let parsed_files = changed.len();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if generation(&tx)? != self.baseline_generation {
            return Err(StaleStore.into());
        }
        let (kind, previous_root): (String, Option<String>) = tx.query_row(
            "SELECT kind, root FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(kind != "imported", "cannot update an imported snapshot");
        if let Some(previous_root) = previous_root {
            ensure!(
                previous_root == root,
                "database root mismatch: indexed {previous_root}, requested {root}"
            );
        }
        let mut keys = BTreeSet::new();
        let search_migrated = ensure_compact_storage(&tx, &mut keys)?;
        let aliases_migrated = !keys.is_empty();
        let mut metadata: serde_json::Value = serde_json::from_str(&tx.query_row(
            "SELECT graph_metadata FROM metadata WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        let mut options_changed = false;
        if let Some(options) = options {
            if metadata.is_null() {
                metadata = serde_json::json!({});
            }
            let attrs = metadata
                .as_object_mut()
                .context("native graph metadata must be an object")?;
            options_changed = attrs.get("graf_index_options") != Some(&options);
            attrs.insert("graf_index_options".to_owned(), options);
        }
        if kind == "empty"
            && deleted.is_empty()
            && tx.query_row("SELECT count(*)=0 FROM files", [], |row| {
                row.get::<_, bool>(0)
            })?
        {
            publish_initial_native(&tx, &changed)?;
            tx.execute(
                "UPDATE metadata SET kind='native', root=?1, coverage=?2, generation=generation+1, graph_metadata=?3 WHERE singleton=1",
                params![
                    root,
                    serde_json::to_string(&coverage)?,
                    serde_json::to_string(&metadata)?
                ],
            )?;
            let stats = read_stats(&tx)?;
            let report = IndexReport {
                schema_version: SCHEMA_VERSION,
                generation: stats.generation,
                parsed_files,
                unchanged_files: coverage.unchanged_files,
                deleted_files: 0,
                nodes: stats.nodes,
                edges: stats.edges,
                diagnostics: stats.diagnostics,
                semantic_usage: None,
                provider_usage: None,
                timings: None,
            };
            tx.commit()?;
            self.baseline_generation = report.generation;
            return Ok(report);
        }
        let mut changed_facts = Vec::with_capacity(changed.len());
        let mut changed_digests = HashMap::with_capacity(changed.len());
        let mut stamps_changed = false;
        for facts in changed {
            let digest = facts_hash(&facts)?;
            let previous: Option<String> = tx
                .query_row(
                    "SELECT facts_hash FROM files WHERE path=?1",
                    [&facts.path],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if previous.as_deref() == Some(&digest) {
                let updated = tx.execute(
                    "UPDATE files SET hash=?1,module=?2,diagnostics=?3,facts_hash=?4
                     WHERE path=?5 AND hash IS NOT ?1",
                    params![
                        facts.hash,
                        facts.module,
                        serde_json::to_string(&facts.diagnostics)?,
                        digest,
                        facts.path
                    ],
                )?;
                stamps_changed |= updated != 0;
            } else {
                changed_digests.insert(facts.path.clone(), digest);
                changed_facts.push(facts);
            }
        }
        let changed = changed_facts;
        // Replacing a target cascades away incoming edges even when their
        // unchanged owner still asserts them. References are rebound below;
        // direct edges have no reference record from which to rebuild them.
        let replaced: BTreeSet<_> = deleted
            .iter()
            .map(String::as_str)
            .chain(changed.iter().map(|facts| facts.path.as_str()))
            .collect();
        let mut ruby_changed = changed.iter().any(|facts| {
            facts
                .nodes
                .iter()
                .any(|node| node.metadata["language"] == "ruby")
        });
        if !ruby_changed {
            for path in &replaced {
                ruby_changed = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM nodes WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) AND json_extract(metadata,'$.language')='ruby')",
                    [path], |row| row.get(0),
                )?;
                if ruby_changed {
                    break;
                }
            }
        }
        let mut incoming = Vec::new();
        for facts in &changed {
            let mut stmt = tx.prepare(
                "SELECT e.payload,f.path FROM nodes n JOIN edges e ON e.target_key=n.nkey
                 JOIN files f ON f.fkey=e.owner_key
                 WHERE n.owner_key=(SELECT fkey FROM files WHERE path=?1) AND e.ref_key IS NULL",
            )?;
            for row in stmt.query_map([&facts.path], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })? {
                let (payload, owner) = row?;
                if !replaced.contains(owner.as_str()) {
                    incoming.push((serde_json::from_str::<Edge>(&payload)?, owner));
                }
            }
        }
        let mut removed = 0;
        for path in deleted.iter().chain(changed.iter().map(|f| &f.path)) {
            let mut stmt = tx.prepare(
                "SELECT binding_key FROM nodes WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) AND binding_key IS NOT NULL
                 UNION SELECT a.binding_key FROM node_aliases a JOIN nodes n ON n.nkey=a.node_key WHERE n.owner_key=(SELECT fkey FROM files WHERE path=?1)",
            )?;
            for key in stmt.query_map([path], |r| r.get::<_, String>(0))? {
                keys.insert(key?);
            }
        }
        for path in &deleted {
            removed += tx.execute("DELETE FROM files WHERE path=?1", [path])?;
        }
        for facts in &changed {
            tx.execute("DELETE FROM files WHERE path=?1", [&facts.path])?;
        }
        for facts in &changed {
            tx.execute(
                "INSERT INTO files(path,hash,module,diagnostics,facts_hash) VALUES(?1,?2,?3,?4,?5)",
                params![
                    facts.path,
                    facts.hash,
                    facts.module,
                    serde_json::to_string(&facts.diagnostics)?,
                    changed_digests
                        .get(&facts.path)
                        .context("missing extracted-facts digest")?
                ],
            )?;
            for node in &facts.nodes {
                if let Some(key) = &node.binding_key {
                    keys.insert(key.clone());
                }
                insert_node(&tx, node, Some(&facts.path))?;
                for alias in binding_aliases(node)? {
                    keys.insert(alias.to_owned());
                    tx.execute(
                        "INSERT INTO node_aliases(node_key,binding_key) VALUES((SELECT nkey FROM nodes WHERE id=?1),?2) ON CONFLICT(node_key,binding_key) DO NOTHING",
                        params![node.id, alias],
                    )?;
                }
            }
        }
        for (edge, owner) in incoming {
            let endpoints_survive: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1) AND EXISTS(SELECT 1 FROM nodes WHERE id=?2)",
                params![edge.source, edge.target],
                |r| r.get(0),
            )?;
            if endpoints_survive {
                insert_edge(&tx, &edge, Some(&owner), None)?;
            }
        }
        let mut affected = BTreeSet::new();
        for facts in &changed {
            for edge in &facts.edges {
                insert_edge(&tx, edge, Some(&facts.path), None)?;
            }
            for reference in &facts.references {
                tx.execute("INSERT INTO refs(id,source,source_key,owner_key,label,relation,file,line,candidate_keys,reason,resolved_target_key,resolution_reason) VALUES(?1,?2,(SELECT nkey FROM nodes WHERE id=?2),(SELECT fkey FROM files WHERE path=?3),?4,?5,?6,?7,?8,?9,NULL,?9)",
                    params![reference.id, reference.source, facts.path, reference.label, reference.relation, reference.file, reference.line, serde_json::to_string(&reference.candidate_keys)?, reference.reason])?;
                for (priority, key) in reference.candidate_keys.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO ref_keys(ref_key,priority,binding_key) VALUES((SELECT rkey FROM refs WHERE id=?1),?2,?3)",
                        params![reference.id, priority as i64, key],
                    )?;
                }
                affected.insert(reference.id.clone());
            }
        }
        for key in keys {
            let mut stmt = tx.prepare("SELECT r.id FROM ref_keys k JOIN refs r ON r.rkey=k.ref_key WHERE k.binding_key=?1")?;
            for id in stmt.query_map([key], |r| r.get::<_, String>(0))? {
                affected.insert(id?);
            }
        }
        if ruby_changed {
            let nodes: Vec<Node> = read_payloads(
                &tx,
                "SELECT payload FROM nodes WHERE json_extract(metadata,'$.language')='ruby' ORDER BY id",
            )?;
            let context = ctx_graph_languages::scripted::RubyContext::from_nodes(&nodes);
            let references: Vec<Reference> = read_payloads(
                &tx,
                "SELECT r.payload FROM refs r JOIN nodes n ON n.nkey=r.source_key WHERE json_extract(n.metadata,'$.language')='ruby' ORDER BY r.id",
            )?;
            for reference in references {
                let keys = context
                    .inherited_keys(&reference)
                    .unwrap_or_else(|| reference.candidate_keys.clone());
                tx.execute(
                    "DELETE FROM ref_keys WHERE ref_key=(SELECT rkey FROM refs WHERE id=?1)",
                    [&reference.id],
                )?;
                for (priority, key) in keys.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO ref_keys(ref_key,priority,binding_key) VALUES((SELECT rkey FROM refs WHERE id=?1),?2,?3)",
                        params![reference.id, priority as i64, key],
                    )?;
                }
                affected.insert(reference.id);
            }
        }
        if !affected.is_empty() {
            let mut payload_statement = tx.prepare("SELECT payload FROM refs WHERE id=?1")?;
            let mut delete_edges =
                tx.prepare("DELETE FROM edges WHERE ref_key=(SELECT rkey FROM refs WHERE id=?1)")?;
            let mut candidate_keys = tx.prepare("SELECT binding_key FROM ref_keys WHERE ref_key=(SELECT rkey FROM refs WHERE id=?1) ORDER BY priority")?;
            let mut update_resolution = tx.prepare("UPDATE refs SET resolved_target_key=(SELECT nkey FROM nodes WHERE id=?1),resolution_reason=?2 WHERE id=?3")?;
            for id in affected {
                resolve_reference(
                    &tx,
                    &id,
                    &mut payload_statement,
                    &mut delete_edges,
                    &mut candidate_keys,
                    &mut update_resolution,
                )?;
            }
        }
        let previous_coverage: Coverage = serde_json::from_str(&tx.query_row(
            "SELECT coverage FROM metadata WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        // The unchanged count describes this scan, not a change in stored facts.
        // Physical upgrades may write on a no-op scan; only logical changes
        // (including coverage) publish a graph generation.
        let coverage_changed = previous_coverage.supported_files != coverage.supported_files
            || previous_coverage.unsupported_files != coverage.unsupported_files;
        let changed_generation = kind == "empty"
            || !changed.is_empty()
            || stamps_changed
            || removed > 0
            || options_changed
            || aliases_migrated
            || search_migrated
            || coverage_changed;
        if changed_generation {
            tx.execute("UPDATE metadata SET kind='native', root=?1, coverage=?2, generation=generation+1, graph_metadata=?3 WHERE singleton=1",
                params![root, serde_json::to_string(&coverage)?, serde_json::to_string(&metadata)?])?;
        }
        let stats = read_stats(&tx)?;
        let report = IndexReport {
            schema_version: SCHEMA_VERSION,
            generation: stats.generation,
            parsed_files,
            unchanged_files: coverage.unchanged_files,
            deleted_files: removed,
            nodes: stats.nodes,
            edges: stats.edges,
            diagnostics: stats.diagnostics,
            semantic_usage: None,
            provider_usage: None,
            timings: None,
        };
        tx.commit()?;
        self.baseline_generation = report.generation;
        Ok(report)
    }

    pub fn import_graph(&mut self, graph: ImportedGraph) -> Result<Stats> {
        self.write_import(graph, false)
    }

    /// Atomically replace an imported graph. Native indexes (even empty ones)
    /// and handles opened before another writer committed cannot be replaced.
    pub fn refresh_import(&mut self, graph: ImportedGraph) -> Result<Stats> {
        self.write_import(graph, true)
    }

    pub(super) fn write_import(&mut self, graph: ImportedGraph, refresh: bool) -> Result<Stats> {
        crate::snapshot::validate_graph(&graph.nodes, &graph.edges)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if generation(&tx)? != self.baseline_generation {
            return Err(StaleStore.into());
        }
        let (kind, root): (String, Option<String>) = tx.query_row(
            "SELECT kind,root FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if refresh {
            ensure!(
                kind == "imported" && root.is_none(),
                "refresh requires an imported snapshot without an index root"
            );
        } else {
            ensure!(
                kind == "empty" && root.is_none(),
                "import requires an empty Graf database"
            );
        }
        ensure_compact_storage(&tx, &mut BTreeSet::new())?;
        if refresh {
            tx.execute("DELETE FROM edges", [])?;
            tx.execute("DELETE FROM nodes", [])?;
        }
        // Deletes, inserts, search triggers and generation advance commit as
        // one transaction. Any validation or SQL failure retains the old graph.
        for node in &graph.nodes {
            insert_node(&tx, node, None)?;
        }
        for edge in &graph.edges {
            insert_edge(&tx, edge, None, None)?;
        }
        tx.execute("UPDATE metadata SET kind='imported', generation=generation+1, graph_metadata=?1 WHERE singleton=1",
            [serde_json::to_string(&graph.metadata)?])?;
        let stats = read_stats(&tx)?;
        tx.commit()?;
        self.baseline_generation = stats.generation;
        Ok(stats)
    }

    pub fn query_extended(
        &self,
        text: &str,
        options: &crate::query::SearchOptions,
    ) -> Result<crate::query::SearchResult> {
        crate::query::query_extended(&self.conn, text, options, false)
    }
    pub fn neighbors_extended(
        &self,
        symbol: &str,
        options: &crate::query::SearchOptions,
    ) -> Result<crate::query::SearchResult> {
        crate::query::query_extended(&self.conn, symbol, options, true)
    }
    pub fn path_extended(
        &self,
        source: &str,
        target: &str,
        options: &crate::query::SearchOptions,
    ) -> Result<crate::query::PathSearchResult> {
        crate::query::path_extended(&self.conn, source, target, options)
    }

    pub fn query(&self, text: &str, options: &QueryOptions) -> Result<GraphResult> {
        crate::query::query(&self.conn, text, options)
    }
    pub fn neighbors(&self, symbol: &str, options: &QueryOptions) -> Result<GraphResult> {
        crate::query::neighbors(&self.conn, symbol, options)
    }
    pub fn path(&self, source: &str, target: &str, options: &QueryOptions) -> Result<PathResult> {
        crate::query::path(&self.conn, source, target, options)
    }
}
