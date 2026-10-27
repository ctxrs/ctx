use crate::{spool::Spool, Error, Provider, Result};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{BufWriter, Write},
};

pub(crate) const STRIDE: u64 = 1_000_000;
pub(crate) struct Writer {
    pub out: BufWriter<File>,
    pub provider: Provider,
    pub pos: u64,
    slot: u64,
    pub count: u64,
    pub last: Option<String>,
    pub gaps: u64,
    pub tip: Option<String>,
    pub turn: Option<String>,
    pub model: Option<String>,
    pub root_session: String,
}
impl Writer {
    pub fn new(file: File, provider: Provider, root_session: String) -> Self {
        Self {
            out: BufWriter::new(file),
            provider,
            pos: 0,
            slot: 0,
            count: 0,
            last: None,
            gaps: 0,
            tip: None,
            turn: None,
            model: None,
            root_session,
        }
    }
    pub fn position(&mut self, pos: u64) {
        self.pos = pos;
        self.slot = 0;
    }
    pub fn slot(&mut self, slot: u64) {
        self.slot = slot;
    }
    pub fn id(&self) -> String {
        format!("r{}", self.pos * STRIDE + self.slot)
    }
    pub fn emit(&mut self, mut value: Value, source: &Value) -> Result<String> {
        let seq = self
            .pos
            .checked_mul(STRIDE)
            .and_then(|n| n.checked_add(self.slot))
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or_else(|| Error::Invalid("ASR sequence space exhausted".into()))?;
        if self.slot >= STRIDE {
            return Err(Error::Invalid(
                "too many records for one native item".into(),
            ));
        }
        let id = format!("r{seq}");
        value["seq"] = json!(seq);
        value["id"] = json!(id);
        if let Some(ts) = source
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(timestamp)
        {
            value["ts"] = json!(ts);
        }
        if !matches!(value["type"].as_str(), Some("session")) {
            let turn = source
                .get("promptId")
                .and_then(Value::as_str)
                .or_else(|| source.get("turn_id").and_then(Value::as_str))
                .or_else(|| {
                    source
                        .pointer("/internal_chat_message_metadata_passthrough/turn_id")
                        .and_then(Value::as_str)
                })
                .or(self.turn.as_deref());
            if let Some(turn) = turn {
                value["turnId"] = json!(turn);
            }
        }
        if let (Some(session), Some(id)) = (
            source
                .pointer("/forkedFrom/sessionId")
                .and_then(Value::as_str),
            source
                .pointer("/forkedFrom/messageUuid")
                .and_then(Value::as_str),
        ) {
            value["copiedFrom"] = json!({"session":session_ref(self.provider,session),"record":format!("messageId={id}"),"proof":"native_field"});
        }
        if context(&value) && value.get("supersedes").is_none() {
            self.tip = Some(id.clone());
        }
        if value["type"] == "rewind" {
            self.tip = value["to"].as_str().map(str::to_owned);
        }
        if value["type"] == "gap" {
            self.gaps += 1;
        }
        serde_json::to_writer(&mut self.out, &value)?;
        self.out.write_all(b"\n")?;
        self.count += 1;
        self.last = Some(id.clone());
        self.slot += 1;
        Ok(id)
    }
    pub fn revise(
        &mut self,
        spool: &Spool,
        key: &str,
        mut value: Value,
        source: &Value,
    ) -> Result<String> {
        if let Some(previous) = spool.entity(key)? {
            value["supersedes"] = previous["_record"].clone();
        }
        let id = self.emit(value.clone(), source)?;
        value["_record"] = json!(id);
        spool.save(key, &value)?;
        Ok(id)
    }
    pub fn vendor(&mut self, value: &Value, kind: &str) -> Result<()> {
        let safe = kind
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        self.emit(json!({"type":format!("{}/{}",self.provider.domain(),if safe.is_empty(){"unknown"}else{&safe}),"native":{"record":value}}),value)?;
        Ok(())
    }
}
pub(crate) fn context(v: &Value) -> bool {
    matches!(
        v["type"].as_str(),
        Some("message" | "tool_call" | "tool_result" | "compaction")
    )
}
pub(crate) fn timestamp(s: &str) -> Option<String> {
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    if s.ends_with('Z') {
        Some(s.into())
    } else {
        Some(
            dt.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        )
    }
}
pub(crate) fn copy(target: &mut Value, dest: &str, source: &Value, key: &str) {
    if let Some(v) = source.get(key).filter(|v| !v.is_null()) {
        target[dest] = v.clone();
    }
}
pub(crate) fn meta(target: &mut Value, provider: Provider, key: &str, value: &Value) {
    if target.get("_meta").is_none() {
        target["_meta"] = json!({});
    }
    target["_meta"][format!("{}/{key}", provider.domain())] = value.clone();
}
pub(crate) fn extras(target: &mut Value, provider: Provider, source: &Value, covered: &[&str]) {
    if let Some(fields) = source.as_object() {
        for (key, v) in fields {
            if !covered.contains(&key.as_str())
                && !matches!(key.as_str(), "timestamp" | "type" | "_sidecar")
            {
                meta(target, provider, key, v);
            }
        }
    }
}
pub(crate) fn session_ref(provider: Provider, id: &str) -> String {
    let mut encoded = String::new();
    for c in id.chars() {
        if c.is_control() || matches!(c, '%' | ':' | '#' | ' ') {
            for b in c.to_string().as_bytes() {
                encoded.push_str(&format!("%{b:02X}"));
            }
        } else {
            encoded.push(c);
        }
    }
    format!("{}:{encoded}", provider.agent())
}
pub(crate) fn path(value: &str, cwd: Option<&str>) -> Value {
    let spelling = value.replace('\\', "/");
    let absolute = spelling.starts_with('/') || spelling.as_bytes().get(1) == Some(&b':');
    let relative = if let Some(cwd) = cwd {
        if spelling == cwd {
            Some(".")
        } else {
            spelling.strip_prefix(&format!("{}/", cwd.trim_end_matches('/')))
        }
    } else {
        None
    };
    let mut result = if let Some(relative) =
        relative.or_else(|| (!absolute).then_some(spelling.as_str()))
    {
        if relative.split('/').any(|p| p == "..") && cwd.is_some() {
            json!({"root":null,"path":if absolute {spelling.clone()}else{format!("{}/{relative}",cwd.unwrap_or_default())}})
        } else {
            let normalized = relative
                .split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect::<Vec<_>>()
                .join("/");
            json!({"root":if cwd.is_some(){"workspace"}else{"unknown"},"path":if normalized.is_empty(){"."}else{&normalized}})
        }
    } else {
        json!({"root":null,"path":spelling})
    };
    if result["path"].as_str() != Some(value) {
        result["nativePath"] = json!(value);
    }
    result
}
