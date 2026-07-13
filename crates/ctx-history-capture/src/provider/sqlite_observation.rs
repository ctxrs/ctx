use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const SQLITE_GENERATION_MAX_ATTEMPTS: usize = 3;
const SQLITE_HEADER_BYTES: usize = 100;
const WAL_HEADER_BYTES: usize = 32;
const WAL_FRAME_HEADER_BYTES: usize = 24;
const WAL_COMMIT_SCAN_FRAMES: u64 = 64;
const JOURNAL_SENTINEL_BYTES: usize = 64;
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteObservedFile {
    path: PathBuf,
    len: u64,
    modified_at: SystemTime,
    modified_secs: u64,
    modified_nanos: u32,
    sentinel: Vec<u8>,
    snapshot_relevant: bool,
}

impl SqliteObservedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    pub fn modified_secs(&self) -> u64 {
        self.modified_secs
    }

    pub fn modified_nanos(&self) -> u32 {
        self.modified_nanos
    }

    pub fn sentinel(&self) -> &[u8] {
        &self.sentinel
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSourceGeneration {
    main: SqliteObservedFile,
    wal: Option<SqliteObservedFile>,
    journal: Option<SqliteObservedFile>,
}

impl SqliteSourceGeneration {
    pub fn main(&self) -> &SqliteObservedFile {
        &self.main
    }

    pub fn files(&self) -> Vec<&SqliteObservedFile> {
        let mut files = vec![&self.main];
        files.extend(self.wal.iter());
        files.extend(self.journal.iter());
        files
    }

    pub(crate) fn snapshot_files(&self) -> Vec<&SqliteObservedFile> {
        let mut files = vec![&self.main];
        files.extend(self.wal.iter().filter(|file| file.snapshot_relevant));
        files.extend(self.journal.iter().filter(|file| file.snapshot_relevant));
        files
    }

    pub(crate) fn requires_snapshot(&self) -> bool {
        self.wal
            .iter()
            .chain(self.journal.iter())
            .any(|file| file.snapshot_relevant)
    }
}

pub fn observe_sqlite_source_generation(path: &Path) -> io::Result<SqliteSourceGeneration> {
    for _ in 0..SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = observe_generation_once(path)?;
        let after = observe_generation_once(path)?;
        if before == after {
            return Ok(after);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!(
            "SQLite source generation kept changing while observing {}",
            path.display()
        ),
    ))
}

fn observe_generation_once(path: &Path) -> io::Result<SqliteSourceGeneration> {
    Ok(SqliteSourceGeneration {
        main: observe_required_file(path, SentinelKind::Main)?,
        wal: observe_optional_file(&sidecar_path(path, "-wal"), SentinelKind::Wal)?,
        journal: observe_optional_file(&sidecar_path(path, "-journal"), SentinelKind::Journal)?,
    })
}

#[derive(Clone, Copy)]
enum SentinelKind {
    Main,
    Wal,
    Journal,
}

fn observe_required_file(path: &Path, kind: SentinelKind) -> io::Result<SqliteObservedFile> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite source is not a regular file: {}", path.display()),
        ));
    }
    observe_file(path, metadata, kind)
}

fn observe_optional_file(
    path: &Path,
    kind: SentinelKind,
) -> io::Result<Option<SqliteObservedFile>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite sidecar is not a regular file: {}", path.display()),
        ));
    }
    match observe_file(path, metadata, kind) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn observe_file(
    path: &Path,
    metadata: Metadata,
    kind: SentinelKind,
) -> io::Result<SqliteObservedFile> {
    let modified_at = metadata.modified().unwrap_or(UNIX_EPOCH);
    let modified = modified_at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let mut file = File::open(path)?;
    let (sentinel, snapshot_relevant) = match kind {
        SentinelKind::Main => (main_header_sentinel(&mut file)?, false),
        SentinelKind::Wal => wal_sentinel(&mut file, metadata.len())?,
        SentinelKind::Journal => journal_sentinel(&mut file, metadata.len())?,
    };
    Ok(SqliteObservedFile {
        path: path.to_path_buf(),
        len: metadata.len(),
        modified_at,
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        sentinel,
        snapshot_relevant,
    })
}

