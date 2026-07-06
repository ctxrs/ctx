#[allow(unused_imports)]
use super::*;

pub(crate) fn read_kimi_session_index(path: &Path) -> BTreeMap<String, KimiSessionIndexEntry> {
    let Ok(text) = read_text_file_limited(
        path,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "Kimi Code CLI session_index.jsonl",
    ) else {
        return BTreeMap::new();
    };
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(session_id) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            continue;
        };
        entries.insert(
            session_id.to_owned(),
            KimiSessionIndexEntry {
                session_id: session_id.to_owned(),
                session_dir: value
                    .get("sessionDir")
                    .or_else(|| value.get("session_dir"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                work_dir: value
                    .get("workDir")
                    .or_else(|| value.get("work_dir"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
        );
    }
    entries
}

pub(crate) fn kimi_session_index_metadata(entry: &KimiSessionIndexEntry) -> Value {
    json!({
        "session_id": entry.session_id,
        "session_dir": entry.session_dir,
        "work_dir": entry.work_dir,
    })
}
