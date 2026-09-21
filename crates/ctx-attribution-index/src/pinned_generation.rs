//! Physical Flat storage pinned to one immutable attribution generation.

use std::path::Path;
use std::sync::Mutex;

use thiserror::Error;

use super::flat::{FlatQueryContinuation, FlatQueryPage};
use super::model::EventOwnerKey;
use super::{
    FLAT_SERVING_ROLE, FactFamily, FlatSegmentError, FlatSegmentReader, SegmentFile,
    SegmentManifest, SegmentRef, SegmentStore, SegmentStoreError, ServingRecord,
    TombstoneQueryWork,
};
use ctx_attribution_model::CoreMaterializationReceipt;

const MAX_GRAPH_FLAT_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PINNED_FLAT_LAYERS: usize = 16;
const MAX_ACTIVE_OPEN_ATTEMPTS: usize = 4;

/// Physical identities and bounds required by one Flat serving consumer.
#[derive(Clone, Copy)]
pub struct FlatOpenPolicy {
    schema: &'static str,
    evidence: &'static str,
    ordering: &'static str,
}

impl FlatOpenPolicy {
    #[must_use]
    pub const fn new(
        schema_identity: &'static str,
        evidence_identity: &'static str,
        ordering_identity: &'static str,
    ) -> Self {
        Self {
            schema: schema_identity,
            evidence: evidence_identity,
            ordering: ordering_identity,
        }
    }

    fn validate(self, manifest: &SegmentManifest) -> Result<(), PinnedFlatGenerationError> {
        manifest
            .validate()
            .map_err(|_| PinnedFlatGenerationError::Corrupt("segment manifest ordering"))?;
        if manifest.schema_identity != self.schema {
            return Err(PinnedFlatGenerationError::Identity("schema"));
        }
        if manifest.evidence_identity != self.evidence {
            return Err(PinnedFlatGenerationError::Identity("evidence"));
        }
        if manifest.ordering_identity != self.ordering {
            return Err(PinnedFlatGenerationError::Identity("ordering"));
        }
        let mut prior_publication_generation = None;
        let mut layers = 0_usize;
        for segment in manifest
            .segments
            .iter()
            .filter(|segment| segment.role == FLAT_SERVING_ROLE)
        {
            if prior_publication_generation != Some(segment.publication_generation) {
                layers = layers
                    .checked_add(1)
                    .ok_or(PinnedFlatGenerationError::Corrupt("Flat layer bound"))?;
                if layers > MAX_PINNED_FLAT_LAYERS {
                    return Err(PinnedFlatGenerationError::Corrupt("Flat layer bound"));
                }
                prior_publication_generation = Some(segment.publication_generation);
            }
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn validate_manifest_for_test(
        self,
        manifest: &SegmentManifest,
    ) -> Result<(), PinnedFlatGenerationError> {
        self.validate(manifest)
    }
}

/// Selects an active generation from the caller-owned attribution root.
pub struct FlatStore<'a> {
    root: &'a Path,
}

impl<'a> FlatStore<'a> {
    #[must_use]
    pub const fn new(root: &'a Path) -> Self {
        Self { root }
    }

