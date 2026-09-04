use std::path::Path;

use anyhow::Result;
use ctx_history_index::VerifiedIndex;
use ctx_history_read_application::{
    GenerationRead, GenerationReadRequest, GenerationReadTarget, RetainedPeerRead,
};

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
        GenerationReadTarget::Active => match request.retained_peer {
            RetainedPeerRead::Omit => super::shared::open_index(data_root)?,
            RetainedPeerRead::IfAvailable => {
                super::shared::open_index_with_retained_peer(data_root)?
            }
        },
        GenerationReadTarget::Exact(generation_id) => match request.retained_peer {
            RetainedPeerRead::Omit => VerifiedIndex::open_pinned_generation(&root, generation_id)?,
            RetainedPeerRead::IfAvailable => {
                VerifiedIndex::open_pinned_generation_with_retained_peer(&root, generation_id)?
            }
        },
    };
    generation_read(index, request)
}

fn open_retained_peer(current: &mut VerifiedIndex) -> Result<Option<VerifiedIndex>> {
    let retained_peer = current.take_retained_generation_peer_for_reader()?;
    Ok(retained_peer)
}
