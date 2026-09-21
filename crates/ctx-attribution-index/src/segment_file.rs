//! Plaintext block framing. Checksums detect corruption, not intentional edits.
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write};
use std::path::Path;

use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::filesystem;

pub const SEGMENT_CHUNK_BYTES: u32 = 16 * 1024;
pub const SEGMENT_HEADER_BYTES: u64 = 96;
pub const SEGMENT_CHECKSUM_BYTES: u64 = 32;
pub const MAX_BLOCK_CACHE_ENTRIES: usize = 1;
pub const MAX_BLOCK_CACHE_BYTES: usize = SEGMENT_CHUNK_BYTES as usize;
pub const MAX_SEGMENT_READ_ALL_BYTES: usize = 128 * 1024 * 1024;
const MAX_RANGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = crate::manifest::MAX_SEGMENT_PLAINTEXT_BYTES;
const MAGIC: &[u8; 8] = b"CTXIDX01";

#[derive(Debug, Error)]
pub enum SegmentFileError {
    #[error("attribution segment I/O failed")]
    Io(#[from] io::Error),
    #[error("attribution segment is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("attribution segment bound exceeded: {0}")]
    Bounds(&'static str),
}

pub struct SegmentWriter {
    file: File,
    generation: [u8; 32],
    role: u32,
    length: u64,
    pending: Vec<u8>,
}

impl SegmentWriter {
    pub fn create(
        path: &Path,
        generation: [u8; 32],
        role: u32,
        chunk_bytes: u32,
    ) -> Result<Self, SegmentFileError> {
        if chunk_bytes != SEGMENT_CHUNK_BYTES {
            return Err(SegmentFileError::Bounds("block size"));
        }
        let mut file = filesystem::create_private_file_new(path)?;
        // An interrupted writer has an invalid header and cannot be published.
        file.write_all(&[0; SEGMENT_HEADER_BYTES as usize])?;
        Ok(Self {
            file,
            generation,
            role,
            length: 0,
            pending: Vec::with_capacity(chunk_bytes as usize),
        })
    }

    fn write_block(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.file.write_all(&self.pending)?;
        self.file.write_all(&Sha256::digest(&self.pending))?;
        self.pending.clear();
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), SegmentFileError> {
        self.write_block()?;
        let header = encode_header(self.generation, self.role, self.length);
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)?;
        self.file.sync_all()?;
        Ok(())
    }
}

impl Write for SegmentWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = self
            .length
            .checked_add(bytes.len() as u64)
            .filter(|length| *length <= MAX_FILE_BYTES)
            .ok_or_else(|| io::Error::other("segment length bound"))?;
        for part in bytes.chunks(SEGMENT_CHUNK_BYTES as usize) {
            let mut remaining = part;
            while !remaining.is_empty() {
                let count = remaining
                    .len()
                    .min(SEGMENT_CHUNK_BYTES as usize - self.pending.len());
                self.pending.extend_from_slice(&remaining[..count]);
                remaining = &remaining[count..];
                if self.pending.len() == SEGMENT_CHUNK_BYTES as usize {
                    self.write_block()?;
                }
            }
        }
        self.length = length;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

pub struct SegmentFile {
    file: File,
    start: u64,
    length: u64,
    file_length: u64,
    chunk_reads: u64,
    cache: Option<(u64, Vec<u8>)>,
}

impl SegmentFile {
    pub fn open(path: &Path, generation: [u8; 32], role: u32) -> Result<Self, SegmentFileError> {
        let file = filesystem::open(path)?;
        let length = file.metadata()?.len();
        Self::open_file_region(file, path, 0, length, generation, role)
    }

    pub fn open_file_region(
        mut file: File,
        _path: &Path,
        region_start: u64,
        region_bytes: u64,
        generation: [u8; 32],
        role: u32,
    ) -> Result<Self, SegmentFileError> {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || region_bytes < SEGMENT_HEADER_BYTES
            || region_start
                .checked_add(region_bytes)
                .is_none_or(|end| end > metadata.len())
        {
            return Err(SegmentFileError::Corrupt("file region"));
        }
        file.seek(SeekFrom::Start(region_start))?;
        let mut header = [0_u8; SEGMENT_HEADER_BYTES as usize];
        file.read_exact(&mut header)?;
        let length = decode_header(&header, generation, role)?;
        if physical_bytes(length)? != region_bytes {
            return Err(SegmentFileError::Corrupt("file length"));
        }
        Ok(Self {
            file,
            start: region_start,
            length,
            file_length: metadata.len(),
            chunk_reads: 0,
            cache: None,
        })
    }