    /// Pins exactly one checksummed active manifest generation and all of
    /// its Flat file descriptors. Payload ranges remain lazy.
    pub fn open_active(
        self,
        policy: FlatOpenPolicy,
    ) -> Result<PinnedFlatGeneration, PinnedFlatGenerationError> {
        self.open_active_inner(policy, || {})
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_active_after_selection_for_test(
        self,
        policy: FlatOpenPolicy,
        after_selection: impl FnOnce(),
    ) -> Result<PinnedFlatGeneration, PinnedFlatGenerationError> {
        self.open_active_inner(policy, after_selection)
    }

    fn open_active_inner(
        self,
        policy: FlatOpenPolicy,
        after_selection: impl FnOnce(),
    ) -> Result<PinnedFlatGeneration, PinnedFlatGenerationError> {
        let store = SegmentStore::new(self.root);
        let mut after_selection = Some(after_selection);
        for _attempt in 0..MAX_ACTIVE_OPEN_ATTEMPTS {
            // This remains the only active-manifest selection in the normal
            // serving path. Identity-only publication races retry from the
            // active name; checksum and format failures never retry.
            let manifest = match store.load_active() {
                Ok(Some(manifest)) => manifest,
                Ok(None) => return Err(PinnedFlatGenerationError::Unavailable),
                Err(error) if is_active_selection_change(&error) => continue,
                Err(error) => return Err(error.into()),
            };
            policy.validate(&manifest)?;
            if let Some(after_selection) = after_selection.take() {
                after_selection();
            }

            match pin_flat_layers(self.root, &manifest) {
                Ok(layers) => {
                    return Ok(PinnedFlatGeneration {
                        layers: Mutex::new(layers),
                        graph_generation: manifest.graph_generation,
                        completed_receipt: manifest.core_receipt,
                        generation_id: manifest.generation_id,
                    });
                }
                Err(error) if error.is_segment_selection_change() => {
                    // A selected generation is retained as the active
                    // manifest's exact predecessor. If enough publications
                    // race to retire it before every descriptor is pinned,
                    // restart only when the checksummed active generation
                    // actually advanced.
                    match store.load_active() {
                        Ok(Some(active)) if active.generation_id != manifest.generation_id => {}
                        Err(active_error) if is_active_selection_change(&active_error) => {}
                        Err(active_error) => return Err(active_error.into()),
                        Ok(_) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(PinnedFlatGenerationError::Corrupt(
            "active Flat generation changed repeatedly while opening",
        ))
    }
}

fn pin_flat_layers(
    root: &Path,
    manifest: &SegmentManifest,
) -> Result<Vec<FlatSegmentLayer>, PinnedFlatGenerationError> {
    let mut layers = Vec::<FlatSegmentLayer>::new();
    let mut flat_directory_bytes = 0_u64;
    for segment in manifest
        .segments
        .iter()
        .filter(|segment| segment.role == FLAT_SERVING_ROLE)
    {
        let starts_new_layer = layers
            .last()
            .is_none_or(|layer| layer.publication_generation != segment.publication_generation);
        if starts_new_layer && layers.len() >= MAX_PINNED_FLAT_LAYERS {
            return Err(PinnedFlatGenerationError::Corrupt("Flat layer bound"));
        }
        let remaining_directory_bytes = MAX_GRAPH_FLAT_DIRECTORY_BYTES
            .checked_sub(flat_directory_bytes)
            .ok_or(PinnedFlatGenerationError::Corrupt("Flat directory budget"))?;
        let reader = open_flat(root, segment, remaining_directory_bytes)?;
        flat_directory_bytes = flat_directory_bytes
            .checked_add(reader.directory_bytes())
            .ok_or(PinnedFlatGenerationError::Corrupt("Flat directory budget"))?;
        if starts_new_layer {
            layers.push(FlatSegmentLayer {
                publication_generation: segment.publication_generation,
                readers: vec![reader],
            });
        } else if let Some(layer) = layers.last_mut() {
            layer.readers.push(reader);
        }
    }
    Ok(layers)
}

/// Owns the pinned file descriptors and receipt of one immutable generation.
pub struct PinnedFlatGeneration {
    layers: Mutex<Vec<FlatSegmentLayer>>,
    graph_generation: u64,
    completed_receipt: CoreMaterializationReceipt,
    generation_id: String,
}

struct FlatSegmentLayer {
    // Retained to make same-publication grouping an explicit physical-open
    // invariant, even though traversal needs only the immutable vector order.
    publication_generation: u64,
    readers: Vec<FlatSegmentReader>,
}

/// Opaque location within the immutable physical reader ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnedFlatPosition {
    layer_index: usize,
    reader_index: usize,
}

/// Opaque continuation for one bounded physical Flat lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedFlatContinuation(FlatQueryContinuation);

/// Checked decoded records from one bounded physical Flat lookup.
pub struct PinnedFlatPage {
    pub records: Vec<ServingRecord>,
    pub continuation: Option<PinnedFlatContinuation>,
}

impl PinnedFlatGeneration {
    pub const fn graph_generation(&self) -> u64 {
        self.graph_generation
    }

    pub const fn completed_receipt(&self) -> &CoreMaterializationReceipt {
        &self.completed_receipt
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn first_reader(&self) -> Result<Option<PinnedFlatPosition>, PinnedFlatGenerationError> {
        let layers = self.lock_layers()?;
        Ok(first_position(&layers))
    }

    pub fn next_reader(
        &self,
        position: PinnedFlatPosition,
    ) -> Result<Option<PinnedFlatPosition>, PinnedFlatGenerationError> {
        let layers = self.lock_layers()?;
        validate_position(&layers, position)?;
        Ok(next_position(&layers, position, layers.len()))
    }

    pub fn scan_strictly_newer_tombstones<E>(
        &self,
        position: PinnedFlatPosition,
        owner_keys: &[EventOwnerKey],
        mut charge_before_membership: impl FnMut(TombstoneQueryWork) -> Result<(), E>,
        mut observe_membership: impl FnMut(Vec<bool>) -> Result<bool, E>,
    ) -> Result<Result<(), E>, PinnedFlatGenerationError> {
        let mut layers = self.lock_layers()?;
        validate_position(&layers, position)?;
        for layer in layers.iter_mut().take(position.layer_index) {
            for reader in &mut layer.readers {
                let planned = reader.tombstone_query_work(owner_keys)?;
                if let Err(error) = charge_before_membership(planned) {
                    return Ok(Err(error));
                }
                let membership = reader.tombstone_membership(owner_keys)?;
                match observe_membership(membership) {
                    Ok(true) => return Ok(Ok(())),
                    Ok(false) => {}
                    Err(error) => return Ok(Err(error)),
                }
            }
        }
        Ok(Ok(()))
    }

    pub fn query_exact_page(
        &self,
        position: PinnedFlatPosition,
        repository: &str,
        family: &FactFamily,
        term: &str,
        continuation: Option<&PinnedFlatContinuation>,
    ) -> Result<PinnedFlatPage, PinnedFlatGenerationError> {
        self.query_page(position, continuation, |reader, continuation| {
            reader.query_exact_page(
                repository,
                family,
                term,
                continuation,
                super::MAX_QUERY_RESULTS,
            )
        })
    }

    pub fn query_exact_unscoped_page(
        &self,
        position: PinnedFlatPosition,
        family: &FactFamily,
        term: &str,
        continuation: Option<&PinnedFlatContinuation>,
    ) -> Result<PinnedFlatPage, PinnedFlatGenerationError> {
        self.query_page(position, continuation, |reader, continuation| {
            reader.query_exact_unscoped_page(family, term, continuation, super::MAX_QUERY_RESULTS)
        })
    }

    pub fn query_prefix_page(
        &self,
        position: PinnedFlatPosition,
        repository: &str,
        family: &FactFamily,
        term: &str,
        continuation: Option<&PinnedFlatContinuation>,
    ) -> Result<PinnedFlatPage, PinnedFlatGenerationError> {
        self.query_page(position, continuation, |reader, continuation| {
            reader.query_prefix_page(
                repository,
                family,
                term,
                continuation,
                super::MAX_QUERY_RESULTS,
            )
        })
    }

    pub fn query_prefix_unscoped_page(
        &self,
        position: PinnedFlatPosition,
        family: &FactFamily,
        term: &str,
        continuation: Option<&PinnedFlatContinuation>,
    ) -> Result<PinnedFlatPage, PinnedFlatGenerationError> {
        self.query_page(position, continuation, |reader, continuation| {
            reader.query_prefix_unscoped_page(family, term, continuation, super::MAX_QUERY_RESULTS)
        })
    }

    fn query_page(
        &self,
        position: PinnedFlatPosition,
        continuation: Option<&PinnedFlatContinuation>,
        query: impl FnOnce(
            &mut FlatSegmentReader,
            Option<&FlatQueryContinuation>,
        ) -> Result<FlatQueryPage, FlatSegmentError>,
    ) -> Result<PinnedFlatPage, PinnedFlatGenerationError> {
        let mut layers = self.lock_layers()?;
        let reader = reader_at_mut(&mut layers, position)?;
        let page = query(reader, continuation.map(|continuation| &continuation.0))?;
        Ok(PinnedFlatPage {
            records: page.records,
            continuation: page.continuation.map(PinnedFlatContinuation),
        })
    }

    fn lock_layers(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Vec<FlatSegmentLayer>>, PinnedFlatGenerationError> {
        self.layers
            .lock()
            .map_err(|_| PinnedFlatGenerationError::Corrupt("pinned Flat reader lock"))
    }
}

fn first_position(layers: &[FlatSegmentLayer]) -> Option<PinnedFlatPosition> {
    layers.first().map(|_| PinnedFlatPosition {
        layer_index: 0,
        reader_index: 0,
    })
}

fn next_position(
    layers: &[FlatSegmentLayer],
    position: PinnedFlatPosition,
    layer_limit: usize,
) -> Option<PinnedFlatPosition> {
    let layer = layers.get(position.layer_index)?;
    if position.reader_index + 1 < layer.readers.len() {
        return Some(PinnedFlatPosition {
            layer_index: position.layer_index,
            reader_index: position.reader_index + 1,
        });
    }
    let next_layer = position.layer_index + 1;
    (next_layer < layer_limit).then_some(PinnedFlatPosition {
        layer_index: next_layer,
        reader_index: 0,
    })
}

fn validate_position(
    layers: &[FlatSegmentLayer],
    position: PinnedFlatPosition,
) -> Result<(), PinnedFlatGenerationError> {
    layers
        .get(position.layer_index)
        .and_then(|layer| layer.readers.get(position.reader_index))
        .map(|_| ())
        .ok_or(PinnedFlatGenerationError::Corrupt(
            "pinned Flat reader position",
        ))
}

fn reader_at_mut(
    layers: &mut [FlatSegmentLayer],
    position: PinnedFlatPosition,
) -> Result<&mut FlatSegmentReader, PinnedFlatGenerationError> {
    layers
        .get_mut(position.layer_index)
        .and_then(|layer| layer.readers.get_mut(position.reader_index))
        .ok_or(PinnedFlatGenerationError::Corrupt(
            "pinned Flat reader position",
        ))
}

fn open_flat(
    root: &Path,
    segment: &SegmentRef,
    directory_budget: u64,
) -> Result<FlatSegmentReader, PinnedFlatGenerationError> {
    let path = root.join(&segment.file_name);
    let generation = decode_generation(&segment.generation_id)?;
    let file = SegmentFile::open(&path, generation, segment.role).map_err(|error| match error {
        crate::SegmentFileError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            PinnedFlatGenerationError::SegmentIdentityChanged
        }
        error => PinnedFlatGenerationError::Flat(FlatSegmentError::from(error)),
    })?;
    if file.plaintext_len() != segment.plaintext_bytes {
        return Err(PinnedFlatGenerationError::Corrupt(
            "segment plaintext length",
        ));
    }
    Ok(FlatSegmentReader::open_bounded(file, directory_budget)?)
}

fn is_active_selection_change(error: &SegmentStoreError) -> bool {
    matches!(error, SegmentStoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
}

fn decode_generation(value: &str) -> Result<[u8; 32], PinnedFlatGenerationError> {
    let bytes =
        hex::decode(value).map_err(|_| PinnedFlatGenerationError::Corrupt("segment generation"))?;
    bytes
        .try_into()
        .map_err(|_| PinnedFlatGenerationError::Corrupt("segment generation"))
}

#[derive(Debug, Error)]
pub enum PinnedFlatGenerationError {
    #[error("the active graph segment manifest is unavailable")]
    Unavailable,
    #[error("the active graph segment manifest has an incompatible {0} identity")]
    Identity(&'static str),
    #[error("the active graph segment set is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("a Flat segment identity changed while pinning the active generation")]
    SegmentIdentityChanged,
    #[error(transparent)]
    Store(#[from] SegmentStoreError),
    #[error(transparent)]
    Flat(#[from] FlatSegmentError),
    #[error("graph segment verification failed")]
    Verification(#[source] std::io::Error),
}

impl PinnedFlatGenerationError {
    fn is_segment_selection_change(&self) -> bool {
        matches!(self, Self::SegmentIdentityChanged)
    }
}
