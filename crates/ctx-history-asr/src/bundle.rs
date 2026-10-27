mod header;
use crate::{
    claude, codex,
    records::{session_ref, Writer},
    source,
    spool::Spool,
    Error, Options, Provider, Result, Summary,
};
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};
const REVISION: &str = "ctx-asr-7";

pub fn export(options: &Options) -> Result<Summary> {
    if options.max_record_bytes < 1024 {
        return Err(Error::Invalid(
            "max record bytes must be at least 1024".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&options.out)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(&options.out)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(options.out.join(".ctx-asr.lock"))?;
    lock.lock_exclusive()?;
    let db = Connection::open(options.out.join(".ctx-asr.sqlite"))?;
    db.execute_batch("PRAGMA cache_size=-4096; PRAGMA temp_store=FILE;
        CREATE TABLE IF NOT EXISTS exports(agent TEXT, session TEXT, stamp TEXT, entry TEXT, PRIMARY KEY(agent,session));
        CREATE TABLE IF NOT EXISTS positions(agent TEXT,session TEXT,segment TEXT,local TEXT,pos INTEGER,PRIMARY KEY(agent,session,segment,local));
        CREATE INDEX IF NOT EXISTS observation_order ON positions(agent,session,pos);
        CREATE TEMP TABLE sources(agent TEXT,session TEXT,path TEXT,stamp TEXT,header TEXT,segment TEXT,PRIMARY KEY(agent,session,path));
        CREATE INDEX source_sessions ON sources(agent,session,path);")?;
    let mut summary = Summary::default();
    for input in &options.inputs {
        source::visit(
            input,
            options.max_record_bytes,
            &mut |mut s| {
                if options.session.as_deref().is_none_or(|id| id == s.session) {
                    if s.provider.is_relational() {
                        if let Some(namespace) = options.namespace.as_deref() {
                            if namespace.is_empty() {
                                return Err(Error::Invalid(
                                    "ASR namespace must be nonempty".into(),
                                ));
                            }
                            s.header["_namespace"] = json!(namespace);
                        }
                        if s.provider == Provider::Hermes && !s.header["_namespace"].is_string() {
                            return Err(Error::Invalid("Hermes store lacks state_meta.store_instance_id; supply a stable installation --namespace and reuse it after relocation".into()));
                        }
                    }
                    db.execute(
                        "INSERT OR IGNORE INTO sources VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            s.provider.agent(),
                            crate::relational::storage_key(&s),
                            s.path.to_string_lossy(),
                            s.stamp,
                            serde_json::to_string(&s.header)?,
                            s.segment
                        ],
                    )?;
                }
                Ok(())
            },
            &mut summary.unidentified_sources,
        )?;
    }
    let mut manifest = tempfile::NamedTempFile::new_in(&options.out)?;
    let mut output = BufWriter::new(manifest.as_file_mut());
    write!(output, "{{\"formatVersion\":\"0.1\",\"purpose\":\"archive\",\"producer\":{{\"name\":\"ctx\",\"version\":\"{}\"}},\"sessions\":[", env!("CARGO_PKG_VERSION"))?;
    let mut previous = (String::new(), String::new());
    let mut comma = false;
    loop {
        let next:Option<(String,String)>=db.query_row("SELECT agent,session FROM sources WHERE (agent,session)>(?1,?2) GROUP BY agent,session ORDER BY agent,session LIMIT 1",params![previous.0,previous.1],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((agent, session)) = next else {
            break;
        };
        previous = (agent.clone(), session.clone());
        let provider = match agent.as_str() {
            "codex" => Provider::Codex,
            "claude-code" => Provider::Claude,
            "com.nousresearch.hermes" => Provider::Hermes,
            "app.astrbot" => Provider::AstrBot,
            "dev.exe.shelley" => Provider::Shelley,
            "zed" => Provider::Zed,
            _ => return Err(Error::Invalid("unknown ASR provider".into())),
        };
        if provider.is_relational() {
            let sources: u64 = db.query_row(
                "SELECT count(*) FROM sources WHERE agent=?1 AND session=?2",
                params![agent, session],
                |r| r.get(0),
            )?;
            if sources > 1 {
                return Err(Error::Invalid("multiple native databases claim the same session; select one source, or export distinct installations with separate stable namespaces".into()));
            }
        }
        let mut hash = Sha256::new();
        hash.update(REVISION);
        if provider.is_relational() {
            hash.update(crate::relational::REVISION);
        }
        hash.update(env!("CARGO_PKG_VERSION"));
        hash.update(options.max_record_bytes.to_le_bytes());
        let mut stmt = db.prepare(
            "SELECT path,stamp FROM sources WHERE agent=?1 AND session=?2 ORDER BY path",
        )?;
        let mut rows = stmt.query(params![agent, session])?;
        while let Some(row) = rows.next()? {
            for n in 0..2 {
                hash.update(row.get::<_, String>(n)?.as_bytes());
                hash.update([0]);
            }
        }
        let stamp = format!("{:x}", hash.finalize());
        let cached: Option<(String, String)> = db
            .query_row(
                "SELECT stamp,entry FROM exports WHERE agent=?1 AND session=?2",
                params![agent, session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let old = cached
            .as_ref()
            .map(|(_, raw)| serde_json::from_str::<Value>(raw))
            .transpose()?;
        let unchanged = cached.as_ref().is_some_and(|(s, _)| *s == stamp)
            && old
                .as_ref()
                .and_then(|e| e["path"].as_str())
                .is_some_and(|p| {
                    std::fs::metadata(options.out.join(p))
                        .ok()
                        .is_some_and(|m| {
                            Some(m.len())
                                == old
                                    .as_ref()
                                    .and_then(|v| v.pointer("/_meta/rs.ctx~1bytes"))
                                    .and_then(Value::as_u64)
                        })
                });
        let entry = if unchanged {
            summary.unchanged += 1;
            old.clone()
                .ok_or_else(|| Error::Invalid("missing cached manifest entry".into()))?
        } else {
            let mut spool = Spool::new(&options.out, options.max_record_bytes)?;
            let mut stmt = db.prepare(
                "SELECT path,header,segment FROM sources WHERE agent=?1 AND session=?2 ORDER BY json_extract(header,'$.timestamp'),path",
            )?;
            let mut rows = stmt.query(params![agent, session])?;
            let mut source_header = None;
            db.execute_batch("BEGIN")?;
            let mut last_pos: u64 = db.query_row(
                "SELECT coalesce(max(pos),0) FROM positions WHERE agent=?1 AND session=?2",
                params![agent, session],
                |r| r.get(0),
            )?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                if source_header.is_none() {
                    source_header = Some(serde_json::from_str::<Value>(&row.get::<_, String>(1)?)?);
                }
                let segment: String = row.get(2)?;
                let header: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
                spool.save(&format!("segment:{segment}"), &header)?;
                if provider.is_relational() {
                    crate::relational::ingest(&mut spool, provider, Path::new(&path), &header)?;
                } else {
                    spool.ingest(Path::new(&path), provider, &segment, &mut |local| {
                    let old: Option<u64> = db.query_row("SELECT pos FROM positions WHERE agent=?1 AND session=?2 AND segment=?3 AND local=?4",params![agent,session,segment,local],|r|r.get(0)).optional()?;
                    if let Some(pos)=old { return Ok(pos); }
                    last_pos+=1;
                    db.execute("INSERT INTO positions VALUES(?1,?2,?3,?4,?5)",params![agent,session,segment,local,last_pos])?;
                    Ok(last_pos)
                })?;
                }
            }
            crate::privacy::prepare(&mut spool)?;
            db.execute_batch("COMMIT")?;
            let mut source_header = if provider.is_relational() {
                spool.entity("native_header")?.unwrap_or(json!({}))
            } else {
                source_header.unwrap_or(json!({}))
            };
            crate::privacy::sanitize(&spool, &mut source_header)?;
            header::resolve(&db, provider, &session, &mut source_header)?;
            if provider == Provider::Claude {
                let fork: Option<u64> = spool
                    .db
                    .query_row(
                        "SELECT pos FROM rows WHERE kind='fork-context-ref' ORDER BY pos LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(pos) = fork {
                    source_header["_forkRef"] = spool.get(pos)?;
                }
            }
            let mut temporary = tempfile::NamedTempFile::new_in(&options.out)?;
            let root = source_header
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or(&session);
            let mut writer = Writer::new(
                temporary.reopen()?,
                provider,
                if provider.is_relational() {
                    crate::relational::reference(
                        provider,
                        &source_header,
                        source_header["_sessionId"].as_str().unwrap_or(root),
                    )
                } else {
                    session_ref(provider, root)
                },
            );
            let mut stream_header = header::make(provider, &session, &source_header);
            if provider.is_relational() {
                stream_header["observedAt"] =
                    json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
            }
            let copied_boundary =
                header::copied_boundary(&spool, provider, &session, &source_header)?;
            let mut copied_last = None;
            let cwd = source_header["cwd"].as_str();
            let subagent = source_header["agentId"].as_str() == Some(session.as_str());
            let mut pos = 0;
            let mut segment = String::new();
            while let Some(next) = spool.next(pos)? {
                pos = next;
                writer.position(pos);
                let value = spool.get(pos)?;
                if provider == Provider::Codex {
                    let current: String = spool.db.query_row(
                        "SELECT segment FROM rows WHERE pos=?1",
                        [pos],
                        |r| r.get(0),
                    )?;
                    if current != segment {
                        codex::segment(&spool, &mut writer, &current, &value)?;
                        segment = current;
                    }
                }
                match provider {
                    Provider::Claude => {
                        claude::convert(&mut spool, &mut writer, pos, &value, cwd, subagent)?
                    }
                    Provider::Codex => codex::convert(&mut spool, &mut writer, pos, &value, cwd)?,
                    _ => {
                        crate::relational::convert(&mut spool, &mut writer, &source_header, &value)?
                    }
                }
                if copied_boundary.is_some_and(|n| pos <= n) {
                    copied_last = writer.last.clone();
                }
                if provider == Provider::Codex {
                    spool.save(
                        &format!(
                            "tip:{segment}:{}",
                            value["ordinal"].as_u64().unwrap_or(pos - 1)
                        ),
                        &json!(writer.tip),
                    )?;
                }
            }
            spool.finish()?;
            writer.out.flush()?;
            let count = writer.count + 1;
            summary.gaps += writer.gaps;
            drop(writer);
            if let Some(last) = copied_last {
                stream_header["lineage"]["inherits"]["localThroughId"] = json!(last);
            }
            let mut complete = tempfile::NamedTempFile::new_in(&options.out)?;
            stream_header["seq"] = json!(0);
            stream_header["id"] = json!("r0");
            serde_json::to_writer(complete.as_file_mut(), &stream_header)?;
            complete.write_all(b"\n")?;
            temporary.as_file_mut().rewind()?;
            std::io::copy(temporary.as_file_mut(), complete.as_file_mut())?;
            temporary = complete;
            let (digest, size) = digest(temporary.as_file_mut())?;
            let storage = format!(
                "{:x}",
                Sha256::digest(session_ref(provider, &session).as_bytes())
            );
            let default = format!("sessions/{agent}/{storage}.jsonl");
            let prior = old
                .as_ref()
                .and_then(|v| v["path"].as_str())
                .unwrap_or(&default);
            let target = options.out.join(prior);
            fs::create_dir_all(
                target
                    .parent()
                    .ok_or_else(|| Error::Invalid("stream has no directory".into()))?,
            )?;
            let path = if !target.exists() {
                temporary.persist(&target).map_err(|e| e.error)?;
                prior.to_string()
            } else if !provider.is_relational() && append_prefix(&target, temporary.as_file_mut())?
            {
                prior.to_string()
            } else {
                summary.snapshots += 1;
                let path = format!("sessions/{agent}/{storage}-{digest}.jsonl");
                let target = options.out.join(&path);
                if !target.exists() {
                    temporary.persist(&target).map_err(|e| e.error)?;
                }
                path
            };
            summary.source_bytes += spool.source_bytes;
            let mut entry = json!({"agent":agent,"sessionId":session,"status":"present","path":path,"digest":{"alg":"sha256","value":digest},"recordCount":count,"_meta":{"rs.ctx/bytes":size}});
            if provider.is_relational() {
                entry["sessionId"] = source_header["_sessionId"].clone();
                entry["namespace"] = source_header["_namespace"].clone();
            }
            db.execute(
                "INSERT OR REPLACE INTO exports VALUES(?1,?2,?3,?4)",
                params![agent, session, stamp, serde_json::to_string(&entry)?],
            )?;
            entry
        };
        summary.sessions += 1;
        summary.records += entry["recordCount"].as_u64().unwrap_or(0);
        summary.output_bytes += entry
            .pointer("/_meta/rs.ctx~1bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if comma {
            output.write_all(b",")?;
        }
        comma = true;
        serde_json::to_writer(&mut output, &entry)?;
    }
    output.write_all(b"]")?;
    crate::relational::bundle_facts(options, &mut output)?;
    output.write_all(b"}\n")?;
    output.flush()?;
    drop(output);
    if summary.sessions == 0 && options.session.is_some() {
        return Err(Error::Invalid(
            "native session ID was not found in selected sources".into(),
        ));
    }
    manifest
        .persist(options.out.join("bundle.json"))
        .map_err(|e| e.error)?;
    Ok(summary)
}
fn digest(file: &mut File) -> Result<(String, u64)> {
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut size = 0;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        size += n as u64;
    }
    Ok((format!("{:x}", hash.finalize()), size))
}
fn append_prefix(path: &Path, new: &mut File) -> Result<bool> {
    let mut old = File::options().read(true).write(true).open(path)?;
    let mut length = old.metadata()?.len();
    // Only the incomplete tail is discarded; complete records remain immutable.
    let mut end = length;
    let mut buffer = [0u8; 65536];
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let n = (end - start) as usize;
        old.seek(SeekFrom::Start(start))?;
        old.read_exact(&mut buffer[..n])?;
        if let Some(i) = buffer[..n].iter().rposition(|b| *b == b'\n') {
            length = start + i as u64 + 1;
            break;
        }
        end = start;
    }
    if end == 0 {
        length = 0;
    }
    if length != old.metadata()?.len() {
        old.set_len(length)?;
    }
    old.rewind()?;
    if length > new.metadata()?.len() {
        return Ok(false);
    }
    let mut old_reader = BufReader::new(old.try_clone()?);
    new.rewind()?;
    let mut a = [0u8; 65536];
    let mut b = [0u8; 65536];
    loop {
        let n = old_reader.read(&mut a)?;
        if n == 0 {
            break;
        }
        new.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Ok(false);
        }
    }
    let mut old = old;
    old.seek(SeekFrom::End(0))?;
    std::io::copy(new, &mut old)?;
    old.flush()?;
    Ok(true)
}