fn main_header_sentinel(file: &mut File) -> io::Result<Vec<u8>> {
    let header = read_prefix(file, SQLITE_HEADER_BYTES)?;
    let mut sentinel = b"sqlite-main-v1".to_vec();
    if header.starts_with(b"SQLite format 3\0") && header.len() >= SQLITE_HEADER_BYTES {
        for range in [24..32, 40..48, 60..64, 92..100] {
            sentinel.extend_from_slice(&header[range]);
        }
    } else {
        sentinel.extend_from_slice(&header);
    }
    Ok(sentinel)
}

fn wal_sentinel(file: &mut File, len: u64) -> io::Result<(Vec<u8>, bool)> {
    let header = read_prefix(file, WAL_HEADER_BYTES)?;
    let mut sentinel = b"sqlite-wal-v1".to_vec();
    sentinel.extend_from_slice(&header);
    let Some(page_size) = wal_page_size(&header) else {
        sentinel.extend_from_slice(&read_tail(file, len, JOURNAL_SENTINEL_BYTES)?);
        return Ok((sentinel, false));
    };
    let frame_size = u64::from(page_size) + WAL_FRAME_HEADER_BYTES as u64;
    let frame_count = len.saturating_sub(WAL_HEADER_BYTES as u64) / frame_size;
    sentinel.extend_from_slice(&frame_count.to_le_bytes());
    if frame_count == 0 {
        return Ok((sentinel, false));
    }

    let last_header = read_wal_frame_header(file, frame_count, frame_size)?;
    sentinel.extend_from_slice(&last_header);
    let first_scanned = frame_count
        .saturating_sub(WAL_COMMIT_SCAN_FRAMES)
        .saturating_add(1);
    let mut committed = None;
    for frame in (first_scanned..=frame_count).rev() {
        let frame_header = read_wal_frame_header(file, frame, frame_size)?;
        if u32::from_be_bytes(frame_header[4..8].try_into().unwrap_or_default()) != 0 {
            committed = Some((frame, frame_header));
            break;
        }
    }
    // A bounded scan proves the common no-commit case cheaply. If a WAL has
    // more frames than the scan window, preserve correctness by letting SQLite
    // decide which frames are durable in the temporary snapshot.
    let snapshot_relevant = committed.is_some() || frame_count > WAL_COMMIT_SCAN_FRAMES;
    if let Some((frame, frame_header)) = committed {
        sentinel.extend_from_slice(&frame.to_le_bytes());
        sentinel.extend_from_slice(&frame_header);
    }
    Ok((sentinel, snapshot_relevant))
}

fn wal_page_size(header: &[u8]) -> Option<u32> {
    if header.len() < WAL_HEADER_BYTES {
        return None;
    }
    let magic = u32::from_be_bytes(header[0..4].try_into().ok()?);
    if !matches!(magic, 0x377f_0682 | 0x377f_0683) {
        return None;
    }
    let raw = u32::from_be_bytes(header[8..12].try_into().ok()?);
    let page_size = if raw == 1 { 65_536 } else { raw };
    (page_size.is_power_of_two() && (512..=65_536).contains(&page_size)).then_some(page_size)
}

fn read_wal_frame_header(file: &mut File, frame: u64, frame_size: u64) -> io::Result<[u8; 24]> {
    let offset = WAL_HEADER_BYTES as u64 + frame.saturating_sub(1).saturating_mul(frame_size);
    let mut header = [0_u8; WAL_FRAME_HEADER_BYTES];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut header)?;
    Ok(header)
}

fn journal_sentinel(file: &mut File, len: u64) -> io::Result<(Vec<u8>, bool)> {
    let prefix = read_prefix(file, JOURNAL_SENTINEL_BYTES)?;
    let mut sentinel = b"sqlite-journal-v1".to_vec();
    sentinel.extend_from_slice(&prefix);
    sentinel.extend_from_slice(&read_tail(file, len, JOURNAL_SENTINEL_BYTES)?);
    let hot = len > 512 && prefix.starts_with(&JOURNAL_MAGIC);
    Ok((sentinel, hot))
}

fn read_prefix(file: &mut File, limit: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = vec![0_u8; limit];
    let read = file.read(&mut bytes)?;
    bytes.truncate(read);
    Ok(bytes)
}

fn read_tail(file: &mut File, len: u64, limit: usize) -> io::Result<Vec<u8>> {
    let count = usize::try_from(len.min(limit as u64)).unwrap_or(limit);
    file.seek(SeekFrom::Start(len.saturating_sub(count as u64)))?;
    let mut bytes = vec![0_u8; count];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}
