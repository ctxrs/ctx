use std::cmp::Ordering;
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::{self, Write as _};

#[cfg(test)]
use fst::MapBuilder;
use fst::{IntoStreamer as _, Map, Streamer as _};
use thiserror::Error;

use super::model::{
    EventOwnerKey, EventTombstone, FactFamily, MAX_FACT_FAMILY_BYTES, MAX_IDENTIFIER_BYTES,
    MAX_INDEX_TERM_BYTES, MAX_REPOSITORY_ID_BYTES, ServingModelError, ServingRecord,
};
use super::segment_file::{SegmentFile, SegmentFileError, SegmentWriter};

mod codec;
mod format;
mod index;
mod metrics;
mod query;
#[cfg(test)]
pub(crate) mod tests;
mod tombstone;
mod writer;

#[cfg(test)]
use codec::{add_frame_bytes, canonical_json};
use codec::{
    canonical_record_order, index_key, index_key_prefix, read_posting_page, tracked_read,
    unscoped_index_key, unscoped_index_key_prefix, validate_limit, validate_repository_scope,
    write_canonical_frame,
};
use format::{
    Directory, FstShardDescriptor, Header, Layout, read_u32, read_u64, write_u32, write_u64,
};
use index::{first_candidate_shard, shard_intersects};
#[cfg(any(test, feature = "test-support"))]
pub use metrics::FlatReadObservability;
use query::{QuerySelector, QueryState};
use tombstone::{decode_tombstone_page, find_tombstone_page};

