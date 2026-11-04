use ctx_history_core::{StableEntityId, TypedKey};

use crate::Result;

/// Exact call-result term attached to the existing Core document, independent
/// of its native event-ID selector. The canonical session includes its source
/// ownership; the typed call ID retains its native type and exact bytes.
pub fn provider_call_result_key(
    session_id: StableEntityId,
    call_id: &TypedKey,
) -> Result<Option<Vec<u8>>> {
    let mut key = session_id.encode_canonical()?.to_vec();
    serde_json::to_writer(&mut key, call_id)?;
    // Tantivy drops terms beyond its key limit. They cannot prove absence.
    Ok((key.len() <= tantivy::tokenizer::MAX_TOKEN_LEN).then_some(key))
}
