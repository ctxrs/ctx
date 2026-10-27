//! Append producer enrichment only when previously observed native positions are unchanged.
use crate::{spool::Spool, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Seek, Write},
    path::Path,
};

pub(super) fn observe(
    db: &Connection,
    spool: &mut Spool,
    agent: &str,
    session: &str,
) -> Result<bool> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS observations(agent TEXT,session TEXT,pos INTEGER,digest TEXT,PRIMARY KEY(agent,session,pos)); CREATE TEMP TABLE IF NOT EXISTS observation_candidate(pos INTEGER PRIMARY KEY,digest TEXT); DELETE FROM observation_candidate;")?;
    let mut pos = 0;
    db.execute_batch("BEGIN")?;
    while let Some(next) = spool.next(pos)? {
        pos = next;
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&spool.get(pos)?)?)
        );
        db.execute(
            "INSERT INTO observation_candidate VALUES(?1,?2)",
            params![pos, digest],
        )?;
    }
    db.execute_batch("COMMIT")?;
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM observations WHERE agent=?1 AND session=?2) AND NOT EXISTS(SELECT 1 FROM observations o LEFT JOIN observation_candidate c ON o.pos=c.pos WHERE o.agent=?1 AND o.session=?2 AND (c.pos IS NULL OR o.digest<>c.digest))",params![agent,session], |r| r.get(0))?)
}
pub(super) fn remember(db: &Connection, agent: &str, session: &str) -> Result<()> {
    db.execute(
        "DELETE FROM observations WHERE agent=?1 AND session=?2",
        params![agent, session],
    )?;
    db.execute(
        "INSERT INTO observations SELECT ?1,?2,pos,digest FROM observation_candidate",
        params![agent, session],
    )?;
    Ok(())
}
fn comparable(mut v: Value) -> Value {
    let o = v.as_object_mut().unwrap();
    o.remove("seq");
    o.remove("id");
    // Only enrichment IDs have this suffix. Native revision links stay significant.
    if v["supersedes"].is_string() && v["assembly"]["mode"] == "folded" {
        v.as_object_mut().unwrap().remove("supersedes");
    }
    v
}
fn grows(before: &Value, after: &Value) -> bool {
    if before["assembly"]["mode"] != "folded" || after["assembly"]["mode"] != "folded" {
        return false;
    }
    let (Some(a), Some(b)) = (
        before["assembly"]["sourceIds"].as_array(),
        after["assembly"]["sourceIds"].as_array(),
    ) else {
        return false;
    };
    b.len() > a.len() && b.starts_with(a)
}
/// Build the entire suffix before touching the published stream. SQLite holds the
/// effective record index; memory holds at most two records, not the whole session.
pub(super) fn append(parent: &Path, target: &Path, candidate: &mut File) -> Result<Option<u64>> {
    let dir = tempfile::tempdir_in(parent)?;
    let db = Connection::open(dir.path().join("effective.sqlite"))?;
    db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048; CREATE TABLE effective(base TEXT PRIMARY KEY,id TEXT,value TEXT,seen INTEGER DEFAULT 0); BEGIN;")?;
    let mut old_count = 0;
    let mut seq = 0;
    for line in BufReader::new(File::open(target)?).lines() {
        let v: Value = serde_json::from_str(&line?)?;
        let id = v["id"].as_str().unwrap_or("");
        let base = id.split_once(".g").map_or(id, |(base, _)| base);
        db.execute(
            "INSERT OR REPLACE INTO effective(base,id,value) VALUES(?1,?2,?3)",
            params![base, id, serde_json::to_string(&comparable(v.clone()))?],
        )?;
        old_count += 1;
        seq = v["seq"].as_u64().unwrap_or(seq);
    }
    let mut suffix = tempfile::NamedTempFile::new_in(parent)?;
    let mut added = 0;
    candidate.rewind()?;
    for line in BufReader::new(candidate).lines() {
        let mut v: Value = serde_json::from_str(&line?)?;
        let base = v["id"].as_str().unwrap_or("").to_owned();
        let prior: Option<(String, String)> = db
            .query_row(
                "SELECT id,value FROM effective WHERE base=?1",
                [&base],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, raw)) = prior {
            db.execute("UPDATE effective SET seen=1 WHERE base=?1", [&base])?;
            let old: Value = serde_json::from_str(&raw)?;
            if old == comparable(v.clone()) {
                continue;
            }
            if !grows(&old, &v) {
                return Ok(None);
            }
            // This ID depends on source identities, never on response content.
            let hash = Sha256::digest(serde_json::to_vec(&v["assembly"]["sourceIds"])?);
            v["id"] = json!(format!("{base}.g{hash:x}"));
            v["supersedes"] = json!(id);
        }
        seq += 1;
        v["seq"] = json!(seq);
        serde_json::to_writer(suffix.as_file_mut(), &v)?;
        suffix.write_all(b"\n")?;
        added += 1;
    }
    let missing: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM effective WHERE seen=0)",
        [],
        |r| r.get(0),
    )?;
    if missing {
        return Ok(None);
    }
    suffix.as_file_mut().rewind()?;
    let mut out = File::options().append(true).open(target)?;
    std::io::copy(suffix.as_file_mut(), &mut out)?;
    out.flush()?;
    Ok(Some(old_count + added))
}
