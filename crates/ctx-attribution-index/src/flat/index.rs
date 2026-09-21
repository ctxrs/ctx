use std::collections::BTreeMap;
use std::io::Write as _;

use fst::MapBuilder;

use super::format::{Directory, FstShardDescriptor};
use super::{
    FlatSegmentError, MAX_FST_BYTES, MAX_FST_KEYS_PER_SHARD, MAX_FST_SHARD_BYTES, MAX_FST_SHARDS,
    SegmentWriter,
};

pub struct FstShardPlan {
    pub descriptor: FstShardDescriptor,
    first_posting_offset: u64,
}

struct IndexEntry<'a> {
    key: &'a [u8],
    posting_offset: u64,
}

pub fn plan_fst_shards(
    postings: &BTreeMap<Vec<u8>, Vec<u64>>,
) -> Result<Vec<FstShardPlan>, FlatSegmentError> {
    let mut plans = Vec::new();
    let mut entries = Vec::with_capacity(MAX_FST_KEYS_PER_SHARD);
    let mut posting_offset = 0_u64;
    for (key, offsets) in postings {
        entries.push(IndexEntry {
            key,
            posting_offset,
        });
        posting_offset = posting_offset
            .checked_add(posting_list_bytes(offsets.len())?)
            .ok_or(FlatSegmentError::Bound("postings section bytes"))?;
        if entries.len() == MAX_FST_KEYS_PER_SHARD {
            plan_group(&entries, &mut plans)?;
            entries.clear();
        }
    }
    if !entries.is_empty() {
        plan_group(&entries, &mut plans)?;
    }
    if plans.len() > MAX_FST_SHARDS {
        return Err(FlatSegmentError::Bound("FST shard count"));
    }
    let total = plans.iter().try_fold(0_u64, |bytes, plan| {
        bytes
            .checked_add(u64::from(plan.descriptor.length))
            .ok_or(FlatSegmentError::Bound("FST bytes"))
    })?;
    if total > MAX_FST_BYTES {
        return Err(FlatSegmentError::Bound("FST bytes"));
    }
    Ok(plans)
}

fn plan_group(
    entries: &[IndexEntry<'_>],
    plans: &mut Vec<FstShardPlan>,
) -> Result<(), FlatSegmentError> {
    let encoded = build_fst(entries)?;
    if encoded.len() > MAX_FST_SHARD_BYTES {
        if entries.len() == 1 {
            return Err(FlatSegmentError::Bound("FST shard bytes"));
        }
        let middle = entries.len() / 2;
        plan_group(&entries[..middle], plans)?;
        plan_group(&entries[middle..], plans)?;
        return Ok(());
    }
    let first = entries
        .first()
        .ok_or(FlatSegmentError::Corrupt("empty FST shard plan"))?;
    let last = entries
        .last()
        .ok_or(FlatSegmentError::Corrupt("empty FST shard plan"))?;
    let offset = plans.iter().try_fold(0_u64, |bytes, plan| {
        bytes
            .checked_add(u64::from(plan.descriptor.length))
            .ok_or(FlatSegmentError::Bound("FST bytes"))
    })?;
    plans.push(FstShardPlan {
        descriptor: FstShardDescriptor {
            offset,
            length: u32::try_from(encoded.len())
                .map_err(|_| FlatSegmentError::Bound("FST shard bytes"))?,
            key_count: u32::try_from(entries.len())
                .map_err(|_| FlatSegmentError::Bound("FST shard key count"))?,
            first_key: first.key.to_vec(),
            last_key: last.key.to_vec(),
        },
        first_posting_offset: first.posting_offset,
    });
    Ok(())
}

fn build_fst(entries: &[IndexEntry<'_>]) -> Result<Vec<u8>, FlatSegmentError> {
    let mut builder = MapBuilder::memory();
    for entry in entries {
        builder.insert(entry.key, entry.posting_offset)?;
    }
    Ok(builder.into_inner()?)
}

pub fn write_fst_shards(
    writer: &mut SegmentWriter,
    postings: &BTreeMap<Vec<u8>, Vec<u64>>,
    plans: &[FstShardPlan],
) -> Result<(), FlatSegmentError> {
    for plan in plans {
        let mut builder = MapBuilder::memory();
        let mut posting_offset = plan.first_posting_offset;
        let mut count = 0_u32;
        let mut last_key = None;
        for (key, offsets) in
            postings.range(plan.descriptor.first_key.clone()..=plan.descriptor.last_key.clone())
        {
            builder.insert(key, posting_offset)?;
            posting_offset = posting_offset
                .checked_add(posting_list_bytes(offsets.len())?)
                .ok_or(FlatSegmentError::Bound("postings section bytes"))?;
            count = count
                .checked_add(1)
                .ok_or(FlatSegmentError::Bound("FST shard key count"))?;
            last_key = Some(key.as_slice());
        }
        if count != plan.descriptor.key_count
            || last_key != Some(plan.descriptor.last_key.as_slice())
        {
            return Err(FlatSegmentError::Corrupt("FST shard plan"));
        }
        let encoded = builder.into_inner()?;
        if encoded.len()
            != usize::try_from(plan.descriptor.length)
                .map_err(|_| FlatSegmentError::Bound("FST shard bytes"))?
        {
            return Err(FlatSegmentError::Corrupt("FST shard size accounting"));
        }
        writer.write_all(encoded.as_slice())?;
    }
    Ok(())
}

pub fn fst_directory(plans: &[FstShardPlan]) -> Vec<FstShardDescriptor> {
    plans.iter().map(|plan| plan.descriptor.clone()).collect()
}

pub fn first_candidate_shard(directory: &Directory, key: &[u8]) -> Option<usize> {
    let index = directory
        .fst_shards
        .partition_point(|shard| shard.last_key.as_slice() < key);
    (index < directory.fst_shards.len()).then_some(index)
}

pub fn shard_intersects(descriptor: &FstShardDescriptor, prefix: &[u8], exact: bool) -> bool {
    if exact {
        descriptor.first_key.as_slice() <= prefix && prefix <= descriptor.last_key.as_slice()
    } else {
        descriptor.last_key.as_slice() >= prefix
            && (descriptor.first_key.starts_with(prefix)
                || descriptor.first_key.as_slice() <= prefix)
    }
}

pub fn posting_list_bytes(count: usize) -> Result<u64, FlatSegmentError> {
    u64::try_from(count)
        .ok()
        .and_then(|count| count.checked_mul(8))
        .and_then(|bytes| bytes.checked_add(4))
        .ok_or(FlatSegmentError::Bound("posting list bytes"))
}
