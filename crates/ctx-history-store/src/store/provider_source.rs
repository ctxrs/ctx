#[allow(unused_imports)]
use super::*;

pub(crate) fn provider_event_dedupe_key_prefix(parsed: &ParsedProviderEventDedupeKey) -> String {
    if let Some(source_id) = &parsed.source_id {
        format!("provider-source:{source_id}:{}:", parsed.provider_index)
    } else {
        format!(
            "provider:{}:{}:{}:",
            parsed.provider, parsed.external_session_id, parsed.provider_index
        )
    }
}

pub(crate) fn parse_provider_event_dedupe_key(
    dedupe_key: &str,
) -> Option<ParsedProviderEventDedupeKey> {
    if let Some(rest) = dedupe_key.strip_prefix("provider-source:") {
        let mut parts = rest.splitn(3, ':');
        let source_id = parts.next()?.to_owned();
        let provider_index = parts.next()?.parse().ok()?;
        let payload_hash = parts.next()?.to_owned();
        if source_id.is_empty() || payload_hash.is_empty() {
            return None;
        }
        return Some(ParsedProviderEventDedupeKey {
            provider: "provider-source".to_owned(),
            external_session_id: source_id.clone(),
            source_id: Some(source_id),
            provider_index,
            payload_hash,
        });
    }

    let mut parts = dedupe_key.splitn(5, ':');
    let prefix = parts.next()?;
    if prefix != "provider" {
        return None;
    }
    let provider = parts.next()?.to_owned();
    let external_session_id = parts.next()?.to_owned();
    let provider_index = parts.next()?.parse().ok()?;
    let payload_hash = parts.next()?.to_owned();
    if provider.is_empty() || external_session_id.is_empty() || payload_hash.is_empty() {
        None
    } else {
        Some(ParsedProviderEventDedupeKey {
            provider,
            external_session_id,
            source_id: None,
            provider_index,
            payload_hash,
        })
    }
}

pub(crate) fn catalog_session_select_sql(tail: &str) -> String {
    format!(
        "SELECT source_path, provider, source_format, source_root, external_session_id, parent_external_session_id, agent_type, role_hint, external_agent_id, cwd, session_started_at_ms, file_size_bytes, file_modified_at_ms, cataloged_at_ms, metadata_json FROM catalog_sessions {tail}"
    )
}

pub(crate) fn source_import_file_select_sql(tail: &str) -> String {
    format!(
        "SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms, metadata_json FROM source_import_files {tail}"
    )
}
