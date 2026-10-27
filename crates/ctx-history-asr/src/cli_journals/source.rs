use crate::{
    source::{decode, line, reader, Source},
    Input, Provider, Result,
};
use serde_json::{json, Value};
use std::{fs, io::Read, path::Path};

pub(crate) fn inspect(
    input: &Input,
    limit: usize,
    visitor: &mut dyn FnMut(Source) -> Result<()>,
    unidentified: &mut u64,
) -> Result<()> {
    if input.provider == Provider::Kimi
        && input.path.file_name().and_then(|s| s.to_str()) != Some("wire.jsonl")
    {
        return Ok(());
    }
    let mut stream = reader(&input.path)?;
    let mut bytes = Vec::new();
    let mut header = None;
    for _ in 0..64 {
        let Some((_, large)) = line(&mut *stream, &mut bytes, limit)? else {
            break;
        };
        if large {
            continue;
        }
        let Some(v) = decode(&bytes) else {
            continue;
        };
        let id = match input.provider {
            Provider::Copilot if v["type"] == "session.start" => {
                v.pointer("/data/sessionId").and_then(Value::as_str)
            }
            Provider::Droid if v["type"] == "session_start" => {
                v["sessionId"].as_str().or_else(|| v["id"].as_str())
            }
            Provider::Qoder => v["sessionId"].as_str(),
            _ => None,
        };
        if let Some(id) = id.filter(|id| !id.is_empty()) {
            header = Some((id.to_owned(), v));
            break;
        }
        if input.provider == Provider::Kimi {
            header = kimi_header(&input.path, limit, &v)?;
            break;
        }
    }
    let Some((session, mut header)) = header else {
        *unidentified += 1;
        return Ok(());
    };
    if input.provider == Provider::Copilot {
        if let Some(cwd) = header.pointer("/data/context/cwd").cloned() {
            header["cwd"] = cwd;
        }
    }
    let metadata = fs::metadata(&input.path)?;
    let mut stamp = format!("{}:{:?}", metadata.len(), metadata.modified()?);
    if input.provider == Provider::Kimi {
        if let Some(state) = input.path.ancestors().nth(3).map(|p| p.join("state.json")) {
            if let Ok(m) = fs::metadata(state) {
                stamp.push_str(&format!(":{}:{:?}", m.len(), m.modified()?));
            }
        }
    }
    visitor(Source {
        provider: input.provider,
        path: input.path.clone(),
        segment: session.clone(),
        session: session.clone(),
        header: header.clone(),
        stamp: stamp.clone(),
    })?;
    if input.provider == Provider::Copilot {
        // Embedded child event streams share the journal, not the conversation.
        let temp = tempfile::tempdir()?;
        let ids = ruopencode_source::Connection::open(temp.path().join("agents.sqlite"))?;
        ids.execute_batch(
            "PRAGMA cache_size=-1024; CREATE TABLE agents(id TEXT PRIMARY KEY); BEGIN;",
        )?;
        let mut stream = reader(&input.path)?;
        while let Some((_, large)) = line(&mut *stream, &mut bytes, limit)? {
            if large {
                continue;
            }
            if let Some(v) = decode(&bytes) {
                if let Some(agent) = v["agentId"].as_str().filter(|s| !s.is_empty()) {
                    ids.execute("INSERT OR IGNORE INTO agents VALUES(?1)", [agent])?;
                }
            }
        }
        ids.execute_batch("COMMIT")?;
        let mut stmt = ids.prepare("SELECT id FROM agents ORDER BY id")?;
        for agent in stmt.query_map([], |r| r.get::<_, String>(0))? {
            let agent = agent?;
            let key = serde_json::to_string(&(&session, &agent))?;
            let mut h = header.clone();
            h["_nativeSession"] = json!(session);
            h["_agent"] = json!(agent);
            visitor(Source {
                provider: input.provider,
                path: input.path.clone(),
                segment: key.clone(),
                session: key,
                header: h,
                stamp: stamp.clone(),
            })?;
        }
    }
    Ok(())
}
fn kimi_header(path: &Path, limit: usize, first: &Value) -> Result<Option<(String, Value)>> {
    let Some(agent_dir) = path.parent() else {
        return Ok(None);
    };
    let Some(agents) = agent_dir.parent().filter(|p| p.ends_with("agents")) else {
        return Ok(None);
    };
    let Some(session_dir) = agents.parent() else {
        return Ok(None);
    };
    let (Some(agent), Some(session)) = (
        agent_dir.file_name().and_then(|s| s.to_str()),
        session_dir.file_name().and_then(|s| s.to_str()),
    ) else {
        return Ok(None);
    };
    // These directory components are the provider's own persistent IDs, not
    // location hashes. The namespace separates agent IDs copied across forks.
    let id = serde_json::to_string(&(session, agent))?;
    let mut h = json!({"_nativeSession":session,"_agent":agent,"_first":first});
    let state_path = session_dir.join("state.json");
    if state_path.is_file() {
        let mut bytes = Vec::new();
        fs::File::open(state_path)?
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() <= limit {
            if let Some(state) = decode(&bytes) {
                if let Some(cwd) = state["workDir"].as_str().or_else(|| state["cwd"].as_str()) {
                    h["cwd"] = json!(cwd);
                }
                h["_state"] = state;
            }
        }
    }
    Ok(Some((id, h)))
}
