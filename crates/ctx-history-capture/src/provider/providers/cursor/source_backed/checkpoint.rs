use super::*;
use crate::provider::source_backed::family::jsonl::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
};

pub(super) const MAX_CURSOR_CHECKPOINT_BYTES: usize = 40 * 1024;
const CURSOR_CHECKPOINT_VERSION: u32 = 2;
const CURSOR_CHECKPOINT_PREFIX: &str = "cursor.projector-checkpoint.v2:";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorCheckpoint {
    version: u32,
    native_session_id: String,
    tool_contexts: Vec<(String, CursorToolContextState)>,
    linkage_capacity_exceeded: bool,
}

#[derive(Debug)]
pub(super) struct RestoredCursorCheckpoint {
    pub(super) tool_contexts: BTreeMap<String, CursorToolContextState>,
    pub(super) linkage_capacity_exceeded: bool,
}

fn checkpoint_value(projector: &CursorProjector) -> CursorCheckpoint {
    CursorCheckpoint {
        version: CURSOR_CHECKPOINT_VERSION,
        native_session_id: projector.native_session_id.clone(),
        tool_contexts: projector
            .tool_contexts
            .iter()
            .map(|(call_id, state)| (call_id.clone(), state.clone()))
            .collect(),
        linkage_capacity_exceeded: projector.linkage_capacity_exceeded,
    }
}

pub(super) fn cursor_checkpoint_fits(projector: &CursorProjector) -> bool {
    bounded_checkpoint_fits(&checkpoint_value(projector), MAX_CURSOR_CHECKPOINT_BYTES)
}

pub(super) fn encode_cursor_checkpoint(projector: &CursorProjector) -> Result<TypedKey> {
    encode_bounded_checkpoint(
        CURSOR_CHECKPOINT_PREFIX,
        &checkpoint_value(projector),
        MAX_CURSOR_CHECKPOINT_BYTES,
        "Cursor",
    )
}

pub(super) fn decode_cursor_checkpoint(
    checkpoint: &TypedKey,
    native_session_id: &str,
) -> Result<RestoredCursorCheckpoint> {
    let checkpoint: CursorCheckpoint = decode_bounded_checkpoint(
        checkpoint,
        CURSOR_CHECKPOINT_PREFIX,
        MAX_CURSOR_CHECKPOINT_BYTES,
        "Cursor",
    )?;
    if checkpoint.version != CURSOR_CHECKPOINT_VERSION
        || checkpoint.native_session_id != native_session_id
        || checkpoint.tool_contexts.len() > MAX_CURSOR_TOOL_CONTEXTS
    {
        return Err(CaptureError::InvalidPayload(
            "Cursor projector checkpoint does not match its source binding or capacity".to_owned(),
        ));
    }
    let mut tool_contexts = BTreeMap::new();
    for (call_id, state) in checkpoint.tool_contexts {
        if call_id.is_empty() || tool_contexts.insert(call_id, state).is_some() {
            return Err(CaptureError::InvalidPayload(
                "Cursor projector checkpoint repeats a call identity".to_owned(),
            ));
        }
    }
    Ok(RestoredCursorCheckpoint {
        tool_contexts,
        linkage_capacity_exceeded: checkpoint.linkage_capacity_exceeded,
    })
}
