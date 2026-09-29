//! Ordered source metadata with fixed-width on-disk lookup rows.
//! Lookup keys are bounded independently of inventory length.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;
use std::path::Path;

use ctx_attribution_model::{CoreSourceSnapshot, CoreSourceSnapshotBuilder};

use super::{MAX_METADATA_SEGMENT_ENTRIES, MaterializationIndexError, runtime_file::RuntimeFile};

use sha2::{Digest, Sha256};

use ctx_attribution_model::CoreSourceState;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[expect(
    clippy::large_enum_variant,
    reason = "Source deltas carry the existing Core state by value; boxing every upsert would add an allocation to materialization."
)]
pub enum SourceMutation {
    Upsert {
        state: CoreSourceState,
        materializer_revision: String,
    },
    Removed {
        source_id: String,
    },
}

impl SourceMutation {
    pub fn validate(&self) -> Result<(), MaterializationIndexError> {
        validate_source_mutation(self)
    }

    pub fn source_id(&self) -> String {
        match self {
            Self::Upsert { state, .. } => crate::core_source_storage_id(&state.source),
            Self::Removed { source_id } => source_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSource {
    pub state: CoreSourceState,
    pub materializer_revision: String,
}

const ROW_BYTES: usize = 76;
const MAX_LOOKUP_KEYS: usize = 1024;

#[cfg(test)]
thread_local! {
    static ROW_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub struct SourceInventory {
    rows: RuntimeFile,
    values: RuntimeFile,
    count: usize,
    lookup_keys: Vec<[u8; 32]>,
    lookup_stride: usize,
    value_bytes: u64,
    snapshot: CoreSourceSnapshotBuilder,
}

impl SourceInventory {
    pub fn new(root: &Path) -> Result<Self, MaterializationIndexError> {
        Ok(Self {
            rows: RuntimeFile::new(root)?,
            values: RuntimeFile::new(root)?,
            count: 0,
            lookup_keys: Vec::new(),
            lookup_stride: 1,
            value_bytes: 0,
            snapshot: CoreSourceSnapshotBuilder::default(),
        })
    }

    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn push(&mut self, source: &ActiveSource) -> Result<(), MaterializationIndexError> {
        let identity = source.state.source.identity().digest();
        self.snapshot
            .push(&source.state)
            .map_err(|_| MaterializationIndexError::Conflict)?;
        self.append_mutation(
            identity,
            &SourceMutation::Upsert {
                state: source.state.clone(),
                materializer_revision: source.materializer_revision.clone(),
            },
        )
    }

    fn append_mutation(
        &mut self,
        identity: [u8; 32],
        mutation: &SourceMutation,
    ) -> Result<(), MaterializationIndexError> {
        let encoded =
            serde_json::to_vec(mutation).map_err(|_| MaterializationIndexError::Encoding)?;
        if encoded.len() > ctx_attribution_model::MAX_CORE_CONTROL_WIRE_BYTES {
            return Err(MaterializationIndexError::Bounds);
        }
        let length = u32::try_from(encoded.len()).map_err(|_| MaterializationIndexError::Bounds)?;
        let mut row = [0; ROW_BYTES];
        row[..32].copy_from_slice(&identity);
        row[32..40].copy_from_slice(&self.value_bytes.to_le_bytes());
        row[40..44].copy_from_slice(&length.to_le_bytes());
        row[44..].copy_from_slice(&Sha256::digest(&encoded));
        let offset = self.values.append(&encoded)?;
        if offset != self.value_bytes {
            return Err(MaterializationIndexError::Conflict);
        }
        self.rows.append(&row)?;
        self.value_bytes = offset
            .checked_add(u64::from(length))
            .ok_or(MaterializationIndexError::Bounds)?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or(MaterializationIndexError::Bounds)?;
        let ordinal = self.count - 1;
        if ordinal.is_multiple_of(self.lookup_stride) {
            if self.lookup_keys.len() == MAX_LOOKUP_KEYS {
                // Keep at most 32 KiB of evenly spaced keys, even for very
                // large histories. These only narrow the disk search; the
                // selected row and its checksummed value are still read.
                let mut index = 0;
                self.lookup_keys.retain(|_| {
                    let keep = index % 2 == 0;
                    index += 1;
                    keep
                });
                self.lookup_stride *= 2;
            }
            self.lookup_keys.push(identity);
        }
        Ok(())
    }

    /// Exact source change selection for prior-event reads. Descriptor changes
    /// matter even when stable source identity and event totals are unchanged.
    pub fn changed_from(
        &self,
        prior: &Self,
        storage_id: &str,
    ) -> Result<bool, MaterializationIndexError> {
        match (self.get(storage_id)?, prior.get(storage_id)?) {
            (Some(next), Some(old)) => Ok(next.materializer_revision != old.materializer_revision
                || next.state.event_count != old.state.event_count
                || next.state.core_record_accumulator != old.state.core_record_accumulator
                || !next.state.source.exact_descriptor_eq(&old.state.source)),
            (None, None) => Ok(false),
            _ => Ok(true),
        }
    }

    pub fn snapshot(&self) -> CoreSourceSnapshot {
        self.snapshot.clone().finish()
    }

    pub fn get(&self, storage_id: &str) -> Result<Option<ActiveSource>, MaterializationIndexError> {
        let digest = storage_id
            .strip_prefix("core_source_")
            .ok_or(MaterializationIndexError::Conflict)?;
        let mut identity = [0; 32];
        hex::decode_to_slice(digest, &mut identity)
            .map_err(|_| MaterializationIndexError::Conflict)?;
        self.get_identity(identity)
    }

    pub fn get_identity(
        &self,
        identity: [u8; 32],
    ) -> Result<Option<ActiveSource>, MaterializationIndexError> {
        let (mut low, mut high) = match self.lookup_keys.binary_search(&identity) {
            Ok(index) => {
                let ordinal = index * self.lookup_stride;
                (ordinal, ordinal + 1)
            }
            Err(index) => (
                if index == 0 {
                    0
                } else {
                    (index - 1) * self.lookup_stride + 1
                },
                if index == self.lookup_keys.len() {
                    self.count
                } else {
                    index * self.lookup_stride
                },
            ),
        };
        while low < high {
            let mid = low + (high - low) / 2;
            let row = self.row(mid)?;
            match row[..32].cmp(&identity) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return self.decode(&row).map(Some),
            }
        }
        Ok(None)
    }

    pub fn at(&self, ordinal: usize) -> Result<ActiveSource, MaterializationIndexError> {
        self.decode(&self.row(ordinal)?)
    }

    /// Chooses a metadata page by both item count and exact encoded bytes.
    pub fn metadata_page_end(
        &self,
        start: usize,
        maximum: usize,
    ) -> Result<usize, MaterializationIndexError> {
        let mut end = start;
        let mut bytes = 0_usize;
        while end < self.count && end - start < MAX_METADATA_SEGMENT_ENTRIES {
            let row = self.row(end)?;
            let length = u32::from_le_bytes(
                row[40..44]
                    .try_into()
                    .map_err(|_| MaterializationIndexError::Encoding)?,
            ) as usize;
            let next = bytes
                .checked_add(length)
                .and_then(|n| n.checked_add(usize::from(end > start)))
                .ok_or(MaterializationIndexError::Bounds)?;
            if next > maximum {
                break;
            }
            bytes = next;
            end += 1;
        }
        if end == start && start < self.count {
            return Err(MaterializationIndexError::Bounds);
        }
        Ok(end)
    }

    pub fn metadata_segment_count(
        &self,
        maximum: usize,
    ) -> Result<usize, MaterializationIndexError> {
        let mut end = 0;
        let mut count = 0;
        while end < self.count {
            end = self.metadata_page_end(end, maximum)?;
            count += 1;
        }
        Ok(count.max(1))
    }

    fn row(&self, ordinal: usize) -> Result<[u8; ROW_BYTES], MaterializationIndexError> {
        if ordinal >= self.count {
            return Err(MaterializationIndexError::Conflict);
        }
        let offset = (ordinal as u64)
            .checked_mul(ROW_BYTES as u64)
            .ok_or(MaterializationIndexError::Bounds)?;
        let mut row = [0; ROW_BYTES];
        self.rows.read_at(offset, &mut row)?;
        #[cfg(test)]
        ROW_READS.with(|reads| reads.set(reads.get() + 1));
        Ok(row)
    }

    fn decode(&self, row: &[u8; ROW_BYTES]) -> Result<ActiveSource, MaterializationIndexError> {
        match self.decode_mutation(row)? {
            SourceMutation::Upsert {
                state,
                materializer_revision,
            } => Ok(ActiveSource {
                state,
                materializer_revision,
            }),
            SourceMutation::Removed { .. } => Err(MaterializationIndexError::Corrupt(
                "source inventory contains removal",
            )),
        }
    }

    fn decode_mutation(
        &self,
        row: &[u8; ROW_BYTES],
    ) -> Result<SourceMutation, MaterializationIndexError> {
        let offset = u64::from_le_bytes(
            row[32..40]
                .try_into()
                .map_err(|_| MaterializationIndexError::Encoding)?,
        );
        let length = u32::from_le_bytes(
            row[40..44]
                .try_into()
                .map_err(|_| MaterializationIndexError::Encoding)?,
        ) as usize;
        if length > ctx_attribution_model::MAX_CORE_CONTROL_WIRE_BYTES
            || offset
                .checked_add(length as u64)
                .is_none_or(|end| end > self.value_bytes)
        {
            return Err(MaterializationIndexError::Corrupt(
                "source inventory frame length",
            ));
        }
        let file_length = self.values.byte_len()?;
        if offset
            .checked_add(length as u64)
            .is_none_or(|end| end > file_length)
        {
            return Err(MaterializationIndexError::Corrupt(
                "truncated source inventory",
            ));
        }
        let mut bytes = vec![0; length];
        self.values.read_at(offset, &mut bytes)?;
        if Sha256::digest(&bytes).as_slice() != &row[44..] {
            return Err(MaterializationIndexError::Corrupt(
                "source inventory checksum",
            ));
        }
        let mutation: SourceMutation =
            serde_json::from_slice(&bytes).map_err(|_| MaterializationIndexError::Encoding)?;
        if mutation_identity(&mutation)? != row[..32] {
            return Err(MaterializationIndexError::Corrupt(
                "source inventory identity",
            ));
        }
        Ok(mutation)
    }
}

/// Preserves the existing newest-first mutation semantics while opening old
/// metadata. Current full snapshots take the single ordered pass; overlapping
/// runs merge with one fixed-width row per metadata segment in memory.
pub struct SourceMutationRuns {
    inventory: SourceInventory,
    runs: Vec<Range<usize>>,
    ordered_snapshot: bool,
    prior: Option<[u8; 32]>,
}

impl SourceMutationRuns {
    pub fn new(root: &Path) -> Result<Self, MaterializationIndexError> {
        Ok(Self {
            inventory: SourceInventory::new(root)?,
            runs: Vec::new(),
            ordered_snapshot: true,
            prior: None,
        })
    }

    pub fn push_run(
        &mut self,
        mutations: Vec<SourceMutation>,
    ) -> Result<(), MaterializationIndexError> {
        if mutations.len() > MAX_METADATA_SEGMENT_ENTRIES {
            return Err(MaterializationIndexError::Bounds);
        }
        let start = self.inventory.len();
        let mut keyed = mutations
            .into_iter()
            .map(|mutation| Ok((mutation_identity(&mutation)?, mutation)))
            .collect::<Result<Vec<_>, MaterializationIndexError>>()?;
        // Stable ordering preserves the first occurrence within a segment too.
        keyed.sort_by_key(|(identity, _)| *identity);
        for (identity, mutation) in keyed {
            if self.prior.is_some_and(|prior| prior >= identity) {
                self.ordered_snapshot = false;
            }
            if let SourceMutation::Upsert { state, .. } = &mutation {
                if self.ordered_snapshot {
                    self.inventory
                        .snapshot
                        .push(state)
                        .map_err(|_| MaterializationIndexError::Conflict)?;
                }
            } else {
                self.ordered_snapshot = false;
            }
            self.inventory.append_mutation(identity, &mutation)?;
            self.prior = Some(identity);
        }
        if start != self.inventory.len() {
            self.runs.push(start..self.inventory.len());
        }
        Ok(())
    }

    pub fn finish(self, root: &Path) -> Result<SourceInventory, MaterializationIndexError> {
        if self.ordered_snapshot {
            return Ok(self.inventory);
        }
        let mut output = SourceInventory::new(root)?;
        let mut frontier = BinaryHeap::new();
        for (precedence, run) in self.runs.iter().enumerate() {
            let row = self.inventory.row(run.start)?;
            let identity: [u8; 32] = row[..32]
                .try_into()
                .map_err(|_| MaterializationIndexError::Encoding)?;
            frontier.push(Reverse((identity, precedence, run.start)));
        }
        let mut prior = None;
        while let Some(Reverse((identity, precedence, ordinal))) = frontier.pop() {
            if prior != Some(identity) {
                if let SourceMutation::Upsert {
                    state,
                    materializer_revision,
                } = self
                    .inventory
                    .decode_mutation(&self.inventory.row(ordinal)?)?
                {
                    output.push(&ActiveSource {
                        state,
                        materializer_revision,
                    })?;
                }
                prior = Some(identity);
            }
            let next = ordinal + 1;
            if next < self.runs[precedence].end {
                let row = self.inventory.row(next)?;
                let identity: [u8; 32] = row[..32]
                    .try_into()
                    .map_err(|_| MaterializationIndexError::Encoding)?;
                frontier.push(Reverse((identity, precedence, next)));
            }
        }
        Ok(output)
    }
}

fn mutation_identity(mutation: &SourceMutation) -> Result<[u8; 32], MaterializationIndexError> {
    match mutation {
        SourceMutation::Upsert { state, .. } => Ok(state.source.identity().digest()),
        SourceMutation::Removed { source_id } => {
            let digest = source_id
                .strip_prefix("core_source_")
                .ok_or(MaterializationIndexError::Conflict)?;
            let mut identity = [0; 32];
            hex::decode_to_slice(digest, &mut identity)
                .map_err(|_| MaterializationIndexError::Conflict)?;
            Ok(identity)
        }
    }
}

fn validate_source_mutation(mutation: &SourceMutation) -> Result<(), MaterializationIndexError> {
    match mutation {
        SourceMutation::Upsert {
            state,
            materializer_revision,
        } => {
            state
                .validate()
                .map_err(|_| MaterializationIndexError::Corrupt("source mutation"))?;
            validate_source_id(&crate::core_source_storage_id(&state.source))?;
            if materializer_revision.is_empty()
                || materializer_revision.len()
                    > ctx_attribution_model::MAX_CORE_MATERIALIZER_REVISION_BYTES
                || materializer_revision.chars().any(char::is_control)
            {
                return Err(MaterializationIndexError::Corrupt("source mutation"));
            }
        }
        SourceMutation::Removed { source_id } => validate_source_id(source_id)?,
    }
    Ok(())
}

fn validate_source_id(value: &str) -> Result<(), MaterializationIndexError> {
    let digest = value
        .strip_prefix("core_source_")
        .ok_or(MaterializationIndexError::Corrupt("source mutation"))?;
    if !is_lower_sha256(digest) {
        return Err(MaterializationIndexError::Corrupt("source mutation"));
    }
    Ok(())
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests;
