use std::path::Path;

use anyhow::Result;
use ctx_history_index::VerifiedIndex;
use ctx_history_read_application::{
    GenerationRead, GenerationReadRequest, GenerationReadTarget, RetainedPeerRead,
};
use ctx_history_refresh::verify_generation_query_authority;

pub(super) fn generation_read(
    mut index: VerifiedIndex,
    request: &GenerationReadRequest,
) -> Result<GenerationRead> {
    let retained_peer = match request.retained_peer {
        RetainedPeerRead::Omit => None,
        RetainedPeerRead::IfAvailable => open_retained_peer(&mut index)?,
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
    generation_read(index, request)
}

fn open_retained_peer(current: &mut VerifiedIndex) -> Result<Option<VerifiedIndex>> {
    let retained_peer = current.take_retained_generation_peer_for_reader()?;
    if let Some(peer) = retained_peer.as_ref() {
        verify_generation_query_authority(peer).map_err(anyhow::Error::new)?;
    }
    Ok(retained_peer)
}
