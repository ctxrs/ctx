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
    pub record_base: Option<String>,
    slot: u64,
    pub identity: Option<String>,
    identity_offset: u64,
    pub capture_scope: bool,
    pub snapshot: bool,
    pub bundle: Option<std::path::PathBuf>,
    pub exact_id: Option<String>,
    pub capture_scoped: bool,
    native_parts: std::collections::BTreeMap<String, u64>,
    pub count: u64,
    pub last: Option<String>,
    pub gaps: u64,
    pub tip: Option<String>,
    pub turn: Option<String>,
    pub model: Option<String>,
    pub root_session: String,
    pub copied: Option<Value>,
    last_seq: Option<u64>,
    /// Native snapshots separate record identity from capture-local serialization order.
    pub native_identity: Option<String>,
    pub native_snapshot: bool,
    pub native_projection: Option<String>,
    pub native_emissions: std::collections::BTreeMap<String, u64>,
}
impl Writer {
    pub fn new(file: File, provider: Provider, root_session: String) -> Self {
        Self {
            out: BufWriter::new(file),
            provider,
            pos: 0,
            record_base: None,
            slot: 0,
            identity: None,
            identity_offset: 0,
            capture_scope: false,
            snapshot: false,
            bundle: None,
            exact_id: None,
            capture_scoped: false,
            native_parts: Default::default(),
            count: 0,
            last: None,
            gaps: 0,
            tip: None,
            turn: None,
            model: None,
            root_session,
            copied: None,
            last_seq: None,
            native_identity: None,
            native_snapshot: false,
            native_projection: None,
            native_emissions: Default::default(),
        }
    }
    pub fn position(&mut self, pos: u64) {
        self.pos = pos;
        self.exact_id = None;
        self.slot = 0;
        self.copied = None;
        self.identity = None;
        self.identity_offset = 0;
        self.capture_scope = false;
        self.native_emissions.clear();
    }
    pub fn capture_identity(&mut self, key: &str, scoped: bool, snapshot: bool) {
        use sha2::{Digest, Sha256};
        self.identity = Some(format!("n{:x}", Sha256::digest(key.as_bytes())));
        self.identity_offset = self.slot;
        self.capture_scope = scoped;
        self.snapshot = snapshot;
    }
    pub fn capture_scoped(&self) -> bool {
        self.capture_scope
    }
    pub fn slot(&mut self, slot: u64) {
        self.entity_key = None;
        self.slot = slot;
    }
    pub fn entity_identity(&mut self, identity: String) {
        self.identity = Some(identity);
        self.identity_offset = self.slot;
    }
    pub fn id(&self) -> String {
        if let Some(id) = &self.exact_id {
            return id.clone();
        }
        if let Some(key) = &self.native_identity {
            use sha2::{Digest, Sha256};
            return format!(
                "r{:x}",
                Sha256::digest(
                    format!(
                        "{}\0{}",
                        self.root_session,
                        self.native_projection
                            .as_deref()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("{key}\0{}", self.slot))
                    )
                    .as_bytes()
                )
            );
        }
        self.identity
            .as_ref()
            .map(|id| format!("{id}-{}", self.slot - self.identity_offset))
            .unwrap_or_else(|| format!("r{}", self.pos * STRIDE + self.slot))
    }
    pub fn native_position(&mut self, pos: u64, identity: &str) {
        self.position(pos);
        self.identity = Some(crate::native::stable_id(identity));
        self.native_parts.clear();
    }
    /// Override one projection's identity without changing later turn subslots.
    pub fn emit_identified(&mut self, value: Value, source: &Value, key: &str) -> Result<String> {
        let previous = (
            self.identity.clone(),
            self.identity_offset,
            self.capture_scope,
        );
        self.capture_identity(key, false, self.snapshot);
        let result = self.emit(value, source);
        (self.identity, self.identity_offset, self.capture_scope) = previous;
        result
    }
    pub fn emit(&mut self, mut value: Value, source: &Value) -> Result<String> {
        if self.snapshot {
            value["assembly"] = json!({"mode":"snapshot"});
        }
        if self.provider.is_sqlite() {
            if value.get("assembly").is_none() {
                value["assembly"] = json!({"mode":"snapshot"});
            }
            if self.provider == Provider::Kiro {
                value["idScope"] = json!("capture");
            }
        }
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
        let id = if let Some(identity) = self.identity.as_ref().filter(|_| self.provider.mixed()) {
            let entity = [
                "callId",
                "inferenceId",
                "messageId",
                "requestId",
                "compactionId",
                "planId",
            ]
            .iter()
            .find_map(|k| value.get(*k))
            .unwrap_or(&Value::Null);
            let key = json!([identity, value["type"], entity]).to_string();
            let occurrence = self.native_parts.entry(key.clone()).or_default();
            let id = crate::native::stable_id(&json!([key, *occurrence]).to_string());
            if *occurrence > 0 {
                value["idScope"] = json!("capture");
            }
            *occurrence += 1;
            id
        } else {
            self.id()
        };
        if self.capture_scope {
            value["idScope"] = json!("capture");
        }
        if self.snapshot {
            value["assembly"] = json!({"mode":"snapshot"});
        }
        if self.last_seq.is_some_and(|previous| seq <= previous) {
            return Err(Error::Invalid(
                "native item exceeded its reserved ASR emission slots".into(),
            ));
        }
        self.last_seq = Some(seq);
        if self.provider.is_batch8() {
            value.as_object_mut().map(|o| {
                o.entry("assembly")
                    .or_insert_with(|| json!({"mode":"snapshot"}))
            });
            if self.capture_scoped {
                value["idScope"] = json!("capture");
            }
        }
        value["seq"] = json!(seq);
        value["id"] = json!(id);
        if let Some(copied) = &self.copied {
            value["copiedFrom"] = copied.clone();
        }
        if matches!(self.provider, Provider::Droid | Provider::Kimi) {
            value["idScope"] = json!("capture");
        }
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
        if let (true, Some(session), Some(id)) = (
            matches!(self.provider, Provider::Claude | Provider::QwenCode),
            source
                .pointer("/forkedFrom/sessionId")
                .and_then(Value::as_str),
            source
                .pointer("/forkedFrom/messageUuid")
                .and_then(Value::as_str),
        ) {
            value["copiedFrom"] = json!({"session":session_ref(self.provider,session),"record":format!("messageId={id}"),"proof":"native_field"});
            if self.provider == Provider::QwenCode && value["type"] != "message" {
                value["copiedFrom"]
                    .as_object_mut()
                    .unwrap()
                    .remove("record");
            }
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
        if let Some(out) = &self.bundle {
            crate::journals::blobs::externalize(out, &mut value)?;
        }
        serde_json::to_writer(&mut self.out, &value)?;
        self.out.write_all(b"\n")?;
        self.count += 1;
        self.last = Some(id.clone());
        self.slot += 1;
        self.entity_key = None;
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
        let clock = crate::jsonl::clock(self.provider, value);
        self.emit(json!({"type":format!("{}/{}",self.provider.domain(),if safe.is_empty(){"unknown"}else{&safe}),"native":{"record":value}}),if matches!(self.provider, Provider::Pi | Provider::GrokBuild | Provider::DeepSeekHarness) { &clock } else { value })?;
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
    if matches!(provider, Provider::Kimi | Provider::Copilot) {
        if let Ok((namespace, agent)) = serde_json::from_str::<(String, String)>(id) {
            return format!(
                "{}:{}:{}",
                provider.agent(),
                component(&namespace),
                component(&agent)
            );
        }
    }
    format!("{}:{}", provider.agent(), component(id))
}
fn component(id: &str) -> String {
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
    encoded
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
