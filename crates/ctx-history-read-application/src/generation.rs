use ctx_history_index_query::VerifiedIndex;
use thiserror::Error;

/// Generation authority selected before an application read opens any records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerationReadTarget {
    Active,
    Exact(String),
}

/// Whether an operation needs the retained peer used by compact references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedPeerRead {
    Omit,
    IfAvailable,
    /// Retain the peer lease, but open its search reader only when the caller
    /// needs compact references after the active query completes.
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationReadRequest {
    pub target: GenerationReadTarget,
    pub retained_peer: RetainedPeerRead,
}

/// Indexes opened by an injected adapter. Concrete pointer and lifecycle
/// policy remains outside this crate.
pub struct GenerationRead {
    index: VerifiedIndex,
    retained_peer: Option<VerifiedIndex>,
}

impl GenerationRead {
    pub fn new(index: VerifiedIndex, retained_peer: Option<VerifiedIndex>) -> Self {
        Self {
            index,
            retained_peer,
        }
    }

    pub const fn index(&self) -> &VerifiedIndex {
        &self.index
    }

    pub const fn retained_peer(&self) -> Option<&VerifiedIndex> {
        self.retained_peer.as_ref()
    }
}

/// Static-dispatch port for opening one generation read.
pub trait GenerationReadPort {
    type Error;

    fn read_generation(
        &mut self,
        request: &GenerationReadRequest,
    ) -> Result<GenerationRead, Self::Error>;
}

impl<Read, Error> GenerationReadPort for Read
where
    Read: FnMut(&GenerationReadRequest) -> Result<GenerationRead, Error>,
{
    type Error = Error;

    fn read_generation(
        &mut self,
        request: &GenerationReadRequest,
    ) -> Result<GenerationRead, Self::Error> {
        self(request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationReadReceipt<'read> {
    pub target: &'read GenerationReadTarget,
    pub generation_id: &'read str,
    pub retained_generation_id: Option<&'read str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GenerationReadAuthorityError {
    #[error("requested generation {expected} does not match opened generation {actual}")]
    TargetMismatch { expected: String, actual: String },
    #[error("generation adapter returned a retained peer when none was requested")]
    UnexpectedRetainedPeer,
}

#[derive(Debug)]
pub enum GenerationReadError<PortError> {
    Port(PortError),
    Authority(GenerationReadAuthorityError),
}

pub(crate) struct PinnedGenerationRead {
    index: VerifiedIndex,
    retained_peer: Option<VerifiedIndex>,
    deferred_peer: bool,
    target: GenerationReadTarget,
}

impl PinnedGenerationRead {
    pub(crate) fn open<Port: GenerationReadPort>(
        port: &mut Port,
        request: GenerationReadRequest,
    ) -> Result<Self, GenerationReadError<Port::Error>> {
        let GenerationRead {
            index,
            retained_peer,
        } = port
            .read_generation(&request)
            .map_err(GenerationReadError::Port)?;
        if let GenerationReadTarget::Exact(expected) = &request.target {
            if index.generation_id() != expected {
                return Err(GenerationReadError::Authority(
                    GenerationReadAuthorityError::TargetMismatch {
                        expected: expected.clone(),
                        actual: index.generation_id().to_owned(),
                    },
                ));
            }
        }
        if request.retained_peer == RetainedPeerRead::Omit && retained_peer.is_some() {
            return Err(GenerationReadError::Authority(
                GenerationReadAuthorityError::UnexpectedRetainedPeer,
            ));
        }
        Ok(Self {
            index,
            retained_peer,
            deferred_peer: request.retained_peer == RetainedPeerRead::Deferred,
            target: request.target,
        })
    }

    pub(crate) const fn index(&self) -> &VerifiedIndex {
        &self.index
    }

    pub(crate) const fn retained_peer(&self) -> Option<&VerifiedIndex> {
        self.retained_peer.as_ref()
    }

    pub(crate) fn materialize_deferred_peer(&mut self) -> anyhow::Result<()> {
        if self.deferred_peer && self.retained_peer.is_none() {
            self.retained_peer = self.index.take_retained_generation_peer_for_reader()?;
            self.deferred_peer = false;
        }
        Ok(())
    }

    pub(crate) const fn is_deferred_without_peer(&self) -> bool {
        self.deferred_peer && self.retained_peer.is_none()
    }

    pub(crate) fn into_deferred_peer_for_projection(self) -> anyhow::Result<Option<VerifiedIndex>> {
        if !self.deferred_peer || self.retained_peer.is_some() {
            return Ok(self.retained_peer);
        }
        self.index
            .into_retained_generation_peer_for_reader()
            .map_err(Into::into)
    }

    pub(crate) fn receipt(&self) -> GenerationReadReceipt<'_> {
        GenerationReadReceipt {
            target: &self.target,
            generation_id: self.index.generation_id(),
            retained_generation_id: self
                .retained_peer
                .as_ref()
                .map(VerifiedIndex::generation_id),
        }
    }

    pub(crate) fn into_index(self) -> VerifiedIndex {
        self.index
    }
}