pub const FLAT_SERVING_ROLE: u32 = 0x46_4c_41_54;
/// Checksum block size used for newly published production Flat files.
/// All index roles share the same bounded block framing.
pub const FLAT_CHUNK_BYTES: u32 = crate::SEGMENT_CHUNK_BYTES;
pub const MAX_QUERY_RESULTS: usize = 256;
pub const MAX_FLAT_RECORD_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_FLAT_RECORD_FRAME_BYTES: usize = 4 + MAX_FLAT_RECORD_PAYLOAD_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlatRecordWork {
    /// Exact bytes written for the canonical length-prefixed record frame.
    pub frame_bytes: usize,
    /// Scoped plus unscoped posting associations constructed by the writer.
    pub index_associations: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TombstoneQueryWork {
    pub pages: u64,
    pub page_bytes: u64,
    pub checked_chunks: u64,
    pub checked_bytes: u64,
}

const FORMAT_MAGIC: [u8; 8] = *b"CTXFLAT2";
const FORMAT_VERSION: u16 = 2;
pub const FLAT_FORMAT_VERSION: u16 = FORMAT_VERSION;
const HEADER_BYTES: usize = 96;
const KEY_VERSION: u8 = 2;
const SCOPED_KEY: u8 = 0;
const UNSCOPED_KEY: u8 = 1;
const REPOSITORY_SCOPE_DIGEST_BYTES: usize = 32;
const MAX_RECORD_BYTES: usize = MAX_FLAT_RECORD_PAYLOAD_BYTES;
const MAX_RECORDS: usize = 1_000_000;
const MAX_TOMBSTONES: usize = 250_000;
const MAX_INDEX_KEYS: usize = 8_000_000;
const MAX_INDEX_ASSOCIATIONS: u64 = 8_000_000;
const MAX_QUERY_POSTINGS_SCANNED: usize = 1_000_000;
const MAX_QUERY_FST_SHARDS_PER_PAGE: usize = 16;
const MAX_RECORD_SECTION_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOMBSTONE_SECTION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_POSTINGS_SECTION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FST_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FST_SHARD_BYTES: usize = 256 * 1024;
const MAX_FST_KEYS_PER_SHARD: usize = 1_024;
const MAX_FST_SHARDS: usize = 65_536;
const MAX_TOMBSTONE_PAGE_BYTES: usize = 64 * 1024;
const MAX_TOMBSTONE_PAGES: usize = 65_536;
const MAX_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FST_KEY_BYTES: usize =
    4 + REPOSITORY_SCOPE_DIGEST_BYTES + MAX_FACT_FAMILY_BYTES + MAX_INDEX_TERM_BYTES;

#[derive(Debug, Error)]
pub enum FlatSegmentError {
    #[error(transparent)]
    File(#[from] SegmentFileError),
    #[error("flat segment I/O failed")]
    Io(#[from] io::Error),
    #[error("flat segment FST operation failed")]
    Fst(#[from] fst::Error),
    #[error("flat segment canonical serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Model(#[from] ServingModelError),
    #[error("flat segment bound exceeded: {0}")]
    Bound(&'static str),
    #[error("flat segment is corrupt: {0}")]
    Corrupt(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlatSegmentStats {
    pub record_count: u32,
    pub tombstone_count: u32,
    pub index_key_count: u32,
    pub index_association_count: u64,
    pub plaintext_bytes: u64,
    pub fst_bytes: u64,
    pub fst_shard_count: u32,
    pub tombstone_page_count: u32,
    pub directory_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatQueryPage {
    pub records: Vec<ServingRecord>,
    pub repository_ambiguous: bool,
    pub continuation: Option<FlatQueryContinuation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnscopedQueryResult {
    pub records: Vec<ServingRecord>,
    pub repository_ambiguous: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatQueryContinuation {
    shard_index: usize,
    key: Option<Vec<u8>>,
    next_posting_index: usize,
    postings_scanned: usize,
    first_repository_id: Option<String>,
    repository_ambiguous: bool,
}

pub struct FlatSegmentWriter;

pub struct FlatSegmentReader {
    file: SegmentFile,
    layout: Layout,
    directory: Directory,
    fst_cache: Option<(usize, Map<Vec<u8>>)>,
    tombstone_cache: Option<(usize, Vec<EventOwnerKey>, usize)>,
    read_bytes: u64,
    range_reads: u64,
    cache_high_water_bytes: usize,
    cache_high_water_entries: usize,
}

impl FlatSegmentReader {
    pub fn open(file: SegmentFile) -> Result<Self, FlatSegmentError> {
        Self::open_source(file, MAX_DIRECTORY_BYTES)
    }

    pub fn open_bounded(
        file: SegmentFile,
        directory_budget: u64,
    ) -> Result<Self, FlatSegmentError> {
        Self::open_source(file, directory_budget)
    }

    fn open_source(mut file: SegmentFile, directory_budget: u64) -> Result<Self, FlatSegmentError> {
        let plaintext_bytes = file.plaintext_len();
        if plaintext_bytes
            < u64::try_from(HEADER_BYTES)
                .map_err(|_| FlatSegmentError::Corrupt("header length is not representable"))?
        {
            return Err(FlatSegmentError::Corrupt("truncated header"));
        }
        let mut read_bytes = 0_u64;
        let mut range_reads = 0_u64;
        let header_bytes = tracked_read(
            &mut file,
            &mut read_bytes,
            &mut range_reads,
            0,
            HEADER_BYTES,
        )?;
        let header = Header::decode(header_bytes.as_slice())?;
        header.validate_bounds()?;
        if header.directory_bytes > directory_budget {
            return Err(FlatSegmentError::Bound("graph Flat directory bytes"));
        }
        let layout = Layout::new(&header, plaintext_bytes)?;
        let directory_length = usize::try_from(layout.directory_bytes)
            .map_err(|_| FlatSegmentError::Bound("directory bytes"))?;
        let directory_bytes = tracked_read(
            &mut file,
            &mut read_bytes,
            &mut range_reads,
            layout.directory_start,
            directory_length,
        )?;
        let directory = Directory::decode(directory_bytes.as_slice(), &header)?;
        Ok(Self {
            file,
            layout,
            directory,
            fst_cache: None,
            tombstone_cache: None,
            read_bytes,
            range_reads,
            cache_high_water_bytes: 0,
            cache_high_water_entries: 0,
        })
    }

    pub fn contains_tombstone(&mut self, key: &EventOwnerKey) -> Result<bool, FlatSegmentError> {
        let Some(page_index) = find_tombstone_page(&self.directory, key) else {
            return Ok(false);
        };
        if self
            .tombstone_cache
            .as_ref()
            .is_some_and(|(cached, _, _)| *cached != page_index)
        {
            self.tombstone_cache = None;
        }
        if self.tombstone_cache.is_none() {
            let descriptor = self
                .directory
                .tombstone_pages
                .get(page_index)
                .ok_or(FlatSegmentError::Corrupt("tombstone page index"))?
                .clone();
            let start = self
                .layout
                .tombstone_pages_start
                .checked_add(descriptor.offset)
                .ok_or(FlatSegmentError::Corrupt("tombstone page offset"))?;
            let length = usize::try_from(descriptor.length)
                .map_err(|_| FlatSegmentError::Corrupt("tombstone page length"))?;
            let bytes = self.read_range(start, length)?;
            let keys = decode_tombstone_page(bytes.as_slice(), &descriptor)?;
            self.tombstone_cache = Some((page_index, keys, length));
            self.observe_cache_high_water();
        }
        self.tombstone_cache
            .as_ref()
            .map(|(_, keys, _)| keys.binary_search(key).is_ok())
            .ok_or(FlatSegmentError::Corrupt("tombstone page cache"))
    }

    pub fn tombstone_membership(
        &mut self,
        keys: &[EventOwnerKey],
    ) -> Result<Vec<bool>, FlatSegmentError> {
        if keys.len() > MAX_QUERY_RESULTS {
            return Err(FlatSegmentError::Bound("tombstone membership keys"));
        }
        let mut order = keys.iter().enumerate().collect::<Vec<_>>();
        order.sort_by(|left, right| left.1.cmp(right.1));
        let mut membership = vec![false; keys.len()];
        for (original_index, key) in order {
            membership[original_index] = self.contains_tombstone(key)?;
        }
        Ok(membership)
    }

    /// Computes a conservative checked-I/O plan before tombstone pages
    /// are loaded or decoded. Callers can reject aggregate graph work first.
    pub fn tombstone_query_work(
        &self,
        keys: &[EventOwnerKey],
    ) -> Result<TombstoneQueryWork, FlatSegmentError> {
        if keys.len() > MAX_QUERY_RESULTS {
            return Err(FlatSegmentError::Bound("tombstone membership keys"));
        }
        let page_indices = keys
            .iter()
            .filter_map(|key| find_tombstone_page(&self.directory, key))
            .collect::<BTreeSet<_>>();
        let mut work = TombstoneQueryWork {
            pages: u64::try_from(page_indices.len())
                .map_err(|_| FlatSegmentError::Bound("tombstone query pages"))?,
            ..TombstoneQueryWork::default()
        };
        let chunk_bytes = u64::from(self.file.chunk_bytes());
        let mut checked_chunks = BTreeSet::<u64>::new();
        for page_index in page_indices {
            let descriptor = self
                .directory
                .tombstone_pages
                .get(page_index)
                .ok_or(FlatSegmentError::Corrupt("tombstone page index"))?;
            work.page_bytes = work
                .page_bytes
                .checked_add(u64::from(descriptor.length))
                .ok_or(FlatSegmentError::Bound("tombstone query page bytes"))?;
            if chunk_bytes == 0 {
                continue;
            }
            let start = self
                .layout
                .tombstone_pages_start
                .checked_add(descriptor.offset)
                .ok_or(FlatSegmentError::Corrupt("tombstone page offset"))?;
            let end = start
                .checked_add(u64::from(descriptor.length))
                .ok_or(FlatSegmentError::Corrupt("tombstone page range"))?;
            if end == 0 || end > self.file.plaintext_len() {
                return Err(FlatSegmentError::Corrupt("tombstone page range"));
            }
            let first_chunk = start / chunk_bytes;
            let last_chunk = (end - 1) / chunk_bytes;
            checked_chunks.extend(first_chunk..=last_chunk);
        }
        work.checked_chunks = u64::try_from(checked_chunks.len())
            .map_err(|_| FlatSegmentError::Bound("tombstone checked chunks"))?;
        for chunk in checked_chunks {
            let start = chunk
                .checked_mul(chunk_bytes)
                .ok_or(FlatSegmentError::Bound("tombstone checked bytes"))?;
            work.checked_bytes = work
                .checked_bytes
                .checked_add(
                    self.file
                        .plaintext_len()
                        .saturating_sub(start)
                        .min(chunk_bytes),
                )
                .ok_or(FlatSegmentError::Bound("tombstone checked bytes"))?;
        }
        Ok(work)
    }

    #[must_use]
    pub fn directory_bytes(&self) -> u64 {
        self.layout.directory_bytes
    }

    pub fn query_exact(
        &mut self,
        repository_id: &str,
        fact_family: &FactFamily,
        term: &str,
        limit: usize,
    ) -> Result<Vec<ServingRecord>, FlatSegmentError> {
        Self::collect_pages(self, limit, |reader, continuation| {
            reader.query_exact_page(
                repository_id,
                fact_family,
                term,
                continuation,
                MAX_QUERY_RESULTS,
            )
        })
        .map(|page| page.records)
    }

    pub fn query_prefix(
        &mut self,
        repository_id: &str,
        fact_family: &FactFamily,
        term_prefix: &str,
        limit: usize,
    ) -> Result<Vec<ServingRecord>, FlatSegmentError> {
        Self::collect_pages(self, limit, |reader, continuation| {
            reader.query_prefix_page(
                repository_id,
                fact_family,
                term_prefix,
                continuation,
                MAX_QUERY_RESULTS,
            )
        })
        .map(|page| page.records)
    }

    pub fn query_exact_unscoped(
        &mut self,
        fact_family: &FactFamily,
        term: &str,
        limit: usize,
    ) -> Result<UnscopedQueryResult, FlatSegmentError> {
        Self::collect_pages(self, limit, |reader, continuation| {
            reader.query_exact_unscoped_page(fact_family, term, continuation, MAX_QUERY_RESULTS)
        })
        .map(|page| UnscopedQueryResult {
            records: page.records,
            repository_ambiguous: page.repository_ambiguous,
        })
    }

    pub fn query_prefix_unscoped(
        &mut self,
        fact_family: &FactFamily,
        term_prefix: &str,
        limit: usize,
    ) -> Result<UnscopedQueryResult, FlatSegmentError> {
        Self::collect_pages(self, limit, |reader, continuation| {
            reader.query_prefix_unscoped_page(
                fact_family,
                term_prefix,
                continuation,
                MAX_QUERY_RESULTS,
            )
        })
        .map(|page| UnscopedQueryResult {
            records: page.records,
            repository_ambiguous: page.repository_ambiguous,
        })
    }

    pub fn query_exact_page(
        &mut self,
        repository_id: &str,
        fact_family: &FactFamily,
        term: &str,
        continuation: Option<&FlatQueryContinuation>,
        limit: usize,
    ) -> Result<FlatQueryPage, FlatSegmentError> {
        let key = index_key(repository_id, fact_family, term)?;
        let selector = QuerySelector::Exact {
            repository_id,
            fact_family,
            term,
        };
        self.query_page(&key, term.len(), true, &selector, continuation, limit)
    }

    pub fn query_prefix_page(
        &mut self,
        repository_id: &str,
        fact_family: &FactFamily,
        term_prefix: &str,
        continuation: Option<&FlatQueryContinuation>,
        limit: usize,
    ) -> Result<FlatQueryPage, FlatSegmentError> {
        let prefix = index_key_prefix(repository_id, fact_family, term_prefix)?;
        let selector = QuerySelector::Prefix {
            repository_id,
            fact_family,
            term_prefix,
        };
        self.query_page(
            &prefix,
            term_prefix.len(),
            false,
            &selector,
            continuation,
            limit,
        )
    }

    pub fn query_exact_unscoped_page(
        &mut self,
        fact_family: &FactFamily,
        term: &str,
        continuation: Option<&FlatQueryContinuation>,
        limit: usize,
    ) -> Result<FlatQueryPage, FlatSegmentError> {
        let key = unscoped_index_key(fact_family, term)?;
        let selector = QuerySelector::ExactUnscoped { fact_family, term };
        self.query_page(&key, term.len(), true, &selector, continuation, limit)
    }

    pub fn query_prefix_unscoped_page(
        &mut self,
        fact_family: &FactFamily,
        term_prefix: &str,
        continuation: Option<&FlatQueryContinuation>,
        limit: usize,
    ) -> Result<FlatQueryPage, FlatSegmentError> {
        let prefix = unscoped_index_key_prefix(fact_family, term_prefix)?;
        let selector = QuerySelector::PrefixUnscoped {
            fact_family,
            term_prefix,
        };
        self.query_page(
            &prefix,
            term_prefix.len(),
            false,
            &selector,
            continuation,
            limit,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn query_page(
        &mut self,
        prefix: &[u8],
        term_prefix_bytes: usize,
        exact: bool,
        selector: &QuerySelector<'_>,
        continuation: Option<&FlatQueryContinuation>,
        limit: usize,
    ) -> Result<FlatQueryPage, FlatSegmentError> {
        validate_limit(limit)?;
        let term_offset = prefix
            .len()
            .checked_sub(term_prefix_bytes)
            .ok_or(FlatSegmentError::Corrupt("query key term offset"))?;
        let mut state = QueryState::resume(continuation);
        state.validate_continuation()?;
        if let Some(value) = continuation
            && (value.shard_index >= self.directory.fst_shards.len()
                || value
                    .key
                    .as_ref()
                    .is_some_and(|key| !key.starts_with(prefix) || (exact && key != prefix))
                || (value.key.is_none() && (exact || value.next_posting_index != 0)))
        {
            return Err(FlatSegmentError::Corrupt("query continuation scope"));
        }
        let Some(mut shard_index) = continuation
            .map(|value| value.shard_index)
            .or_else(|| first_candidate_shard(&self.directory, prefix))
        else {
            return Ok(state.into_page(None));
        };
        let mut resume_key = continuation.and_then(|value| value.key.as_deref());
        let mut shards_read = 0_usize;
        while shard_index < self.directory.fst_shards.len() {
            let descriptor = self
                .directory
                .fst_shards
                .get(shard_index)
                .ok_or(FlatSegmentError::Corrupt("FST shard index"))?
                .clone();
            if !shard_intersects(&descriptor, prefix, exact) {
                break;
            }
            if shards_read == MAX_QUERY_FST_SHARDS_PER_PAGE {
                return Ok(state.into_page(Some((shard_index, None, 0))));
            }
            shards_read = shards_read
                .checked_add(1)
                .ok_or(FlatSegmentError::Bound("query FST shards"))?;
            let index = self.load_fst_shard(shard_index, &descriptor)?;
            let start = resume_key.unwrap_or(prefix);
            let mut stream = index.range().ge(start).into_stream();
            let mut continuation_pending = resume_key.is_some();
            while let Some((key, posting_offset)) = stream.next() {
                if !key.starts_with(prefix) || (exact && key != prefix) {
                    break;
                }
                let posting_index = if continuation_pending {
                    let expected = continuation
                        .and_then(|value| value.key.as_deref())
                        .ok_or(FlatSegmentError::Corrupt("missing query continuation"))?;
                    if key != expected {
                        return Err(FlatSegmentError::Corrupt("query continuation key"));
                    }
                    continuation_pending = false;
                    continuation.map_or(0, |value| value.next_posting_index)
                } else {
                    0
                };
                let indexed_term = std::str::from_utf8(
                    key.get(term_offset..)
                        .ok_or(FlatSegmentError::Corrupt("query key term"))?,
                )
                .map_err(|_| FlatSegmentError::Corrupt("query key term"))?;
                let next_posting_index = read_posting_page(
                    &mut self.file,
                    &mut self.read_bytes,
                    &mut self.range_reads,
                    &self.layout,
                    posting_offset,
                    posting_index,
                    key,
                    indexed_term,
                    selector,
                    limit,
                    &mut state,
                )?;
                if state.results.len() == limit {
                    let next = if let Some(next_posting_index) = next_posting_index {
                        Some((shard_index, Some(key.to_vec()), next_posting_index))
                    } else if exact {
                        None
                    } else if let Some((next_key, _)) = stream.next() {
                        next_key
                            .starts_with(prefix)
                            .then(|| (shard_index, Some(next_key.to_vec()), 0))
                    } else {
                        self.next_prefix_shard(shard_index + 1, prefix)
                            .map(|next| (next, None, 0))
                    };
                    self.store_fst_cache(shard_index, index);
                    return Ok(state.into_page(next));
                }
            }
            if continuation_pending {
                return Err(FlatSegmentError::Corrupt("query continuation key"));
            }
            self.store_fst_cache(shard_index, index);
            if exact {
                break;
            }
            let Some(next) = self.next_prefix_shard(shard_index + 1, prefix) else {
                break;
            };
            shard_index = next;
            resume_key = None;
        }
        Ok(state.into_page(None))
    }

    fn load_fst_shard(
        &mut self,
        shard_index: usize,
        descriptor: &FstShardDescriptor,
    ) -> Result<Map<Vec<u8>>, FlatSegmentError> {
        if self
            .fst_cache
            .as_ref()
            .is_some_and(|(cached, _)| *cached == shard_index)
        {
            return self
                .fst_cache
                .take()
                .map(|(_, index)| index)
                .ok_or(FlatSegmentError::Corrupt("FST shard cache"));
        }
        self.fst_cache = None;
        let start = self
            .layout
            .fst_shards_start
            .checked_add(descriptor.offset)
            .ok_or(FlatSegmentError::Corrupt("FST shard offset"))?;
        let length = usize::try_from(descriptor.length)
            .map_err(|_| FlatSegmentError::Corrupt("FST shard length"))?;
        let bytes = self.read_range(start, length)?;
        let index = Map::new(bytes)?;
        if index.len()
            != usize::try_from(descriptor.key_count)
                .map_err(|_| FlatSegmentError::Corrupt("FST shard key count"))?
        {
            return Err(FlatSegmentError::Corrupt("FST shard key count"));
        }
        let mut stream = index.stream();
        let first = stream
            .next()
            .map(|(key, _)| key.to_vec())
            .ok_or(FlatSegmentError::Corrupt("empty FST shard"))?;
        let mut last = first.clone();
        while let Some((key, _)) = stream.next() {
            last.clear();
            last.extend_from_slice(key);
        }
        if first != descriptor.first_key || last != descriptor.last_key {
            return Err(FlatSegmentError::Corrupt("FST shard fences"));
        }
        Ok(index)
    }

    fn store_fst_cache(&mut self, shard_index: usize, index: Map<Vec<u8>>) {
        self.fst_cache = Some((shard_index, index));
        self.observe_cache_high_water();
    }

    fn next_prefix_shard(&self, start: usize, prefix: &[u8]) -> Option<usize> {
        self.directory
            .fst_shards
            .iter()
            .enumerate()
            .skip(start)
            .find_map(|(index, descriptor)| {
                if shard_intersects(descriptor, prefix, false) {
                    Some(index)
                } else {
                    None
                }
            })
    }

    fn read_range(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, FlatSegmentError> {
        tracked_read(
            &mut self.file,
            &mut self.read_bytes,
            &mut self.range_reads,
            offset,
            length,
        )
        .map_err(FlatSegmentError::from)
    }

    fn collect_pages(
        reader: &mut Self,
        limit: usize,
        mut query: impl FnMut(
            &mut Self,
            Option<&FlatQueryContinuation>,
        ) -> Result<FlatQueryPage, FlatSegmentError>,
    ) -> Result<FlatQueryPage, FlatSegmentError> {
        validate_limit(limit)?;
        let mut continuation = None;
        let mut records = Vec::with_capacity(limit);
        loop {
            let page = query(reader, continuation.as_ref())?;
            let repository_ambiguous = page.repository_ambiguous;
            for record in page.records {
                let position = records.partition_point(|existing| {
                    canonical_record_order(existing, &record) != Ordering::Greater
                });
                if records.len() < limit {
                    records.insert(position, record);
                } else if position < limit {
                    records.insert(position, record);
                    records.pop();
                }
            }
            continuation = page.continuation;
            if continuation.is_none() {
                return Ok(FlatQueryPage {
                    records,
                    repository_ambiguous,
                    continuation: None,
                });
            }
        }
    }
}
