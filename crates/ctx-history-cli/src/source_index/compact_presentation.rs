use std::path::Path;

use anyhow::Result;
use ctx_history_index::{IndexError, VerifiedIndex};
use ctx_history_read_application::{
    GenerationRead, GenerationReadRequest, GenerationReadTarget, RetainedPeerRead,
};
use ctx_history_refresh::verify_generation_query_authority;

pub(super) fn generation_read(
    index: VerifiedIndex,
    index_root: &Path,
    request: &GenerationReadRequest,
) -> Result<GenerationRead> {
    let retained_peer = match request.retained_peer {
        RetainedPeerRead::Omit => None,
        RetainedPeerRead::IfAvailable => open_retained_peer(&index, index_root)?,
    };
    Ok(GenerationRead::new(index, retained_peer))
}

pub(crate) fn open_generation_read(
    data_root: &Path,
    request: &GenerationReadRequest,
) -> Result<GenerationRead> {
    let root = super::shared::index_root(data_root);
    let index = match &request.target {
        GenerationReadTarget::Active => super::shared::open_index(data_root)?,
        GenerationReadTarget::Exact(generation_id) => {
            let index = VerifiedIndex::open_pinned_generation(&root, generation_id)?;
            verify_generation_query_authority(&index).map_err(anyhow::Error::new)?;
            index
        }
    };
    generation_read(index, &root, request)
}

fn open_retained_peer(current: &VerifiedIndex, index_root: &Path) -> Result<Option<VerifiedIndex>> {
    let retained_peer = current
        .open_retained_generation_peer_for_reader(index_root)
        .map_err(|error| match error {
            IndexError::PinnedGenerationNotRetained { .. } => {
                IndexError::ConcurrentGenerationChange
            }
            error => error,
        })?;
    if let Some(peer) = retained_peer.as_ref() {
        verify_generation_query_authority(peer).map_err(anyhow::Error::new)?;
    }
    Ok(retained_peer)
}
