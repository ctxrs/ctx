mod batch10;
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

/// A bounded SQLite cache plus a byte spool, never a Vec of session bodies.
pub struct Spool {
    pub db: Connection,
    pub bytes: File,
    _dir: tempfile::TempDir,
    pub source_bytes: u64,
    pub source_lines: u64,
    pub limit: usize,
    pub has_secrets: bool,
    pub bundle: std::path::PathBuf,
}
impl Spool {
    /// Native cursors spool one bounded row at a time. Positions are capture-local;
    /// the cursor carries stable source identity independently of serialization order.
    pub fn push_native(&mut self, value: &Value) -> Result<()> {
        if let Some(identity) = value.get("identity") {
            let key = format!("native-identity:{identity}");
            if self.entity(&key)?.is_some() {
                return Err(crate::Error::Invalid(
                    "conflicting native row identities across selected sources".into(),
                ));
            }
            self.save(&key, &json!(true))?;
        }
        crate::privacy::questions(self, value)?;
        self.source_lines += 1;
        let pos = self.source_lines;
        let off = self.bytes.stream_position()?;
        let serialized = serde_json::to_vec(value)?;
        self.source_bytes += serialized.len() as u64;
        self.bytes.write_all(&serialized)?;
        let (kind, native, response, parent) = if value.get("identity").is_some() {
            (
                value["table"].as_str(),
                value["row"]["id"].as_str(),
                None,
                value["row"]["parentId"].as_str(),
            )
        } else {
            (
                value["type"].as_str(),
                value["node"].as_str(),
                value["response"].as_str(),
                value["parent"].as_str(),
            )
        };
        self.db.execute("INSERT INTO rows(pos,off,len,kind,native_id,response,parent,segment,observed) VALUES(?1,?2,?3,?4,?5,?6,?7,'',?1)", params![pos,off,serialized.len(),kind,native,response,parent])?;
        Ok(())
    }
    pub fn new(parent: &Path, limit: usize) -> Result<Self> {
        let dir = tempfile::tempdir_in(parent)?;
        let db = Connection::open(dir.path().join("index.sqlite"))?;
        db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-4096; PRAGMA temp_store=FILE;
            CREATE TABLE rows(pos INTEGER PRIMARY KEY, off INTEGER, len INTEGER, kind TEXT, native_id TEXT, response TEXT, parent TEXT, call_id TEXT, segment TEXT, ordinal INTEGER, observed INTEGER);
            CREATE INDEX segment_ordinals ON rows(segment,ordinal); CREATE INDEX responses ON rows(response,pos); CREATE INDEX native_ids ON rows(native_id); CREATE INDEX calls ON rows(call_id);
            CREATE TABLE native_positions(pos INTEGER PRIMARY KEY, segment TEXT, line INTEGER, ordinal TEXT);
            CREATE TABLE entities(key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE structure(id TEXT PRIMARY KEY, parent TEXT, response TEXT, mechanical INTEGER);
            CREATE TABLE journal_parts(group_id TEXT,position INTEGER,PRIMARY KEY(group_id,position));
            CREATE INDEX journal_part_positions ON journal_parts(position);
            CREATE TABLE journal_calls(kind TEXT,call_id TEXT,position INTEGER,PRIMARY KEY(kind,call_id,position)); CREATE INDEX journal_call_ids ON journal_calls(call_id,kind); CREATE INDEX journal_call_positions ON journal_calls(position);
            CREATE TABLE native_calls(call_id TEXT,position INTEGER); CREATE INDEX native_call_ids ON native_calls(call_id); CREATE INDEX native_call_positions ON native_calls(position); CREATE TABLE tool_results(call_id TEXT,position INTEGER); CREATE INDEX tool_result_ids ON tool_results(call_id);
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
    /// Adds one bounded native SQLite row in capture-local serialization order.
    pub fn push_sqlite(&mut self, pos: u64, value: &Value) -> Result<()> {
        let off = self.bytes.stream_position()?;
        let serialized = serde_json::to_vec(value)?;
        self.bytes.write_all(&serialized)?;
        self.db.execute("INSERT INTO rows(pos,off,len,kind,native_id,segment,observed) VALUES(?1,?2,?3,?4,?5,'sqlite',?1)", params![pos,off,serialized.len(),value["_table"].as_str(),value["_key"].as_str()])?;
        Ok(())
    }
    /// Capture-local serialization positions; stable identity is carried separately.
    pub fn push(&mut self, value: &Value, key: Option<&str>) -> Result<u64> {
        crate::privacy::questions(self, value)?;
        self.source_lines += 1;
        let pos = self.source_lines;
        let off = self.bytes.stream_position()?;
        let bytes = serde_json::to_vec(value)?;
        self.bytes.write_all(&bytes)?;
        self.db.execute(
            "INSERT INTO rows(pos,off,len,native_id) VALUES(?1,?2,?3,?4)",
            params![pos, off, bytes.len(), key],
        )?;
        Ok(pos)
    }
    pub fn unique(&self, key: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT count(*)=1 FROM rows WHERE native_id=?1",
            [key],
            |r| r.get(0),
        )?)
    }
    pub fn finish(&self) -> Result<()> {
        self.db.execute_batch("COMMIT;")?;
        Ok(())
    }
    /// Native SQLite cursors already supply their deterministic serialization order.
    pub fn push_relational(
        &mut self,
        kind: &str,
        key: &str,
        value: &Value,
        capture_scope: bool,
    ) -> Result<()> {
        crate::privacy::questions(self, value)?;
        self.source_lines += 1;
        let pos = self.source_lines;
        let row = json!({"kind":kind,"key":key,"data":value,"capture_scope":capture_scope});
        let bytes = serde_json::to_vec(&row)?;
        self.source_bytes += bytes.len() as u64;
        let off = self.bytes.stream_position()?;
        self.bytes.write_all(&bytes)?;
        self.db.execute(
            "INSERT INTO rows(pos,off,len,kind,native_id) VALUES(?1,?2,?3,?4,?5)",
            params![pos, off, bytes.len(), kind, key],
        )?;
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
