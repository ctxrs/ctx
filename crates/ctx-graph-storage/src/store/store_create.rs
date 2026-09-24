use super::*;

impl Store {
    /// Existing files must already be Graf databases. Even an empty foreign
    /// SQLite database is not an invitation to initialize it.
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let fresh = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => {
                drop(file);
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        let mut conn = connect(path)?;
        if fresh {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(NORMALIZED_TABLES)?;
            tx.execute_batch(NORMALIZED_REFERENCE_TABLES)?;
            tx.execute_batch(NORMALIZED_PUBLISH)?;
            tx.execute_batch(NORMALIZED_REFERENCE_PUBLISH)?;
            ensure_storage_indices(&tx)?;
            tx.execute_batch(STORAGE_COUNTS)?;
            tx.execute_batch(STORAGE_COUNT_TRIGGERS)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", 5)?;
            tx.commit()?;
        }
        validate(&conn)?;
        ensure_wal(&conn)?;
        let baseline_generation = generation(&conn)?;
        Ok(Self {
            conn,
            baseline_generation,
        })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = connect(path)?;
        validate(&conn)?;
        let baseline_generation = generation(&conn)?;
        Ok(Self {
            conn,
            baseline_generation,
        })
    }

    /// Prepare a native index publish for a large all-or-nothing write.
    ///
    /// Native indexing owns the database while it publishes a scan. Rollback
    /// journaling keeps the old pages in a temporary journal instead of
    /// retaining every newly written page in a WAL until the publish commits.
    /// Ordinary Store writes stay in WAL mode for concurrent readers.
    pub fn prepare_native_index_write(&self) -> Result<()> {
        ensure!(
            self.conn.is_autocommit(),
            "cannot change native index journal mode inside a transaction"
        );
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
        ensure!(
            mode.eq_ignore_ascii_case("delete"),
            "cannot prepare native index write while the database is busy (journal mode remained {mode})"
        );
        Ok(())
    }

    pub fn restore_native_index_write(&self) -> Result<()> {
        ensure!(
            self.conn.is_autocommit(),
            "cannot restore native index journal mode inside a transaction"
        );
        ensure_wal(&self.conn)
    }

