#[allow(unused_imports)]
use super::*;

pub(crate) fn source_cursor(metadata: &Value) -> Option<String> {
    metadata
        .pointer("/cursor/after/cursor")
        .and_then(|value| value.as_str())
        .or_else(|| metadata.pointer("/cursor").and_then(|value| value.as_str()))
        .map(str::to_owned)
}

pub(crate) fn event_cursor(event: &Event) -> Option<String> {
    if let Some(cursor) = event.payload.get("cursor").and_then(|value| value.as_str()) {
        return Some(cursor.to_owned());
    }
    event
        .payload
        .get("body")
        .and_then(|body| body.get("cursor"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

pub(crate) fn validate_history_source_plugin_output(
    source: &HistorySourcePluginSource,
    stdout: &[u8],
    machine_id: &str,
    require_after_cursor: bool,
) -> Result<()> {
    let mut saw_source = false;
    let mut saw_after_cursor = false;
    for (line_number, line) in history_source_plugin_stdout_lines(source, stdout)? {
        if line.trim().is_empty() {
            continue;
        }
        let record: CtxHistoryJsonlRecord = serde_json::from_str(line).with_context(|| {
            format!(
                "history source plugin {} emitted invalid ctx-history-jsonl-v1 at line {line_number}",
                source.label()
            )
        })?;
        let CtxHistoryJsonlRecord::Source(source_record) = record else {
            continue;
        };
        saw_source = true;
        if source_record
            .cursor
            .as_ref()
            .and_then(|cursor| cursor.after.as_ref())
            .is_some()
        {
            saw_after_cursor = true;
        }
        if source_record.provider_key != source.provider_key
            || source_record.source_id != source.source_id
            || source_record.source_format != source.source_format
        {
            return Err(anyhow!(
                "history source plugin {} emitted source identity {}/{}/{} but manifest declares {}/{}/{}",
                source.label(),
                source_record.provider_key,
                source_record.source_id,
                source_record.source_format,
                source.provider_key,
                source.source_id,
                source.source_format
            ));
        }
        if let Some(source_machine_id) = source_record.machine_id {
            if source_machine_id != machine_id {
                return Err(anyhow!(
                    "history source plugin {} emitted machine_id `{source_machine_id}` but ctx is importing as `{machine_id}`; omit machine_id or set it to CTX_HISTORY_MACHINE_ID",
                    source.label()
                ));
            }
        }
    }
    if !saw_source {
        return Err(anyhow!(
            "history source plugin {} emitted no source record",
            source.label()
        ));
    }
    if require_after_cursor && !saw_after_cursor {
        return Err(anyhow!(
            "history source plugin {} was reset but emitted no source.cursor.after checkpoint; emit a fresh cursor after a full rescan",
            source.label()
        ));
    }
    Ok(())
}
