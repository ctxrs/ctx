use std::collections::BTreeMap;
use std::io::Write as _;

use super::index::{fst_directory, plan_fst_shards, posting_list_bytes, write_fst_shards};
use super::tombstone::{
    canonical_tombstone_keys, plan_tombstone_pages, tombstone_directory, write_tombstone_pages,
};
use super::*;

impl FlatSegmentWriter {
    #[allow(clippy::too_many_lines)]
    pub fn write(
        writer: SegmentWriter,
        mut records: Vec<ServingRecord>,
        tombstones: Vec<EventTombstone>,
    ) -> Result<FlatSegmentStats, FlatSegmentError> {
        if records.len() > MAX_RECORDS {
            return Err(FlatSegmentError::Bound("record count"));
        }
        if tombstones.len() > MAX_TOMBSTONES {
            return Err(FlatSegmentError::Bound("tombstone count"));
        }
        for record in &records {
            record.validate()?;
        }
        for tombstone in &tombstones {
            tombstone.validate()?;
        }

        records.sort_by(canonical_record_order);
        let tombstone_keys = canonical_tombstone_keys(tombstones);
        let mut records_bytes = 0_u64;
        let mut postings = BTreeMap::<Vec<u8>, Vec<u64>>::new();
        let mut index_association_count = 0_u64;
        for record in &records {
            let work = Self::record_work(record)?;
            let record_offset = records_bytes;
            records_bytes = records_bytes
                .checked_add(
                    u64::try_from(work.frame_bytes)
                        .map_err(|_| FlatSegmentError::Bound("record section"))?,
                )
                .ok_or(FlatSegmentError::Bound("record section"))?;
            if records_bytes > MAX_RECORD_SECTION_BYTES {
                return Err(FlatSegmentError::Bound("record section bytes"));
            }
            for term in &record.index_terms {
                for key in [
                    index_key(&record.repository_id, &record.fact_family, term)?,
                    unscoped_index_key(&record.fact_family, term)?,
                ] {
                    postings.entry(key).or_default().push(record_offset);
                    index_association_count = index_association_count
                        .checked_add(1)
                        .ok_or(FlatSegmentError::Bound("index association count"))?;
                    if index_association_count > MAX_INDEX_ASSOCIATIONS {
                        return Err(FlatSegmentError::Bound("index association count"));
                    }
                }
            }
        }
        if postings.len() > MAX_INDEX_KEYS {
            return Err(FlatSegmentError::Bound("index key count"));
        }

        let postings_bytes = postings.values().try_fold(0_u64, |total, offsets| {
            total
                .checked_add(posting_list_bytes(offsets.len())?)
                .ok_or(FlatSegmentError::Bound("postings section bytes"))
        })?;
        if postings_bytes > MAX_POSTINGS_SECTION_BYTES {
            return Err(FlatSegmentError::Bound("postings section bytes"));
        }
        let tombstone_plans = plan_tombstone_pages(&tombstone_keys)?;
        let fst_plans = plan_fst_shards(&postings)?;
        let tombstone_pages_bytes = tombstone_plans.iter().try_fold(0_u64, |total, plan| {
            total
                .checked_add(u64::from(plan.descriptor.length))
                .ok_or(FlatSegmentError::Bound("tombstone section bytes"))
        })?;
        let fst_shards_bytes = fst_plans.iter().try_fold(0_u64, |total, plan| {
            total
                .checked_add(u64::from(plan.descriptor.length))
                .ok_or(FlatSegmentError::Bound("FST bytes"))
        })?;
        let directory = Directory {
            fst_shards: fst_directory(&fst_plans),
            tombstone_pages: tombstone_directory(&tombstone_plans),
        };
        let directory_bytes = directory.encode()?;
        let header = Header {
            record_count: u32::try_from(records.len())
                .map_err(|_| FlatSegmentError::Bound("record count"))?,
            index_key_count: u32::try_from(postings.len())
                .map_err(|_| FlatSegmentError::Bound("index key count"))?,
            tombstone_count: u32::try_from(tombstone_keys.len())
                .map_err(|_| FlatSegmentError::Bound("tombstone count"))?,
            fst_shard_count: u32::try_from(fst_plans.len())
                .map_err(|_| FlatSegmentError::Bound("FST shard count"))?,
            tombstone_page_count: u32::try_from(tombstone_plans.len())
                .map_err(|_| FlatSegmentError::Bound("tombstone page count"))?,
            index_association_count,
            records_bytes,
            tombstone_pages_bytes,
            postings_bytes,
            fst_shards_bytes,
            directory_bytes: u64::try_from(directory_bytes.len())
                .map_err(|_| FlatSegmentError::Bound("directory bytes"))?,
        };

        let mut writer = writer;
        writer.write_all(&header.encode())?;
        for record in &records {
            write_canonical_frame(&mut writer, record, MAX_RECORD_BYTES)?;
        }
        write_tombstone_pages(&mut writer, &tombstone_keys, &tombstone_plans)?;
        for offsets in postings.values() {
            write_u32(
                &mut writer,
                u32::try_from(offsets.len())
                    .map_err(|_| FlatSegmentError::Bound("posting list count"))?,
            )?;
            for offset in offsets {
                write_u64(&mut writer, *offset)?;
            }
        }
        write_fst_shards(&mut writer, &postings, &fst_plans)?;
        writer.write_all(directory_bytes.as_slice())?;
        let plaintext_bytes = u64::try_from(HEADER_BYTES)
            .ok()
            .and_then(|bytes| bytes.checked_add(records_bytes))
            .and_then(|bytes| bytes.checked_add(tombstone_pages_bytes))
            .and_then(|bytes| bytes.checked_add(postings_bytes))
            .and_then(|bytes| bytes.checked_add(fst_shards_bytes))
            .and_then(|bytes| {
                u64::try_from(directory_bytes.len())
                    .ok()
                    .and_then(|length| bytes.checked_add(length))
            })
            .ok_or(FlatSegmentError::Bound("segment plaintext bytes"))?;
        writer.finish()?;
        Ok(FlatSegmentStats {
            record_count: header.record_count,
            tombstone_count: header.tombstone_count,
            index_key_count: header.index_key_count,
            index_association_count,
            plaintext_bytes,
            fst_bytes: fst_shards_bytes,
            fst_shard_count: header.fst_shard_count,
            tombstone_page_count: header.tombstone_page_count,
            directory_bytes: header.directory_bytes,
        })
    }
}