    pub fn finish_native_index_write<T>(&self, result: Result<T>) -> Result<T> {
        match (result, self.restore_native_index_write()) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(restore)) => {
                Err(restore).context("graph committed but restoring WAL mode failed")
            }
            (Err(error), Err(restore)) => Err(error.context(format!(
                "native graph publish failed and restoring WAL mode also failed: {restore:#}"
            ))),
        }
    }

    /// Open without write permission or migrations. Normal SQLite WAL locking
    /// remains enabled so later committed generations stay visible.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        Self::open_read_only_with(path, |_| Ok(()))
    }

    /// Configure request limits before any validation or generation reads.
    pub fn open_read_only_with(
        path: &Path,
        configure: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<Self> {
        let conn = connect_with_setup(path, OpenFlags::SQLITE_OPEN_READ_ONLY, configure)?;
        validate(&conn)?;
        let baseline_generation = generation(&conn)?;
        Ok(Self {
            conn,
            baseline_generation,
        })
    }

    /// Explicitly repack an existing format-2 or format-3 database without changing facts
    /// or the write baseline. SQLite serializes VACUUM with other writers; a
    /// stale handle compacts current data but remains stale for fact writes.
    pub fn compact(&mut self) -> Result<CompactionReport> {
        ensure!(
            self.conn.is_autocommit(),
            "cannot compact inside an active transaction"
        );
        ensure!(
            !self.conn.is_readonly("main")?,
            "cannot compact a read-only Graf database"
        );
        let (page_size, pages_before, free_pages_before) = {
            let tx = self.conn.transaction()?;
            ensure!(
                storage_layout(&tx)? == StorageLayout::Compact,
                "storage format 1 must be upgraded by update or import refresh before compact"
            );
            let counts = (
                tx.pragma_query_value(None, "page_size", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
                tx.pragma_query_value(None, "page_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
                tx.pragma_query_value(None, "freelist_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
            );
            tx.commit()?;
            counts
        };
        // VACUUM owns its transaction; never wrap it in a write transaction or
        // replace the database path. Formats 2/3 have explicit parent INTEGER PKs.
        self.conn
            .execute_batch("VACUUM main")
            .context("cannot compact Graf database")?;
        let checkpoint_busy = self
            .conn
            .query_row("PRAGMA main.wal_checkpoint(TRUNCATE)", [], |row| {
                row.get::<_, bool>(0)
            })
            .context("compaction completed but checkpoint failed")?;
        let (pages_after, free_pages_after) = (|| -> Result<(u64, u64)> {
            let tx = self.conn.transaction()?;
            let counts = (
                tx.pragma_query_value(None, "page_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
                tx.pragma_query_value(None, "freelist_count", |row| {
                    row.get::<_, u32>(0).map(u64::from)
                })?,
            );
            tx.commit()?;
            Ok(counts)
        })()
        .context("compaction completed but reading page counts failed")?;
        Ok(CompactionReport {
            schema_version: SCHEMA_VERSION,
            page_size,
            pages_before,
            pages_after,
            free_pages_before,
            free_pages_after,
            checkpoint_busy,
        })
    }

    pub fn file_stamps(&self) -> Result<Vec<FileStamp>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, hash FROM files ORDER BY path")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(FileStamp {
                    path: r.get(0)?,
                    hash: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let tx = self.conn.unchecked_transaction()?;
        let stats = read_stats(&tx)?;
        tx.commit()?;
        Ok(stats)
    }

    /// Read graph-level metadata without loading any nodes or edges.
    pub fn graph_metadata(&self) -> Result<serde_json::Value> {
        let json: String = self.conn.query_row(
            "SELECT graph_metadata FROM metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&json)?)
    }

    /// Short saved-graph topic labels for an explicitly configured transcription adapter.
    pub fn transcription_topics(&self) -> Result<Vec<String>> {
        let tx = self.conn.unchecked_transaction()?;
        let normalized = normalized_storage(&tx)?;
        let sql = match storage_layout(&tx)? {
            StorageLayout::Legacy =>
                "WITH incidents AS (SELECT source AS id FROM edges UNION ALL SELECT target FROM edges),
                 degrees AS (SELECT id,count(*) AS degree FROM incidents GROUP BY id)
                 SELECT n.label FROM degrees d JOIN nodes n ON n.id=d.id
                 WHERE json_extract(n.payload,'$.kind') NOT IN ('file','module','document','group','rationale')
                 ORDER BY d.degree DESC,n.id LIMIT 64",
            StorageLayout::Compact if normalized =>
                "WITH incidents AS (SELECT source_key AS nkey FROM edges UNION ALL SELECT target_key FROM edges),
                 degrees AS (SELECT nkey,count(*) AS degree FROM incidents GROUP BY nkey)
                 SELECT n.label FROM degrees d JOIN nodes n ON n.nkey=d.nkey
                 WHERE n.kind NOT IN ('file','module','document','group','rationale')
                 ORDER BY d.degree DESC,n.id LIMIT 64",
            StorageLayout::Compact =>
                "WITH incidents AS (SELECT source_key AS nkey FROM edges UNION ALL SELECT target_key FROM edges),
                 degrees AS (SELECT nkey,count(*) AS degree FROM incidents GROUP BY nkey)
                 SELECT n.label FROM degrees d JOIN nodes n ON n.nkey=d.nkey
                 WHERE json_extract(n.payload,'$.kind') NOT IN ('file','module','document','group','rationale')
                 ORDER BY d.degree DESC,n.id LIMIT 64",
        };
        let mut statement = tx.prepare(sql)?;
        let mut topics = Vec::new();
        let mut seen = BTreeSet::new();
        for label in statement.query_map([], |row| row.get::<_, String>(0))? {
            let label: String = label?
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '+' | '.' | '#'))
                .take(64)
                .collect();
            let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
            if !label.is_empty() && seen.insert(label.to_lowercase()) {
                topics.push(label);
                if topics.len() == 8 {
                    break;
                }
            }
        }
        drop(statement);
        tx.commit()?;
        Ok(topics)
    }

    /// Read every node and edge in one SQLite read transaction. This explicit
    /// full read has no query limit and never returns a truncated graph.
    pub fn snapshot(&self) -> Result<GraphSnapshot> {
        self.snapshot_inner(None)
    }

    /// Check counts and stored payload bytes inside the same read transaction
    /// before allocating records. Useful for bounded server consumers.
    pub fn snapshot_bounded(
        &self,
        nodes: usize,
        edges: usize,
        references: usize,
        payload_bytes: usize,
    ) -> Result<GraphSnapshot> {
        self.snapshot_inner(Some((nodes, edges, references, payload_bytes)))
    }

    pub(super) fn snapshot_inner(
        &self,
        limits: Option<(usize, usize, usize, usize)>,
    ) -> Result<GraphSnapshot> {
        let tx = self.conn.unchecked_transaction()?;
        let (generation, kind, root, metadata): (i64, String, Option<String>, String) = tx
            .query_row(
                "SELECT generation,kind,root,graph_metadata FROM metadata WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        let layout = storage_layout(&tx)?;
        if let Some((max_nodes, max_edges, max_refs, max_bytes)) = limits {
            let mut total_bytes = metadata.len() as u64;
            for (sql, limit) in [
                (
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM nodes",
                    max_nodes,
                ),
                (
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM edges",
                    max_edges,
                ),
                (
                    match layout {
                        StorageLayout::Legacy => {
                            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM refs WHERE resolved_target IS NULL"
                        }
                        StorageLayout::Compact => {
                            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM refs WHERE resolved_target_key IS NULL"
                        }
                    },
                    max_refs,
                ),
            ] {
                let (count, bytes): (i64, i64) =
                    tx.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
                let (count, bytes) = (u64::try_from(count)?, u64::try_from(bytes)?);
                ensure!(count <= limit as u64, "snapshot record count exceeds limit");
                total_bytes = total_bytes
                    .checked_add(bytes)
                    .context("snapshot byte count overflow")?;
                ensure!(
                    total_bytes <= max_bytes as u64,
                    "snapshot payload exceeds byte limit"
                );
            }
            if kind == "native" {
                // Charge a conservative JSON-escaped representation before
                // loading source proof. Empty files without nodes add nothing.
                let proof_bytes: i64 = tx.query_row(
                    match layout { StorageLayout::Legacy => "SELECT COALESCE(SUM(length(CAST(path AS BLOB))*6+length(CAST(hash AS BLOB))+100),0) FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_file=files.path)", StorageLayout::Compact => "SELECT COALESCE(SUM(length(CAST(path AS BLOB))*6+length(CAST(hash AS BLOB))+100),0) FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_key=files.fkey)" },
                    [], |row| row.get(0),
                )?;
                total_bytes = total_bytes
                    .checked_add(u64::try_from(proof_bytes)?)
                    .and_then(|v| v.checked_add(64))
                    .context("snapshot byte count overflow")?;
                ensure!(
                    total_bytes <= max_bytes as u64,
                    "snapshot source proof exceeds byte limit"
                );
            }
        }
        let nodes = read_payloads(&tx, "SELECT payload FROM nodes ORDER BY id")?;
        let edges = read_payloads(&tx, "SELECT payload FROM edges ORDER BY id")?;
        let mut metadata: serde_json::Value = serde_json::from_str(&metadata)?;
        if kind == "native" {
            let references: Vec<Reference> = read_payloads(
                &tx,
                match layout {
                    StorageLayout::Legacy => {
                        "SELECT payload FROM refs WHERE resolved_target IS NULL ORDER BY id"
                    }
                    StorageLayout::Compact => {
                        "SELECT payload FROM refs WHERE resolved_target_key IS NULL ORDER BY id"
                    }
                },
            )?;
            if metadata.is_null() {
                metadata = serde_json::json!({});
            }
            metadata
                .as_object_mut()
                .context("native graph metadata must be an object")?
                .insert(
                    "graf_unresolved_references".into(),
                    serde_json::to_value(references)?,
                );
            let mut files = serde_json::Map::new();
            let mut statement = tx.prepare(
                match layout { StorageLayout::Legacy => "SELECT path,hash FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_file=files.path) ORDER BY path", StorageLayout::Compact => "SELECT path,hash FROM files WHERE EXISTS(SELECT 1 FROM nodes WHERE owner_key=files.fkey) ORDER BY path" },
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let stamp: String = row.get(1)?;
                if let Some(digest) = crate::stamps::indexed_source_digest(&stamp) {
                    files.insert(path, serde_json::json!(digest));
                }
            }
            // Replace any persisted claim: these rows share this snapshot's
            // generation and node ownership. No source files are read here.
            metadata.as_object_mut().unwrap().insert(
                "graf_source_digests".into(),
                serde_json::json!({"algorithm":"blake3","files":files}),
            );
        }
        let snapshot = GraphSnapshot {
            schema_version: SCHEMA_VERSION,
            generation: u64::try_from(generation)?,
            kind,
            root,
            nodes,
            edges,
            metadata,
        };
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn semantic_losses(&self, changed: &[FileFacts]) -> Result<Vec<String>> {
        let tx = self.conn.unchecked_transaction()?;
        let layout = storage_layout(&tx)?;
        let mut losses = Vec::new();
        for facts in changed {
            // A managed source's capture key stays stable when its display name changes.
            let source_glob = facts.path.strip_prefix(".graf/sources/").and_then(|path| {
                let (key, _) = path.split_once('/')?;
                (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| format!(".graf/sources/{key}/*"))
            });
            let mut old_nodes = 0;
            let mut old_edges = 0;
            for (query, count) in [
                (
                    match layout {
                        StorageLayout::Legacy => {
                            "SELECT payload FROM nodes WHERE owner_file=?1 OR owner_file IN (SELECT path FROM files WHERE path GLOB ?2)"
                        }
                        StorageLayout::Compact => {
                            "SELECT payload FROM nodes WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) OR owner_key IN (SELECT fkey FROM files WHERE path GLOB ?2)"
                        }
                    },
                    &mut old_nodes,
                ),
                (
                    match layout {
                        StorageLayout::Legacy => {
                            "SELECT payload FROM edges WHERE owner_file=?1 OR owner_file IN (SELECT path FROM files WHERE path GLOB ?2)"
                        }
                        StorageLayout::Compact => {
                            "SELECT payload FROM edges WHERE owner_key=(SELECT fkey FROM files WHERE path=?1) OR owner_key IN (SELECT fkey FROM files WHERE path GLOB ?2)"
                        }
                    },
                    &mut old_edges,
                ),
            ] {
                let mut stmt = tx.prepare(query)?;
                for payload in
                    stmt.query_map(params![facts.path, source_glob], |r| r.get::<_, String>(0))?
                {
                    let value: serde_json::Value = serde_json::from_str(&payload?)?;
                    if crate::stamps::semantic_provenance(&value["metadata"]) {
                        *count += 1;
                    }
                }
            }
            let (nodes, edges) = crate::stamps::semantic_counts(facts);
            if nodes < old_nodes || edges < old_edges {
                losses.push(format!(
                    "{} (nodes {old_nodes}->{nodes}, edges {old_edges}->{edges})",
                    facts.path
                ));
            }
        }
        tx.commit()?;
        Ok(losses)
    }

    pub fn apply_native(
        &mut self,
        root: &str,
        changed: Vec<FileFacts>,
        deleted: Vec<String>,
        coverage: Coverage,
    ) -> Result<IndexReport> {
        self.apply_native_inner(root, changed, deleted, coverage, None)
    }

    pub fn apply_native_with_options(
        &mut self,
        root: &str,
        changed: Vec<FileFacts>,
        deleted: Vec<String>,
        coverage: Coverage,
        options: serde_json::Value,
    ) -> Result<IndexReport> {
        self.apply_native_inner(root, changed, deleted, coverage, Some(options))
    }
}
