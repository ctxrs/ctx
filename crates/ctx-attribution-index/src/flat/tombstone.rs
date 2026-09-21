use std::io::Write as _;

use super::format::{
    Directory, TombstonePageDescriptor, minimum_tombstone_page_bytes, read_u16, read_u32, write_u32,
};
use super::{
    EventOwnerKey, EventTombstone, FlatSegmentError, MAX_IDENTIFIER_BYTES,
    MAX_TOMBSTONE_PAGE_BYTES, MAX_TOMBSTONE_PAGES, MAX_TOMBSTONE_SECTION_BYTES, SegmentWriter,
};

pub struct TombstonePagePlan {
    pub descriptor: TombstonePageDescriptor,
    start: usize,
}

pub fn canonical_tombstone_keys(mut tombstones: Vec<EventTombstone>) -> Vec<EventOwnerKey> {
    tombstones.sort();
    let mut keys = Vec::with_capacity(tombstones.len());
    for tombstone in tombstones {
        let key = tombstone.key();
        if keys.last() != Some(&key) {
            keys.push(key);
        }
    }
    keys
}

pub fn plan_tombstone_pages(
    keys: &[EventOwnerKey],
) -> Result<Vec<TombstonePagePlan>, FlatSegmentError> {
    let mut plans = Vec::new();
    let mut start = 0_usize;
    let mut page_bytes = 4_usize;
    for (index, key) in keys.iter().enumerate() {
        let entry_bytes = tombstone_entry_bytes(key)?;
        if index > start
            && page_bytes
                .checked_add(entry_bytes)
                .ok_or(FlatSegmentError::Bound("tombstone page bytes"))?
                > MAX_TOMBSTONE_PAGE_BYTES
        {
            push_plan(keys, start, index, page_bytes, &mut plans)?;
            start = index;
            page_bytes = 4;
        }
        page_bytes = page_bytes
            .checked_add(entry_bytes)
            .ok_or(FlatSegmentError::Bound("tombstone page bytes"))?;
        if page_bytes > MAX_TOMBSTONE_PAGE_BYTES {
            return Err(FlatSegmentError::Bound("tombstone page bytes"));
        }
    }
    if start < keys.len() {
        push_plan(keys, start, keys.len(), page_bytes, &mut plans)?;
    }
    if plans.len() > MAX_TOMBSTONE_PAGES {
        return Err(FlatSegmentError::Bound("tombstone page count"));
    }
    let total = plans.iter().try_fold(0_u64, |bytes, plan| {
        bytes
            .checked_add(u64::from(plan.descriptor.length))
            .ok_or(FlatSegmentError::Bound("tombstone section bytes"))
    })?;
    if total > MAX_TOMBSTONE_SECTION_BYTES {
        return Err(FlatSegmentError::Bound("tombstone section bytes"));
    }
    Ok(plans)
}

fn push_plan(
    keys: &[EventOwnerKey],
    start: usize,
    end: usize,
    page_bytes: usize,
    plans: &mut Vec<TombstonePagePlan>,
) -> Result<(), FlatSegmentError> {
    let first_key = keys
        .get(start)
        .ok_or(FlatSegmentError::Corrupt("empty tombstone page plan"))?
        .clone();
    let last_key = keys
        .get(end.saturating_sub(1))
        .ok_or(FlatSegmentError::Corrupt("empty tombstone page plan"))?
        .clone();
    let offset = plans.iter().try_fold(0_u64, |bytes, plan| {
        bytes
            .checked_add(u64::from(plan.descriptor.length))
            .ok_or(FlatSegmentError::Bound("tombstone section bytes"))
    })?;
    plans.push(TombstonePagePlan {
        descriptor: TombstonePageDescriptor {
            offset,
            length: u32::try_from(page_bytes)
                .map_err(|_| FlatSegmentError::Bound("tombstone page bytes"))?,
            key_count: u32::try_from(end - start)
                .map_err(|_| FlatSegmentError::Bound("tombstone page key count"))?,
            first_key,
            last_key,
        },
        start,
    });
    Ok(())
}

pub fn write_tombstone_pages(
    writer: &mut SegmentWriter,
    keys: &[EventOwnerKey],
    plans: &[TombstonePagePlan],
) -> Result<(), FlatSegmentError> {
    for plan in plans {
        write_u32(writer, plan.descriptor.key_count)?;
        let end = plan
            .start
            .checked_add(
                usize::try_from(plan.descriptor.key_count)
                    .map_err(|_| FlatSegmentError::Bound("tombstone page key count"))?,
            )
            .ok_or(FlatSegmentError::Bound("tombstone page key count"))?;
        for key in keys
            .get(plan.start..end)
            .ok_or(FlatSegmentError::Corrupt("tombstone page plan"))?
        {
            writer.write_all(
                &u16::try_from(key.source_id.len())
                    .map_err(|_| FlatSegmentError::Bound("tombstone source bytes"))?
                    .to_le_bytes(),
            )?;
            writer.write_all(
                &u16::try_from(key.event_id.len())
                    .map_err(|_| FlatSegmentError::Bound("tombstone event bytes"))?
                    .to_le_bytes(),
            )?;
            writer.write_all(key.source_id.as_bytes())?;
            writer.write_all(key.event_id.as_bytes())?;
        }
    }
    Ok(())
}

