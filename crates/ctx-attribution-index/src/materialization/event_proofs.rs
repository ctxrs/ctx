//! Disposable, ordered prior-event proofs. Each immutable index is opened once.
//! Runs share two scratch files; their merge retains one fixed key per segment.
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{MaterializationIndexError, runtime_file::RuntimeFile};
use crate::{EventIndexEntry, EventIndexReader, EventIndexSource, IndexedCoreEventState};
use ctx_history_core::StableEntityId;

mod encoding;

const ROW_BYTES: usize = 108;
type Key = ([u8; 32], [u8; 32]);

#[derive(Serialize, Deserialize)]
struct Proof(#[serde(with = "encoding::Entry")] EventIndexEntry);

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static PROOF_WRITES: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn proof_writes_for_test() -> (u64, u64) {
    PROOF_WRITES.with(std::cell::Cell::get)
}

pub struct EventProofs {
    rows: RuntimeFile,
    values: RuntimeFile,
    count: u64,
}

impl EventProofs {
    pub fn new(root: &Path) -> Result<Self, MaterializationIndexError> {
        Ok(Self {
            rows: RuntimeFile::new(root)?,
            values: RuntimeFile::new(root)?,
            count: 0,
        })
    }

    pub fn append_index(
        &mut self,
        reader: &mut EventIndexReader,
        include: &dyn Fn(&EventIndexSource) -> Result<bool, MaterializationIndexError>,
        cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Range<u64>, MaterializationIndexError> {
        let start = self.count;
        let mut prior = None;
        // The reader's checked dictionaries are bounded by this one segment.
        // Do not clone its entire source dictionary to iterate it.
        for ordinal in 0..reader.sources().len() {
            if cancelled.is_some_and(|check| check()) {
                return Err(MaterializationIndexError::Cancelled);
            }
            let source = reader.sources()[ordinal].clone();
            if !include(&source)? {
                continue;
            }
            let mut after = None;
            loop {
                if cancelled.is_some_and(|check| check()) {
                    return Err(MaterializationIndexError::Cancelled);
                }
                let page = reader.page(&source, after, crate::MAX_EVENT_INDEX_PAGE_ITEMS)?;
                for entry in page.entries {
                    let key = (source.source.identity().digest(), entry.event_id().digest());
                    if prior.is_some_and(|prior| prior >= key) {
                        return Err(MaterializationIndexError::Corrupt(
                            "event proof run ordering",
                        ));
                    }
                    prior = Some(key);
                    after = Some(entry.event_id());
                    self.append(key, entry)?;
                }
                if page.terminal {
                    break;
                }
            }
        }
        Ok(start..self.count)
    }

    fn append(
        &mut self,
        key: Key,
        entry: EventIndexEntry,
    ) -> Result<(), MaterializationIndexError> {
        let bytes =
            serde_json::to_vec(&Proof(entry)).map_err(|_| MaterializationIndexError::Encoding)?;
        let length = u32::try_from(bytes.len()).map_err(|_| MaterializationIndexError::Bounds)?;
        let offset = self.values.append(&bytes)?;
        #[cfg(any(test, feature = "test-support"))]
        PROOF_WRITES.with(|work| {
            let (rows, bytes_written) = work.get();
            work.set((
                rows + 1,
                bytes_written + bytes.len() as u64 + ROW_BYTES as u64,
            ));
        });
        let mut row = [0; ROW_BYTES];
        row[..32].copy_from_slice(&key.0);
        row[32..64].copy_from_slice(&key.1);
        row[64..72].copy_from_slice(&offset.to_le_bytes());
        row[72..76].copy_from_slice(&length.to_le_bytes());
        row[76..].copy_from_slice(&Sha256::digest(&bytes));
        self.rows.append(&row)?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or(MaterializationIndexError::Bounds)?;
        Ok(())
    }

    /// Keep newest-layer states, checking exact same-layer equality. Tombstones
    /// suppress older states; replacements in their own layer remain visible.
    /// The merged rows reference existing values, without copying their payload.
    pub fn merge(
        mut self,
        root: &Path,
        runs: &[(u64, Range<u64>)],
        cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Self, MaterializationIndexError> {
        let rows = RuntimeFile::new(root)?;
        let mut frontier = BinaryHeap::new();
        for (run, (layer, range)) in runs.iter().enumerate() {
            if !range.is_empty() {
                frontier.push(Reverse((
                    Self::key(&self.row(range.start)?),
                    Reverse(*layer),
                    run,
                    range.start,
                )));
            }
        }
        let mut count = 0_u64;
        while let Some(Reverse((key, _, _, _))) = frontier.peek().copied() {
            if cancelled.is_some_and(|check| check()) {
                return Err(MaterializationIndexError::Cancelled);
            }
            let mut highest = None;
            let mut winner: Option<(IndexedCoreEventState, [u8; ROW_BYTES])> = None;
            while frontier
                .peek()
                .is_some_and(|Reverse((next, _, _, _))| *next == key)
            {
                let Reverse((_, _, run, ordinal)) =
                    frontier.pop().ok_or(MaterializationIndexError::Conflict)?;
                let (layer, range) = &runs[run];
                if highest.is_none_or(|highest| *layer >= highest) {
                    if highest != Some(*layer) {
                        winner = None;
                        highest = Some(*layer);
                    }
                    let row = self.row(ordinal)?;
                    if let EventIndexEntry::State { state, .. } = self.decode(&row)? {
                        if winner.as_ref().is_some_and(|(prior, _)| prior != &state) {
                            return Err(MaterializationIndexError::Corrupt(
                                "same-layer event states conflict",
                            ));
                        }
                        winner = Some((state, row));
                    }
                }
                let next = ordinal
                    .checked_add(1)
                    .ok_or(MaterializationIndexError::Bounds)?;
                if next < range.end {
                    frontier.push(Reverse((
                        Self::key(&self.row(next)?),
                        Reverse(*layer),
                        run,
                        next,
                    )));
                }
            }
            if let Some((_, row)) = winner {
                rows.append(&row)?;
                count = count
                    .checked_add(1)
                    .ok_or(MaterializationIndexError::Bounds)?;
            }
        }
        self.rows = rows;
        self.count = count;
        Ok(self)
    }

    pub fn page(
        &self,
        source: [u8; 32],
        after: Option<StableEntityId>,
        maximum: usize,
    ) -> Result<(Vec<IndexedCoreEventState>, bool), MaterializationIndexError> {
        let mut ordinal =
            self.lower_bound((source, after.map_or([0; 32], StableEntityId::digest)))?;
        let mut states = Vec::new();
        while ordinal < self.count {
            let row = self.row(ordinal)?;
            let key = Self::key(&row);
            if key.0 != source {
                break;
            }
            ordinal += 1;
            if after.is_some_and(|after| key.1 <= after.digest()) {
                continue;
            }
            if states.len() == maximum {
                return Ok((states, false));
            }
            states.push(self.state(&row)?);
        }
        Ok((states, true))
    }

    pub fn lookup(
        &self,
        source: [u8; 32],
        event: StableEntityId,
    ) -> Result<Option<IndexedCoreEventState>, MaterializationIndexError> {
        let key = (source, event.digest());
        let ordinal = self.lower_bound(key)?;
        if ordinal == self.count {
            return Ok(None);
        }
        let row = self.row(ordinal)?;
        if Self::key(&row) == key {
            self.state(&row).map(Some)
        } else {
            Ok(None)
        }
    }

    fn state(
        &self,
        row: &[u8; ROW_BYTES],
    ) -> Result<IndexedCoreEventState, MaterializationIndexError> {
        match self.decode(row)? {
            EventIndexEntry::State { state, .. } => Ok(state),
            EventIndexEntry::Tombstone(_) => Err(MaterializationIndexError::Corrupt(
                "visible event proof is a tombstone",
            )),
        }
    }

    fn lower_bound(&self, key: Key) -> Result<u64, MaterializationIndexError> {
        let mut low = 0;
        let mut high = self.count;
        while low < high {
            let mid = low + (high - low) / 2;
            if Self::key(&self.row(mid)?) < key {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        Ok(low)
    }

    fn row(&self, ordinal: u64) -> Result<[u8; ROW_BYTES], MaterializationIndexError> {
        if ordinal >= self.count {
            return Err(MaterializationIndexError::Conflict);
        }
        let offset = ordinal
            .checked_mul(ROW_BYTES as u64)
            .ok_or(MaterializationIndexError::Bounds)?;
        let mut row = [0; ROW_BYTES];
        self.rows.read_at(offset, &mut row)?;
        Ok(row)
    }

    fn key(row: &[u8; ROW_BYTES]) -> Key {
        let mut source = [0; 32];
        let mut event = [0; 32];
        source.copy_from_slice(&row[..32]);
        event.copy_from_slice(&row[32..64]);
        (source, event)
    }

    fn decode(&self, row: &[u8; ROW_BYTES]) -> Result<EventIndexEntry, MaterializationIndexError> {
        let offset = u64::from_le_bytes(
            row[64..72]
                .try_into()
                .map_err(|_| MaterializationIndexError::Encoding)?,
        );
        let length = u32::from_le_bytes(
            row[72..76]
                .try_into()
                .map_err(|_| MaterializationIndexError::Encoding)?,
        ) as usize;
        // Runtime bytes are disposable, but corruption must not drive allocation.
        let file_length = self.values.len()?;
        if length > ctx_attribution_model::MAX_CORE_CONTROL_WIRE_BYTES
            || offset
                .checked_add(length as u64)
                .is_none_or(|end| end > file_length)
        {
            return Err(MaterializationIndexError::Corrupt(
                "event proof frame length",
            ));
        }
        let mut bytes = vec![0; length];
        self.values.read_at(offset, &mut bytes)?;
        if Sha256::digest(&bytes).as_slice() != &row[76..] {
            return Err(MaterializationIndexError::Corrupt("event proof checksum"));
        }
        let Proof(entry) = serde_json::from_slice(&bytes)
            .map_err(|_| MaterializationIndexError::Corrupt("event proof encoding"))?;
        if Self::key(row) != (entry.event_id().source_digest(), entry.event_id().digest()) {
            return Err(MaterializationIndexError::Corrupt("event proof key"));
        }
        Ok(entry)
    }
}
