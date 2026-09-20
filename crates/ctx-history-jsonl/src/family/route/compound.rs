use super::*;

/// One provider-owned, bounded observation of the other inputs to a JSONL leaf.
/// Only its digest is persisted; the observer retains task-local I/O authority.
pub(super) struct CompoundInput<E: JsonlFamilyError> {
    digest: [u8; 32],
    observe: Arc<dyn Fn() -> JsonlResult<[u8; 32], E> + Send + Sync>,
}

impl<E: JsonlFamilyError> Clone for CompoundInput<E> {
    fn clone(&self) -> Self {
        Self {
            digest: self.digest,
            observe: Arc::clone(&self.observe),
        }
    }
}

impl<E: JsonlFamilyError> std::fmt::Debug for CompoundInput<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompoundInput")
            .finish_non_exhaustive()
    }
}

impl<E: JsonlFamilyError> CompoundInput<E> {
    pub(super) fn revalidate(&self) -> JsonlResult<bool, E> {
        Ok((self.observe)()? == self.digest)
    }

    pub(super) fn hash_into(&self, digest: &mut Sha256) {
        digest.update(b"ctx-jsonl-compound-input-v1\0");
        digest.update(self.digest);
    }
}

impl<E: JsonlFamilyError> JsonlFamilyLeaf<E> {
    /// Binds a provider-owned compound input to this leaf's refresh lifecycle.
    ///
    /// `digest` must describe every additional input that can affect projected
    /// records, including missing inputs so their later repair is observable.
    /// The provider owns a deterministic, versioned, bounded observation using
    /// retained no-follow source capabilities, and must read bodies through the
    /// same authority. A directory mtime alone is not a sufficient observation.
    /// The observer must freshly observe that authority, not return a cached
    /// digest. Errors retain their ordinary source/system failure classification.
    ///
    /// A changed digest forces whole-source replacement before no-op or append
    /// admission. An unchanged digest permits ordinary prefix-certified append.
    /// The family persists the digest in its existing checkpoint binding and
    /// checks it on both sides of the primary file's terminal validation. No
    /// artifact bodies, reference lists, or callbacks enter the checkpoint.
    ///
    /// Compound leaves use the shared projector/semantic executor path, not the
    /// legacy optimized scanner. The provider must ensure its route watch and
    /// discovery cover these inputs; this does not register additional roots.
    pub fn with_compound_input(
        mut self,
        digest: [u8; 32],
        observe: impl Fn() -> JsonlResult<[u8; 32], E> + Send + Sync + 'static,
    ) -> JsonlResult<Self, E> {
        if self.terminal_dependencies.compound.is_some() {
            return Err(E::invalid_payload(
                "JSONL leaf already has a compound input binding".to_owned(),
            ));
        }
        let input = CompoundInput {
            digest,
            observe: Arc::new(observe),
        };
        if !input.revalidate()? {
            return Err(E::source_changed());
        }
        self.terminal_dependencies.compound = Some(input);
        Ok(self)
    }
}