    pub const fn plaintext_len(&self) -> u64 {
        self.length
    }
    pub const fn chunk_bytes(&self) -> u32 {
        SEGMENT_CHUNK_BYTES
    }
    pub const fn chunk_reads(&self) -> u64 {
        self.chunk_reads
    }
    pub fn cached_plaintext_bytes(&self) -> usize {
        self.cache.as_ref().map_or(0, |(_, bytes)| bytes.len())
    }
    pub fn cached_chunk_count(&self) -> usize {
        usize::from(self.cache.is_some())
    }
    pub fn clear_chunk_cache(&mut self) {
        self.cache = None;
    }

    pub fn read_all(&mut self) -> Result<Vec<u8>, SegmentFileError> {
        let length =
            usize::try_from(self.length).map_err(|_| SegmentFileError::Bounds("read all"))?;
        if length > MAX_SEGMENT_READ_ALL_BYTES {
            return Err(SegmentFileError::Bounds("read all"));
        }
        let mut bytes = Vec::with_capacity(length);
        while bytes.len() < length {
            let count = (length - bytes.len()).min(MAX_RANGE_BYTES);
            bytes.extend_from_slice(&self.read_range(bytes.len() as u64, count)?);
        }
        Ok(bytes)
    }

    pub fn read_range(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, SegmentFileError> {
        if length > MAX_RANGE_BYTES {
            return Err(SegmentFileError::Bounds("read range"));
        }
        let end = offset
            .checked_add(length as u64)
            .filter(|end| *end <= self.length)
            .ok_or(SegmentFileError::Corrupt("read range"))?;
        if self.file.metadata()?.len() != self.file_length {
            return Err(SegmentFileError::Corrupt("file length changed"));
        }
        let mut result = Vec::with_capacity(length);
        let mut position = offset;
        while position < end {
            let ordinal = position / u64::from(SEGMENT_CHUNK_BYTES);
            if self
                .cache
                .as_ref()
                .is_none_or(|(cached, _)| *cached != ordinal)
            {
                let block_start = ordinal * u64::from(SEGMENT_CHUNK_BYTES);
                let count =
                    (self.length - block_start).min(u64::from(SEGMENT_CHUNK_BYTES)) as usize;
                let physical = self.start
                    + SEGMENT_HEADER_BYTES
                    + ordinal * (u64::from(SEGMENT_CHUNK_BYTES) + SEGMENT_CHECKSUM_BYTES);
                self.file.seek(SeekFrom::Start(physical))?;
                let mut bytes = vec![0; count];
                let mut checksum = [0_u8; 32];
                self.file.read_exact(&mut bytes)?;
                self.file.read_exact(&mut checksum)?;
                self.chunk_reads = self.chunk_reads.saturating_add(1);
                if Sha256::digest(&bytes).as_slice() != checksum {
                    return Err(SegmentFileError::Corrupt("block checksum"));
                }
                self.cache = Some((ordinal, bytes));
            }
            let (_, block) = self
                .cache
                .as_ref()
                .ok_or(SegmentFileError::Corrupt("block cache"))?;
            let within = (position % u64::from(SEGMENT_CHUNK_BYTES)) as usize;
            let count = (end - position).min((block.len() - within) as u64) as usize;
            result.extend_from_slice(&block[within..within + count]);
            position += count as u64;
        }
        Ok(result)
    }
}

fn physical_bytes(length: u64) -> Result<u64, SegmentFileError> {
    let blocks = length.div_ceil(u64::from(SEGMENT_CHUNK_BYTES));
    blocks
        .checked_mul(SEGMENT_CHECKSUM_BYTES)
        .and_then(|bytes| bytes.checked_add(length))
        .and_then(|bytes| bytes.checked_add(SEGMENT_HEADER_BYTES))
        .ok_or(SegmentFileError::Bounds("physical length"))
}

fn encode_header(generation: [u8; 32], role: u32, length: u64) -> [u8; 96] {
    let mut header = [0_u8; 96];
    header[..8].copy_from_slice(MAGIC);
    header[8..10].copy_from_slice(&1_u16.to_le_bytes());
    header[10..12].copy_from_slice(&96_u16.to_le_bytes());
    header[12..44].copy_from_slice(&generation);
    header[44..48].copy_from_slice(&role.to_le_bytes());
    header[48..56].copy_from_slice(&length.to_le_bytes());
    header[56..60].copy_from_slice(&SEGMENT_CHUNK_BYTES.to_le_bytes());
    let checksum = Sha256::digest(&header[..64]);
    header[64..].copy_from_slice(&checksum);
    header
}

fn decode_header(
    header: &[u8; 96],
    generation: [u8; 32],
    role: u32,
) -> Result<u64, SegmentFileError> {
    let length = u64::from_le_bytes(
        header[48..56]
            .try_into()
            .map_err(|_| SegmentFileError::Corrupt("header length"))?,
    );
    if length > MAX_FILE_BYTES {
        return Err(SegmentFileError::Bounds("file length"));
    }
    if header != &encode_header(generation, role, length) {
        return Err(SegmentFileError::Corrupt(
            "header identity, format or checksum",
        ));
    }
    Ok(length)
}

#[cfg(test)]
mod tests;
