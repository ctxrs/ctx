use super::*;
use crate::provider::source_backed::family::jsonl::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
};

pub(super) const MAX_PROJECTOR_CHECKPOINT_BYTES: usize = 40 * 1024;
const PROJECTOR_CHECKPOINT_VERSION: u32 = 1;
const PROJECTOR_CHECKPOINT_PREFIX: &str = "claude.projector-checkpoint.v1:";

pub(super) struct RestoredProjectorCheckpoint {
    pub(super) session: ClaudeSessionMetadata,
    pub(super) pending_calls: HashMap<String, PendingCallState>,
    pub(super) linkage_capacity_exceeded: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectorCheckpoint {
    version: u32,
    session: ClaudeSessionMetadata,
    pending_calls: Vec<(String, PendingCallState)>,
    linkage_capacity_exceeded: bool,
}

fn checkpoint_value(projector: &ClaudeProjector) -> ProjectorCheckpoint {
    let mut pending_calls = projector
        .pending_calls
        .iter()
        .map(|(call_id, state)| (call_id.clone(), state.clone()))
        .collect::<Vec<_>>();
    pending_calls.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    ProjectorCheckpoint {
        version: PROJECTOR_CHECKPOINT_VERSION,
        session: projector.session.clone(),
        pending_calls,
        linkage_capacity_exceeded: projector.linkage_capacity_exceeded,
    }
}

pub(super) fn projector_checkpoint_fits(projector: &ClaudeProjector) -> bool {
    bounded_checkpoint_fits(&checkpoint_value(projector), MAX_PROJECTOR_CHECKPOINT_BYTES)
}

pub(super) fn encode_projector_checkpoint(projector: &ClaudeProjector) -> Result<TypedKey> {
    encode_bounded_checkpoint(
        PROJECTOR_CHECKPOINT_PREFIX,
        &checkpoint_value(projector),
        MAX_PROJECTOR_CHECKPOINT_BYTES,
        "Claude",
    )
}

pub(super) fn decode_projector_checkpoint(
    checkpoint: &TypedKey,
    binding: &Binding,
) -> Result<RestoredProjectorCheckpoint> {
    let checkpoint: ProjectorCheckpoint = decode_bounded_checkpoint(
        checkpoint,
        PROJECTOR_CHECKPOINT_PREFIX,
        MAX_PROJECTOR_CHECKPOINT_BYTES,
        "Claude",
    )?;
    if checkpoint.version != PROJECTOR_CHECKPOINT_VERSION || checkpoint.session.key != binding.key {
        return Err(CaptureError::InvalidPayload(
            "Claude projector checkpoint does not match its source binding".to_owned(),
        ));
    }
    if checkpoint.pending_calls.len() > MAX_PENDING_CALLS {
        return Err(CaptureError::InvalidPayload(
            "Claude projector checkpoint exceeds its state capacity".to_owned(),
        ));
    }
    let mut pending_calls = HashMap::with_capacity(checkpoint.pending_calls.len());
    for (call_id, state) in checkpoint.pending_calls {
        if call_id.is_empty() || pending_calls.insert(call_id, state).is_some() {
            return Err(CaptureError::InvalidPayload(
                "Claude projector checkpoint repeats a call identity".to_owned(),
            ));
        }
    }
    Ok(RestoredProjectorCheckpoint {
        session: checkpoint.session,
        pending_calls,
        linkage_capacity_exceeded: checkpoint.linkage_capacity_exceeded,
    })
}
