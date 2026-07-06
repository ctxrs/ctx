#[allow(unused_imports)]
use super::*;

pub(crate) fn public_record_item_type(record: &HistoryRecord) -> String {
    let item_type = record.kind.trim();
    match item_type {
        "" | "record" => "indexed_item".to_owned(),
        value => value.to_owned(),
    }
}

pub(crate) fn import_record_for_source(source: &SourceInfo) -> HistoryRecord {
    let key = format!(
        "agent-history:{}:{}",
        source.provider.as_str(),
        source.path.display()
    );
    let mut record = HistoryRecord::new(
        format!("{} agent history", source.provider.as_str()),
        format!(
            "Indexed local agent history from {} ({})",
            source.path.display(),
            source.source_format
        ),
        vec!["agent-history".into(), source.provider.as_str().into()],
        "agent_history",
        source.path.parent().map(|path| path.display().to_string()),
    );
    record.id = stable_capture_uuid(&key, "record");
    record
}
