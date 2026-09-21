use super::*;

const DIRECTORY_MAGIC: [u8; 8] = *b"CTXFDIR2";
const DIRECTORY_VERSION: u16 = 2;
const DIRECTORY_HEADER_BYTES: usize = 24;
const FLAT_HEADER_BYTES: u16 = 96;
const TOMBSTONE_PAGE_HEADER_BYTES: usize = 4;
const MIN_TOMBSTONE_KEY_BYTES: usize = 6;

#[derive(Clone, Copy)]
pub struct Header {
    pub record_count: u32,
    pub index_key_count: u32,
    pub tombstone_count: u32,
    pub fst_shard_count: u32,
    pub tombstone_page_count: u32,
    pub index_association_count: u64,
    pub records_bytes: u64,
    pub tombstone_pages_bytes: u64,
    pub postings_bytes: u64,
    pub fst_shards_bytes: u64,
    pub directory_bytes: u64,
}

impl Header {
    pub fn encode(self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0_u8; HEADER_BYTES];
        bytes[..8].copy_from_slice(&FORMAT_MAGIC);
        bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&FLAT_HEADER_BYTES.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.record_count.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.index_key_count.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.tombstone_count.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.fst_shard_count.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.tombstone_page_count.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.index_association_count.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.records_bytes.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.tombstone_pages_bytes.to_le_bytes());
        bytes[56..64].copy_from_slice(&self.postings_bytes.to_le_bytes());
        bytes[64..72].copy_from_slice(&self.fst_shards_bytes.to_le_bytes());
        bytes[72..80].copy_from_slice(&self.directory_bytes.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FlatSegmentError> {
        if bytes.len() != HEADER_BYTES
            || bytes.get(..8) != Some(FORMAT_MAGIC.as_slice())
            || read_u16(bytes, 8)? != FORMAT_VERSION
            || usize::from(read_u16(bytes, 10)?) != HEADER_BYTES
            || bytes.get(80..HEADER_BYTES) != Some([0_u8; HEADER_BYTES - 80].as_slice())
        {
            return Err(FlatSegmentError::Corrupt("header"));
        }
        Ok(Self {
            record_count: read_u32(bytes, 12)?,
            index_key_count: read_u32(bytes, 16)?,
            tombstone_count: read_u32(bytes, 20)?,
            fst_shard_count: read_u32(bytes, 24)?,
            tombstone_page_count: read_u32(bytes, 28)?,
            index_association_count: read_u64(bytes, 32)?,
            records_bytes: read_u64(bytes, 40)?,
            tombstone_pages_bytes: read_u64(bytes, 48)?,
            postings_bytes: read_u64(bytes, 56)?,
            fst_shards_bytes: read_u64(bytes, 64)?,
            directory_bytes: read_u64(bytes, 72)?,
        })
    }

    pub fn validate_bounds(self) -> Result<(), FlatSegmentError> {
        if usize::try_from(self.record_count).map_or(true, |count| count > MAX_RECORDS)
            || usize::try_from(self.tombstone_count).map_or(true, |count| count > MAX_TOMBSTONES)
            || usize::try_from(self.index_key_count).map_or(true, |count| count > MAX_INDEX_KEYS)
            || usize::try_from(self.fst_shard_count).map_or(true, |count| count > MAX_FST_SHARDS)
            || usize::try_from(self.tombstone_page_count)
                .map_or(true, |count| count > MAX_TOMBSTONE_PAGES)
            || self.index_association_count > MAX_INDEX_ASSOCIATIONS
            || self.records_bytes > MAX_RECORD_SECTION_BYTES
            || self.tombstone_pages_bytes > MAX_TOMBSTONE_SECTION_BYTES
            || self.postings_bytes > MAX_POSTINGS_SECTION_BYTES
            || self.fst_shards_bytes > MAX_FST_BYTES
            || self.directory_bytes < DIRECTORY_HEADER_BYTES as u64
            || self.directory_bytes > MAX_DIRECTORY_BYTES
        {
            return Err(FlatSegmentError::Corrupt("header bounds"));
        }
        if (self.record_count == 0) != (self.records_bytes == 0)
            || (self.tombstone_count == 0)
                != (self.tombstone_pages_bytes == 0 && self.tombstone_page_count == 0)
            || (self.index_key_count == 0)
                != (self.postings_bytes == 0
                    && self.fst_shards_bytes == 0
                    && self.fst_shard_count == 0
                    && self.index_association_count == 0)
        {
            return Err(FlatSegmentError::Corrupt("header section accounting"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct FstShardDescriptor {
    pub offset: u64,
    pub length: u32,
    pub key_count: u32,
    pub first_key: Vec<u8>,
    pub last_key: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct TombstonePageDescriptor {
    pub offset: u64,
    pub length: u32,
    pub key_count: u32,
    pub first_key: EventOwnerKey,
    pub last_key: EventOwnerKey,
}

#[derive(Clone, Debug)]
pub struct Directory {
    pub fst_shards: Vec<FstShardDescriptor>,
    pub tombstone_pages: Vec<TombstonePageDescriptor>,
}

impl Directory {
    pub fn encode(&self) -> Result<Vec<u8>, FlatSegmentError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&DIRECTORY_MAGIC);
        push_u16(&mut bytes, DIRECTORY_VERSION);
        push_u16(
            &mut bytes,
            u16::try_from(DIRECTORY_HEADER_BYTES)
                .map_err(|_| FlatSegmentError::Bound("directory header bytes"))?,
        );
        push_u32(
            &mut bytes,
            u32::try_from(self.fst_shards.len())
                .map_err(|_| FlatSegmentError::Bound("FST shard count"))?,
        );
        push_u32(
            &mut bytes,
            u32::try_from(self.tombstone_pages.len())
                .map_err(|_| FlatSegmentError::Bound("tombstone page count"))?,
        );
        bytes.extend_from_slice(&[0_u8; 4]);
        for shard in &self.fst_shards {
            push_u64(&mut bytes, shard.offset);
            push_u32(&mut bytes, shard.length);
            push_u32(&mut bytes, shard.key_count);
            push_len(&mut bytes, shard.first_key.len(), "FST fence bytes")?;
            push_len(&mut bytes, shard.last_key.len(), "FST fence bytes")?;
            bytes.extend_from_slice(&shard.first_key);
            bytes.extend_from_slice(&shard.last_key);
        }
        for page in &self.tombstone_pages {
            push_u64(&mut bytes, page.offset);
            push_u32(&mut bytes, page.length);
            push_u32(&mut bytes, page.key_count);
            push_len(
                &mut bytes,
                page.first_key.source_id.len(),
                "tombstone fence bytes",
            )?;
            push_len(
                &mut bytes,
                page.first_key.event_id.len(),
                "tombstone fence bytes",
            )?;
            push_len(
                &mut bytes,
                page.last_key.source_id.len(),
                "tombstone fence bytes",
            )?;
            push_len(
                &mut bytes,
                page.last_key.event_id.len(),
                "tombstone fence bytes",
            )?;
            bytes.extend_from_slice(page.first_key.source_id.as_bytes());
            bytes.extend_from_slice(page.first_key.event_id.as_bytes());
            bytes.extend_from_slice(page.last_key.source_id.as_bytes());
            bytes.extend_from_slice(page.last_key.event_id.as_bytes());
        }
        if bytes.len() > usize::try_from(MAX_DIRECTORY_BYTES).unwrap_or(usize::MAX) {
            return Err(FlatSegmentError::Bound("directory bytes"));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8], header: &Header) -> Result<Self, FlatSegmentError> {
        if bytes.len() < DIRECTORY_HEADER_BYTES
            || bytes.get(..8) != Some(DIRECTORY_MAGIC.as_slice())
            || read_u16(bytes, 8)? != DIRECTORY_VERSION
            || usize::from(read_u16(bytes, 10)?) != DIRECTORY_HEADER_BYTES
            || bytes.get(20..24) != Some([0_u8; 4].as_slice())
        {
            return Err(FlatSegmentError::Corrupt("directory header"));
        }
        let fst_count = usize::try_from(read_u32(bytes, 12)?)
            .map_err(|_| FlatSegmentError::Corrupt("FST shard count"))?;
        let tombstone_count = usize::try_from(read_u32(bytes, 16)?)
            .map_err(|_| FlatSegmentError::Corrupt("tombstone page count"))?;
        if fst_count
            != usize::try_from(header.fst_shard_count)
                .map_err(|_| FlatSegmentError::Corrupt("FST shard count"))?
            || tombstone_count
                != usize::try_from(header.tombstone_page_count)
                    .map_err(|_| FlatSegmentError::Corrupt("tombstone page count"))?
        {
            return Err(FlatSegmentError::Corrupt("directory counts"));
        }
        let mut cursor = DIRECTORY_HEADER_BYTES;
        let mut fst_shards = Vec::with_capacity(fst_count);
        for _ in 0..fst_count {
            let offset = take_u64(bytes, &mut cursor)?;
            let length = take_u32(bytes, &mut cursor)?;
            let key_count = take_u32(bytes, &mut cursor)?;
            let first_len = usize::from(take_u16(bytes, &mut cursor)?);
            let last_len = usize::from(take_u16(bytes, &mut cursor)?);
            if length == 0
                || usize::try_from(length).map_or(true, |length| length > MAX_FST_SHARD_BYTES)
                || key_count == 0
                || usize::try_from(key_count).map_or(true, |count| count > MAX_FST_KEYS_PER_SHARD)
                || first_len == 0
                || first_len > MAX_FST_KEY_BYTES
                || last_len == 0
                || last_len > MAX_FST_KEY_BYTES
            {
                return Err(FlatSegmentError::Corrupt("FST shard directory"));
            }
            let first_key = take(bytes, &mut cursor, first_len)?.to_vec();
            let last_key = take(bytes, &mut cursor, last_len)?.to_vec();
            fst_shards.push(FstShardDescriptor {
                offset,
                length,
                key_count,
                first_key,
                last_key,
            });
        }
        let mut tombstone_pages = Vec::with_capacity(tombstone_count);
        for _ in 0..tombstone_count {
            let offset = take_u64(bytes, &mut cursor)?;
            let length = take_u32(bytes, &mut cursor)?;
            let key_count = take_u32(bytes, &mut cursor)?;
            let first_source_len = usize::from(take_u16(bytes, &mut cursor)?);
            let first_event_len = usize::from(take_u16(bytes, &mut cursor)?);
            let last_source_len = usize::from(take_u16(bytes, &mut cursor)?);
            let last_event_len = usize::from(take_u16(bytes, &mut cursor)?);
            if minimum_tombstone_page_bytes(key_count).is_none_or(|minimum| {
                usize::try_from(length).map_or(true, |length| {
                    length < minimum || length > MAX_TOMBSTONE_PAGE_BYTES
                })
            }) || [
                first_source_len,
                first_event_len,
                last_source_len,
                last_event_len,
            ]
            .into_iter()
            .any(|length| length == 0 || length > MAX_IDENTIFIER_BYTES)
            {
                return Err(FlatSegmentError::Corrupt("tombstone page directory"));
            }
            let first_key = take_owner_key(bytes, &mut cursor, first_source_len, first_event_len)?;
            let last_key = take_owner_key(bytes, &mut cursor, last_source_len, last_event_len)?;
            tombstone_pages.push(TombstonePageDescriptor {
                offset,
                length,
                key_count,
                first_key,
                last_key,
            });
        }
        if cursor != bytes.len() {
            return Err(FlatSegmentError::Corrupt("directory length"));
        }
        let directory = Self {
            fst_shards,
            tombstone_pages,
        };
        directory.validate(header)?;
        Ok(directory)
    }

    fn validate(&self, header: &Header) -> Result<(), FlatSegmentError> {
        let mut expected_offset = 0_u64;
        let mut key_count = 0_u64;
        let mut previous_last: Option<&[u8]> = None;
        for shard in &self.fst_shards {
            if shard.offset != expected_offset
                || shard.length == 0
                || usize::try_from(shard.length).map_or(true, |length| length > MAX_FST_SHARD_BYTES)
                || shard.key_count == 0
                || usize::try_from(shard.key_count)
                    .map_or(true, |count| count > MAX_FST_KEYS_PER_SHARD)
                || shard.first_key.is_empty()
                || shard.first_key.len() > MAX_FST_KEY_BYTES
                || shard.last_key.is_empty()
                || shard.last_key.len() > MAX_FST_KEY_BYTES
                || shard.first_key > shard.last_key
                || previous_last.is_some_and(|last| last >= shard.first_key.as_slice())
            {
                return Err(FlatSegmentError::Corrupt("FST shard directory"));
            }
            expected_offset = expected_offset
                .checked_add(u64::from(shard.length))
                .ok_or(FlatSegmentError::Corrupt("FST shard ranges"))?;
            key_count = key_count
                .checked_add(u64::from(shard.key_count))
                .ok_or(FlatSegmentError::Corrupt("FST shard key count"))?;
            previous_last = Some(&shard.last_key);
        }
        if expected_offset != header.fst_shards_bytes
            || key_count != u64::from(header.index_key_count)
        {
            return Err(FlatSegmentError::Corrupt("FST shard accounting"));
        }

        expected_offset = 0;
        key_count = 0;
        let mut previous_tombstone: Option<&EventOwnerKey> = None;
        for page in &self.tombstone_pages {
            if page.offset != expected_offset
                || minimum_tombstone_page_bytes(page.key_count).is_none_or(|minimum| {
                    usize::try_from(page.length).map_or(true, |length| {
                        length < minimum || length > MAX_TOMBSTONE_PAGE_BYTES
                    })
                })
                || page.first_key > page.last_key
                || previous_tombstone.is_some_and(|last| last >= &page.first_key)
            {
                return Err(FlatSegmentError::Corrupt("tombstone page directory"));
            }
            validate_owner_key(&page.first_key)?;
            validate_owner_key(&page.last_key)?;
            expected_offset = expected_offset
                .checked_add(u64::from(page.length))
                .ok_or(FlatSegmentError::Corrupt("tombstone page ranges"))?;
            key_count = key_count
                .checked_add(u64::from(page.key_count))
                .ok_or(FlatSegmentError::Corrupt("tombstone page key count"))?;
            previous_tombstone = Some(&page.last_key);
        }
        if expected_offset != header.tombstone_pages_bytes
            || key_count != u64::from(header.tombstone_count)
        {
            return Err(FlatSegmentError::Corrupt("tombstone page accounting"));
        }
        Ok(())
    }
}

pub fn minimum_tombstone_page_bytes(key_count: u32) -> Option<usize> {
    let count = usize::try_from(key_count).ok()?;
    if count == 0 {
        return None;
    }
    count
        .checked_mul(MIN_TOMBSTONE_KEY_BYTES)
        .and_then(|bytes| bytes.checked_add(TOMBSTONE_PAGE_HEADER_BYTES))
}

#[derive(Clone, Copy)]
pub struct Layout {
    pub records_start: u64,
    pub records_bytes: u64,
    pub tombstone_pages_start: u64,
    pub postings_start: u64,
    pub postings_bytes: u64,
    pub fst_shards_start: u64,
    pub directory_start: u64,
    pub directory_bytes: u64,
}

impl Layout {
    pub fn new(header: &Header, plaintext_bytes: u64) -> Result<Self, FlatSegmentError> {
        let records_start =
            u64::try_from(HEADER_BYTES).map_err(|_| FlatSegmentError::Corrupt("header length"))?;
        let tombstone_pages_start = records_start
            .checked_add(header.records_bytes)
            .ok_or(FlatSegmentError::Corrupt("record section range"))?;
        let postings_start = tombstone_pages_start
            .checked_add(header.tombstone_pages_bytes)
            .ok_or(FlatSegmentError::Corrupt("tombstone section range"))?;
        let fst_shards_start = postings_start
            .checked_add(header.postings_bytes)
            .ok_or(FlatSegmentError::Corrupt("postings section range"))?;
        let directory_start = fst_shards_start
            .checked_add(header.fst_shards_bytes)
            .ok_or(FlatSegmentError::Corrupt("FST shard section range"))?;
        let end = directory_start
            .checked_add(header.directory_bytes)
            .ok_or(FlatSegmentError::Corrupt("directory range"))?;
        if end != plaintext_bytes {
            return Err(FlatSegmentError::Corrupt("section ranges"));
        }
        Ok(Self {
            records_start,
            records_bytes: header.records_bytes,
            tombstone_pages_start,
            postings_start,
            postings_bytes: header.postings_bytes,
            fst_shards_start,
            directory_start,
            directory_bytes: header.directory_bytes,
        })
    }
}

pub fn write_u32(writer: &mut SegmentWriter, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

pub fn write_u64(writer: &mut SegmentWriter, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

pub fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, FlatSegmentError> {
    let encoded: [u8; 2] = bytes
        .get(
            offset
                ..offset
                    .checked_add(2)
                    .ok_or(FlatSegmentError::Corrupt("integer offset"))?,
        )
        .ok_or(FlatSegmentError::Corrupt("integer range"))?
        .try_into()
        .map_err(|_| FlatSegmentError::Corrupt("integer encoding"))?;
    Ok(u16::from_le_bytes(encoded))
}

pub fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, FlatSegmentError> {
    let encoded: [u8; 4] = bytes
        .get(
            offset
                ..offset
                    .checked_add(4)
                    .ok_or(FlatSegmentError::Corrupt("integer offset"))?,
        )
        .ok_or(FlatSegmentError::Corrupt("integer range"))?
        .try_into()
        .map_err(|_| FlatSegmentError::Corrupt("integer encoding"))?;
    Ok(u32::from_le_bytes(encoded))
}

pub fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, FlatSegmentError> {
    let encoded: [u8; 8] = bytes
        .get(
            offset
                ..offset
                    .checked_add(8)
                    .ok_or(FlatSegmentError::Corrupt("integer offset"))?,
        )
        .ok_or(FlatSegmentError::Corrupt("integer range"))?
        .try_into()
        .map_err(|_| FlatSegmentError::Corrupt("integer encoding"))?;
    Ok(u64::from_le_bytes(encoded))
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_len(
    bytes: &mut Vec<u8>,
    length: usize,
    bound: &'static str,
) -> Result<(), FlatSegmentError> {
    push_u16(
        bytes,
        u16::try_from(length).map_err(|_| FlatSegmentError::Bound(bound))?,
    );
    Ok(())
}

fn take<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    length: usize,
) -> Result<&'a [u8], FlatSegmentError> {
    let end = cursor
        .checked_add(length)
        .ok_or(FlatSegmentError::Corrupt("directory offset"))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(FlatSegmentError::Corrupt("directory range"))?;
    *cursor = end;
    Ok(value)
}

fn take_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, FlatSegmentError> {
    let value = read_u16(bytes, *cursor)?;
    *cursor = cursor
        .checked_add(2)
        .ok_or(FlatSegmentError::Corrupt("directory offset"))?;
    Ok(value)
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, FlatSegmentError> {
    let value = read_u32(bytes, *cursor)?;
    *cursor = cursor
        .checked_add(4)
        .ok_or(FlatSegmentError::Corrupt("directory offset"))?;
    Ok(value)
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, FlatSegmentError> {
    let value = read_u64(bytes, *cursor)?;
    *cursor = cursor
        .checked_add(8)
        .ok_or(FlatSegmentError::Corrupt("directory offset"))?;
    Ok(value)
}

fn take_owner_key(
    bytes: &[u8],
    cursor: &mut usize,
    source_len: usize,
    event_len: usize,
) -> Result<EventOwnerKey, FlatSegmentError> {
    let source_id = std::str::from_utf8(take(bytes, cursor, source_len)?)
        .map_err(|_| FlatSegmentError::Corrupt("tombstone fence encoding"))?
        .to_owned();
    let event_id = std::str::from_utf8(take(bytes, cursor, event_len)?)
        .map_err(|_| FlatSegmentError::Corrupt("tombstone fence encoding"))?
        .to_owned();
    let key = EventOwnerKey {
        source_id,
        event_id,
    };
    validate_owner_key(&key)?;
    Ok(key)
}

fn validate_owner_key(key: &EventOwnerKey) -> Result<(), FlatSegmentError> {
    EventTombstone {
        source_id: key.source_id.clone(),
        event_id: key.event_id.clone(),
        event_sequence: 0,
    }
    .validate()
    .map_err(|_| FlatSegmentError::Corrupt("tombstone fence model"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_header() -> Header {
        Header {
            record_count: 0,
            index_key_count: 0,
            tombstone_count: 0,
            fst_shard_count: 0,
            tombstone_page_count: 0,
            index_association_count: 0,
            records_bytes: 0,
            tombstone_pages_bytes: 0,
            postings_bytes: 0,
            fst_shards_bytes: 0,
            directory_bytes: 0,
        }
    }

    #[test]
    fn directory_rejects_oversized_fst_cardinality_before_shard_loading() {
        let key_count = u32::try_from(MAX_FST_KEYS_PER_SHARD + 1).expect("bounded fixture");
        let directory = Directory {
            fst_shards: vec![FstShardDescriptor {
                offset: 0,
                length: 8,
                key_count,
                first_key: b"a".to_vec(),
                last_key: b"z".to_vec(),
            }],
            tombstone_pages: Vec::new(),
        };
        let encoded = directory.encode().expect("encode malformed directory");
        let mut header = empty_header();
        header.index_key_count = key_count;
        header.fst_shard_count = 1;
        header.fst_shards_bytes = 8;
        header.directory_bytes = u64::try_from(encoded.len()).expect("directory bytes");
        assert!(Directory::decode(encoded.as_slice(), &header).is_err());
    }

    #[test]
    fn directory_rejects_tombstone_count_impossible_for_page_bytes() {
        let key_count = u32::try_from(MAX_TOMBSTONES).expect("bounded fixture");
        let directory = Directory {
            fst_shards: Vec::new(),
            tombstone_pages: vec![TombstonePageDescriptor {
                offset: 0,
                length: 4,
                key_count,
                first_key: EventOwnerKey {
                    source_id: "a".to_owned(),
                    event_id: "a".to_owned(),
                },
                last_key: EventOwnerKey {
                    source_id: "z".to_owned(),
                    event_id: "z".to_owned(),
                },
            }],
        };
        let encoded = directory.encode().expect("encode malformed directory");
        let mut header = empty_header();
        header.tombstone_count = key_count;
        header.tombstone_page_count = 1;
        header.tombstone_pages_bytes = 4;
        header.directory_bytes = u64::try_from(encoded.len()).expect("directory bytes");
        assert!(Directory::decode(encoded.as_slice(), &header).is_err());
    }
}
