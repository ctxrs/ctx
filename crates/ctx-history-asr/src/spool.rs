mod batch10;
use crate::{source, Provider, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

/// A bounded SQLite cache plus a byte spool, never a Vec of session bodies.
pub(crate) struct Spool {
    pub db: Connection,
    bytes: File,
    _dir: tempfile::TempDir,
    pub source_bytes: u64,
    source_lines: u64,
    pub limit: usize,
    pub has_secrets: bool,
    pub bundle: std::path::PathBuf,
}
impl Spool {
    pub fn new(parent: &Path, limit: usize) -> Result<Self> {
        let dir = tempfile::tempdir_in(parent)?;
        let db = Connection::open(dir.path().join("index.sqlite"))?;
        db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-4096; PRAGMA temp_store=FILE;
            CREATE TABLE rows(pos INTEGER PRIMARY KEY, off INTEGER, len INTEGER, kind TEXT, native_id TEXT, response TEXT, parent TEXT, call_id TEXT, segment TEXT, ordinal INTEGER, observed INTEGER);
            CREATE INDEX segment_ordinals ON rows(segment,ordinal); CREATE INDEX responses ON rows(response,pos); CREATE INDEX native_ids ON rows(native_id); CREATE INDEX calls ON rows(call_id);
            CREATE TABLE entities(key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE structure(id TEXT PRIMARY KEY, parent TEXT, response TEXT, mechanical INTEGER);
            CREATE TABLE native_calls(call_id TEXT,position INTEGER); CREATE INDEX native_call_ids ON native_calls(call_id); CREATE TABLE tool_results(call_id TEXT,position INTEGER); CREATE INDEX tool_result_ids ON tool_results(call_id);
            CREATE TABLE secret_questions(id TEXT PRIMARY KEY); CREATE TABLE secrets(value TEXT PRIMARY KEY);
            CREATE TABLE turns(position INTEGER PRIMARY KEY, tip TEXT);
            CREATE TABLE outputs(native_id TEXT PRIMARY KEY, record_id TEXT);
            BEGIN;")?;
        let bytes = File::options()
            .create_new(true)
            .read(true)
            .write(true)
            .open(dir.path().join("native.jsonl"))?;
        Ok(Self {
            db,
            bytes,
            _dir: dir,
            source_bytes: 0,
            source_lines: 0,
            limit,
            has_secrets: false,
            bundle: parent.to_owned(),
        })
    }
    pub fn ingest(
        &mut self,
        path: &Path,
        provider: Provider,
        segment: &str,
        allocate: &mut dyn FnMut(&str) -> Result<u64>,
    ) -> Result<()> {
        if provider.batch10() {
            return crate::batch10::source::ingest(self, path, provider);
        }
        let mut reader = source::reader(path)?;
        let mut bytes = Vec::new();
        let mut physical = 0;
        while let Some((size, oversized)) = source::line(&mut *reader, &mut bytes, self.limit)? {
            physical += 1;
            self.source_bytes += size as u64;
            self.source_lines += 1;
            if bytes.iter().all(u8::is_ascii_whitespace) && !oversized {
                continue;
            }
            let value = (!oversized).then(|| source::decode(&bytes)).flatten();
            let v = value.unwrap_or_else(|| json!({"type":"_gap", "reason":if oversized {"conversion_loss"} else {"unparseable"}}));
            let local = if provider == Provider::Codex {
                v["ordinal"]
                    .as_u64()
                    .map(|n| format!("o{n}"))
                    .unwrap_or_else(|| format!("l{physical}"))
            } else {
                v["uuid"]
                    .as_str()
                    .map(|id| format!("uuid:{id}"))
                    .unwrap_or_else(|| format!("l{physical}"))
            };
            let pos = allocate(&local)?;
            crate::privacy::questions(self, &v)?;
            let off = self.bytes.stream_position()?;
            let serialized = serde_json::to_vec(&v)?;
            self.bytes.write_all(&serialized)?;
            let payload = if provider == Provider::Codex {
                &v["payload"]
            } else {
                &v
            };
            let kind = v["type"].as_str().unwrap_or("unknown");
            let native = if provider == Provider::Codex {
                payload["id"].as_str()
            } else {
                v["uuid"].as_str()
            };
            let response = if kind == "assistant" {
                v.pointer("/message/id").and_then(Value::as_str)
            } else {
                None
            };
            let call = payload["call_id"].as_str();
            if provider == Provider::Codex
                && kind == "response_item"
                && matches!(
                    payload["type"].as_str(),
                    Some(
                        "function_call"
                            | "custom_tool_call"
                            | "local_shell_call"
                            | "tool_search_call"
                    )
                )
            {
                if let Some(id) = call.or_else(|| payload["id"].as_str()) {
                    self.db
                        .execute("INSERT INTO native_calls VALUES(?1,?2)", params![id, pos])?;
                }
            }
            if provider == Provider::Claude {
                for block in v
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if block["type"] == "tool_use" {
                        if let Some(id) = block["id"].as_str() {
                            self.db.execute(
                                "INSERT INTO native_calls VALUES(?1,?2)",
                                params![id, pos],
                            )?;
                        }
                    }
                    if block["type"] == "tool_result" {
                        if let Some(id) = block["tool_use_id"].as_str() {
                            self.db.execute(
                                "INSERT INTO tool_results VALUES(?1,?2)",
                                params![id, pos],
                            )?;
                        }
                    }
                }
            }
            // Segment and ordinal identify an observation, including overlapping revert suffixes.
            self.db.execute(
                "INSERT OR REPLACE INTO rows VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    pos,
                    off,
                    serialized.len(),
                    kind,
                    native,
                    response,
                    v["parentUuid"].as_str(),
                    call,
                    segment,
                    v["ordinal"].as_u64(),
                    self.source_lines
                ],
            )?;
        }
        self.bytes.flush()?;
        Ok(())
    }
    pub fn finish(&self) -> Result<()> {
        self.db.execute_batch("COMMIT;")?;
        Ok(())
    }
    pub fn get(&mut self, pos: u64) -> Result<Value> {
        let (off, len): (u64, usize) =
            self.db
                .query_row("SELECT off,len FROM rows WHERE pos=?1", [pos], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
        self.bytes.seek(SeekFrom::Start(off))?;
        let mut bytes = vec![0; len];
        self.bytes.read_exact(&mut bytes)?;
        let mut value = serde_json::from_slice(&bytes)?;
        crate::privacy::sanitize(self, &mut value)?;
        Ok(value)
    }
    pub fn next(&self, pos: u64) -> Result<Option<u64>> {
        Ok(self
            .db
            .prepare_cached("SELECT pos FROM rows WHERE pos>?1 ORDER BY pos LIMIT 1")?
            .query_row([pos], |r| r.get(0))
            .optional()?)
    }
    pub fn entity(&self, key: &str) -> Result<Option<Value>> {
        let raw: Option<String> = self
            .db
            .query_row("SELECT value FROM entities WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }
    pub fn ambiguous_call(&self, id: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT count(DISTINCT position)>1 FROM native_calls WHERE call_id=?1",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn save(&self, key: &str, value: &Value) -> Result<()> {
        self.db
            .prepare_cached("INSERT OR REPLACE INTO entities VALUES(?1,?2)")?
            .execute(params![key, serde_json::to_string(value)?])?;
        Ok(())
    }
    pub fn output(&self, native: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT record_id FROM outputs WHERE native_id=?1",
                [native],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn set_output(&self, native: &str, id: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO outputs VALUES(?1,?2)",
            params![native, id],
        )?;
        Ok(())
    }
}

impl Spool {
    pub fn blob(&self, mut file: tempfile::NamedTempFile) -> Result<Value> {
        use sha2::{Digest, Sha256};
        file.as_file_mut().rewind()?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            hash.update(&buffer[..n]);
        }
        let digest = format!("{:x}", hash.finalize());
        let dir = self.bundle.join("blobs/sha256");
        std::fs::create_dir_all(&dir)?;
        let target = dir.join(&digest);
        if !target.exists() {
            file.persist(target).map_err(|e| e.error)?;
        }
        Ok(json!({"blob":format!("sha256:{digest}"),"size":size,"mimeType":"application/json"}))
    }
}