pub fn tombstone_directory(plans: &[TombstonePagePlan]) -> Vec<TombstonePageDescriptor> {
    plans.iter().map(|plan| plan.descriptor.clone()).collect()
}

pub fn find_tombstone_page(directory: &Directory, key: &EventOwnerKey) -> Option<usize> {
    let index = directory
        .tombstone_pages
        .partition_point(|page| &page.last_key < key);
    directory
        .tombstone_pages
        .get(index)
        .is_some_and(|page| &page.first_key <= key)
        .then_some(index)
}

pub fn decode_tombstone_page(
    bytes: &[u8],
    descriptor: &TombstonePageDescriptor,
) -> Result<Vec<EventOwnerKey>, FlatSegmentError> {
    if bytes.len()
        != usize::try_from(descriptor.length)
            .map_err(|_| FlatSegmentError::Corrupt("tombstone page length"))?
        || read_u32(bytes, 0)? != descriptor.key_count
    {
        return Err(FlatSegmentError::Corrupt("tombstone page header"));
    }
    let count = usize::try_from(descriptor.key_count)
        .map_err(|_| FlatSegmentError::Corrupt("tombstone page key count"))?;
    if minimum_tombstone_page_bytes(descriptor.key_count)
        .is_none_or(|minimum| bytes.len() < minimum)
    {
        return Err(FlatSegmentError::Corrupt("tombstone page key count"));
    }
    let mut cursor = 4_usize;
    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        let source_len = usize::from(read_u16(bytes, cursor)?);
        cursor = cursor
            .checked_add(2)
            .ok_or(FlatSegmentError::Corrupt("tombstone page offset"))?;
        let event_len = usize::from(read_u16(bytes, cursor)?);
        cursor = cursor
            .checked_add(2)
            .ok_or(FlatSegmentError::Corrupt("tombstone page offset"))?;
        if source_len == 0
            || source_len > MAX_IDENTIFIER_BYTES
            || event_len == 0
            || event_len > MAX_IDENTIFIER_BYTES
        {
            return Err(FlatSegmentError::Corrupt("tombstone page key bound"));
        }
        let source_end = cursor
            .checked_add(source_len)
            .ok_or(FlatSegmentError::Corrupt("tombstone page range"))?;
        let source_id = std::str::from_utf8(
            bytes
                .get(cursor..source_end)
                .ok_or(FlatSegmentError::Corrupt("tombstone page range"))?,
        )
        .map_err(|_| FlatSegmentError::Corrupt("tombstone page encoding"))?
        .to_owned();
        cursor = source_end;
        let event_end = cursor
            .checked_add(event_len)
            .ok_or(FlatSegmentError::Corrupt("tombstone page range"))?;
        let event_id = std::str::from_utf8(
            bytes
                .get(cursor..event_end)
                .ok_or(FlatSegmentError::Corrupt("tombstone page range"))?,
        )
        .map_err(|_| FlatSegmentError::Corrupt("tombstone page encoding"))?
        .to_owned();
        cursor = event_end;
        keys.push(EventOwnerKey {
            source_id,
            event_id,
        });
    }
    if cursor != bytes.len()
        || keys.windows(2).any(|pair| pair[0] >= pair[1])
        || keys.first() != Some(&descriptor.first_key)
        || keys.last() != Some(&descriptor.last_key)
    {
        return Err(FlatSegmentError::Corrupt("tombstone page ordering"));
    }
    Ok(keys)
}

fn tombstone_entry_bytes(key: &EventOwnerKey) -> Result<usize, FlatSegmentError> {
    4_usize
        .checked_add(key.source_id.len())
        .and_then(|bytes| bytes.checked_add(key.event_id.len()))
        .ok_or(FlatSegmentError::Bound("tombstone page bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_rejects_impossible_cardinality_before_allocating_keys() {
        let descriptor = TombstonePageDescriptor {
            offset: 0,
            length: 4,
            key_count: u32::try_from(super::super::MAX_TOMBSTONES).expect("bounded fixture"),
            first_key: EventOwnerKey {
                source_id: "a".to_owned(),
                event_id: "a".to_owned(),
            },
            last_key: EventOwnerKey {
                source_id: "z".to_owned(),
                event_id: "z".to_owned(),
            },
        };
        assert!(decode_tombstone_page(&descriptor.key_count.to_le_bytes(), &descriptor).is_err());
    }
}
