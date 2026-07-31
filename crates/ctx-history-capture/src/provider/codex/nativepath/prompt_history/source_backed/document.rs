use ctx_history_index::LexicalDocument;

pub(super) fn retained_document_bytes(document: &LexicalDocument) -> usize {
    document
        .body
        .len()
        .saturating_add(document.provider_session_id.as_ref().map_or(0, String::len))
        .saturating_add(document.source_path.as_ref().map_or(0, String::len))
        .saturating_add(512)
}
