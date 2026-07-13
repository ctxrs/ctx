use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(any(unix, windows)))]
use sha2::{Digest, Sha256};

pub const SQLITE_GENERATION_MAX_ATTEMPTS: usize = 3;
pub const SQLITE_SNAPSHOT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SQLITE_HEADER_BYTES: usize = 100;
const WAL_HEADER_BYTES: usize = 32;
const WAL_FRAME_HEADER_BYTES: usize = 24;
const WAL_FORMAT_VERSION: u32 = 3_007_000;
const JOURNAL_SENTINEL_BYTES: usize = 64;
const MAX_SUPER_JOURNAL_NAME_BYTES: u64 = 64 * 1024;
pub(super) const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
const WAL_CHURN_REASON: &str = "SQLite WAL has an incomplete or changing valid generation";
const WAL_RESOURCE_REASON: &str = "SQLite WAL valid prefix exceeds the snapshot resource ceiling";
const SUPER_JOURNAL_REASON: &str =
    "SQLite rollback journal belongs to an unsupported multi-database transaction";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteObservedFile {
    path: PathBuf,
    len: u64,
    modified_at: SystemTime,
    modified_secs: u64,
    modified_nanos: u32,
    sentinel: Vec<u8>,
    snapshot_relevant: bool,
    snapshot_len: u64,
    deferred_reason: Option<&'static str>,
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

    pub(crate) fn snapshot_len(&self) -> u64 {
        self.snapshot_len
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

    pub(crate) fn deferred_reason(&self) -> Option<&'static str> {
        self.wal
            .iter()
            .chain(self.journal.iter())
            .find_map(|file| file.deferred_reason)
    }
}

pub fn observe_sqlite_source_generation(path: &Path) -> io::Result<SqliteSourceGeneration> {
    let mut retryable_error = None;
    for _ in 0..SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = match observe_generation_once(path) {
            Ok(generation) => generation,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                retryable_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        let after = match observe_generation_once(path) {
            Ok(generation) => generation,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                retryable_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        if before == after {
            if after.deferred_reason() == Some(WAL_CHURN_REASON) {
                retryable_error = Some(io::Error::new(io::ErrorKind::WouldBlock, WAL_CHURN_REASON));
                continue;
            }
            return Ok(after);
        }
    }
    if let Some(error) = retryable_error {
        return Err(error);
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
    let (sentinel, snapshot_relevant, snapshot_len, deferred_reason) = match kind {
        SentinelKind::Main => (
            main_header_sentinel(&mut file, &metadata)?,
            false,
            metadata.len(),
            None,
        ),
        SentinelKind::Wal => wal_sentinel(&mut file, metadata.len())?,
        SentinelKind::Journal => journal_sentinel(path, &mut file, metadata.len())?,
    };
    Ok(SqliteObservedFile {
        path: path.to_path_buf(),
        len: metadata.len(),
        modified_at,
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        sentinel,
        snapshot_relevant,
        snapshot_len,
        deferred_reason,
    })
}

fn main_header_sentinel(file: &mut File, metadata: &Metadata) -> io::Result<Vec<u8>> {
    let header = read_prefix(file, SQLITE_HEADER_BYTES)?;
    let mut sentinel = b"sqlite-main-v2".to_vec();
    if header.starts_with(b"SQLite format 3\0") && header.len() >= SQLITE_HEADER_BYTES {
        for range in [24..32, 40..48, 60..64, 92..100] {
            sentinel.extend_from_slice(&header[range]);
        }
    } else {
        sentinel.extend_from_slice(&header);
    }
    append_main_file_identity(&mut sentinel, file, metadata)?;
    Ok(sentinel)
}

#[cfg(unix)]
fn append_main_file_identity(
    sentinel: &mut Vec<u8>,
    _file: &File,
    metadata: &Metadata,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    sentinel.extend_from_slice(b"unix-file-id-v1");
    sentinel.extend_from_slice(&metadata.dev().to_le_bytes());
    sentinel.extend_from_slice(&metadata.ino().to_le_bytes());
    sentinel.extend_from_slice(&metadata.ctime().to_le_bytes());
    sentinel.extend_from_slice(&metadata.ctime_nsec().to_le_bytes());
    Ok(())
}

#[cfg(windows)]
fn append_main_file_identity(
    sentinel: &mut Vec<u8>,
    file: &File,
    _metadata: &Metadata,
) -> io::Result<()> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut id = MaybeUninit::<FILE_ID_INFO>::zeroed();
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            id.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::zeroed();
    let basic_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let id = unsafe { id.assume_init() };
    let basic = unsafe { basic.assume_init() };
    sentinel.extend_from_slice(b"windows-file-id-v1");
    sentinel.extend_from_slice(&id.VolumeSerialNumber.to_le_bytes());
    sentinel.extend_from_slice(&id.FileId.Identifier);
    sentinel.extend_from_slice(&basic.ChangeTime.to_le_bytes());
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn append_main_file_identity(
    sentinel: &mut Vec<u8>,
    file: &File,
    _metadata: &Metadata,
) -> io::Result<()> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    sentinel.extend_from_slice(b"full-sha256-fallback-v1");
    sentinel.extend_from_slice(&hasher.finalize());
    Ok(())
}

fn wal_sentinel(
    file: &mut File,
    len: u64,
) -> io::Result<(Vec<u8>, bool, u64, Option<&'static str>)> {
    let header = read_prefix(file, WAL_HEADER_BYTES)?;
    let mut sentinel = b"sqlite-wal-v2".to_vec();
    sentinel.extend_from_slice(&header);
    let wal_header = match parse_wal_header(&header) {
        WalHeaderState::Valid(header) => header,
        WalHeaderState::Ignore => return Ok((sentinel, false, 0, None)),
        WalHeaderState::Defer => return Ok((sentinel, false, 0, Some(WAL_CHURN_REASON))),
    };
    let frame_size = u64::from(wal_header.page_size) + WAL_FRAME_HEADER_BYTES as u64;
    let physical_frames = len.saturating_sub(WAL_HEADER_BYTES as u64) / frame_size;
    let trailing_bytes = len.saturating_sub(WAL_HEADER_BYTES as u64) % frame_size;
    let mut checksum = wal_header.checksum;
    let mut page = vec![0_u8; wal_header.page_size as usize];
    let mut valid_frames = 0_u64;
    let mut last_commit = None;
    let mut stale_suffix = false;
    let mut churning_suffix = false;
    for frame in 1..=physical_frames {
        let offset = wal_frame_offset(frame, frame_size)?;
        let frame_header = read_wal_frame_header(file, offset)?;
        if frame_header[8..16] != wal_header.salts {
            stale_suffix = true;
            break;
        }
        if let Err(error) = wal_frame_end_within_snapshot_ceiling(offset, frame_size) {
            return Err(error);
        }
        file.read_exact(&mut page)?;
        if be_u32(&frame_header[0..4]) == 0 {
            churning_suffix = true;
            break;
        }
        checksum = wal_checksum(wal_header.checksum_order, &frame_header[..8], checksum);
        checksum = wal_checksum(wal_header.checksum_order, &page, checksum);
        if checksum != [be_u32(&frame_header[16..20]), be_u32(&frame_header[20..24])] {
            churning_suffix = true;
            break;
        }
        valid_frames = frame;
        if be_u32(&frame_header[4..8]) != 0 {
            last_commit = Some((frame, checksum));
        }
    }

    sentinel.extend_from_slice(&valid_frames.to_le_bytes());
    if churning_suffix || (trailing_bytes != 0 && !stale_suffix) {
        return Ok((sentinel, false, 0, Some(WAL_CHURN_REASON)));
    }
    if let Some((frame, checksum)) = last_commit {
        sentinel.extend_from_slice(&frame.to_le_bytes());
        sentinel.extend_from_slice(&checksum[0].to_le_bytes());
        sentinel.extend_from_slice(&checksum[1].to_le_bytes());
        let committed_len = wal_frame_offset(frame + 1, frame_size)?;
        return Ok((sentinel, true, committed_len, None));
    }
    Ok((sentinel, false, 0, None))
}

#[derive(Clone, Copy)]
struct WalHeader {
    page_size: u32,
    salts: [u8; 8],
    checksum_order: WalChecksumOrder,
    checksum: [u32; 2],
}

enum WalHeaderState {
    Valid(WalHeader),
    Ignore,
    Defer,
}

#[derive(Clone, Copy)]
enum WalChecksumOrder {
    LittleEndian,
    BigEndian,
}

fn parse_wal_header(header: &[u8]) -> WalHeaderState {
    if header.is_empty() {
        return WalHeaderState::Ignore;
    }
    if header.len() >= 4 && !matches!(be_u32(&header[0..4]), 0x377f_0682 | 0x377f_0683) {
        return WalHeaderState::Ignore;
    }
    if header.len() < WAL_HEADER_BYTES {
        return WalHeaderState::Defer;
    }
    let checksum_order = match be_u32(&header[0..4]) {
        0x377f_0682 => WalChecksumOrder::LittleEndian,
        0x377f_0683 => WalChecksumOrder::BigEndian,
        _ => return WalHeaderState::Ignore,
    };
    if be_u32(&header[4..8]) != WAL_FORMAT_VERSION {
        return WalHeaderState::Defer;
    }
    let page_size = be_u32(&header[8..12]);
    if !page_size.is_power_of_two() || !(512..=65_536).contains(&page_size) {
        return WalHeaderState::Ignore;
    }
    let checksum = wal_checksum(checksum_order, &header[..24], [0, 0]);
    if checksum != [be_u32(&header[24..28]), be_u32(&header[28..32])] {
        return WalHeaderState::Defer;
    }
    WalHeaderState::Valid(WalHeader {
        page_size,
        salts: header[16..24].try_into().unwrap_or_default(),
        checksum_order,
        checksum,
    })
}

fn wal_frame_offset(frame: u64, frame_size: u64) -> io::Result<u64> {
    (WAL_HEADER_BYTES as u64)
        .checked_add(
            frame
                .saturating_sub(1)
                .checked_mul(frame_size)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "SQLite WAL frame offset overflow",
                    )
                })?,
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "SQLite WAL frame offset overflow",
            )
        })
}

fn wal_frame_end_within_snapshot_ceiling(offset: u64, frame_size: u64) -> io::Result<u64> {
    let frame_end = offset
        .checked_add(frame_size)
        .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, WAL_RESOURCE_REASON))?;
    if frame_end > SQLITE_SNAPSHOT_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            WAL_RESOURCE_REASON,
        ));
    }
    Ok(frame_end)
}

fn read_wal_frame_header(file: &mut File, offset: u64) -> io::Result<[u8; 24]> {
    let mut header = [0_u8; WAL_FRAME_HEADER_BYTES];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut header)?;
    Ok(header)
}

fn wal_checksum(order: WalChecksumOrder, bytes: &[u8], initial: [u32; 2]) -> [u32; 2] {
    debug_assert_eq!(bytes.len() % 8, 0);
    let mut s1 = initial[0];
    let mut s2 = initial[1];
    for words in bytes.chunks_exact(8) {
        let first = match order {
            WalChecksumOrder::LittleEndian => {
                u32::from_le_bytes(words[0..4].try_into().unwrap_or_default())
            }
            WalChecksumOrder::BigEndian => {
                u32::from_be_bytes(words[0..4].try_into().unwrap_or_default())
            }
        };
        let second = match order {
            WalChecksumOrder::LittleEndian => {
                u32::from_le_bytes(words[4..8].try_into().unwrap_or_default())
            }
            WalChecksumOrder::BigEndian => {
                u32::from_be_bytes(words[4..8].try_into().unwrap_or_default())
            }
        };
        s1 = s1.wrapping_add(first).wrapping_add(s2);
        s2 = s2.wrapping_add(second).wrapping_add(s1);
    }
    [s1, s2]
}

fn journal_sentinel(
    journal_path: &Path,
    file: &mut File,
    len: u64,
) -> io::Result<(Vec<u8>, bool, u64, Option<&'static str>)> {
    let prefix = read_prefix(file, JOURNAL_SENTINEL_BYTES)?;
    let mut sentinel = b"sqlite-journal-v2".to_vec();
    sentinel.extend_from_slice(&prefix);
    sentinel.extend_from_slice(&read_tail(file, len, JOURNAL_SENTINEL_BYTES)?);
    let hot = hot_journal_header(&prefix, len);
    if hot {
        if let Some(super_journal) = super_journal_path(journal_path, file, len)? {
            match super_journal.try_exists() {
                Ok(true) => {
                    sentinel.extend_from_slice(b"super-journal-present");
                    return Ok((sentinel, false, 0, Some(SUPER_JOURNAL_REASON)));
                }
                Ok(false) => {
                    sentinel.extend_from_slice(b"super-journal-missing");
                    return Ok((sentinel, false, 0, None));
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "SQLite super-journal presence could not be established",
                    ));
                }
            }
        }
    }
    Ok((sentinel, hot, if hot { len } else { 0 }, None))
}

fn hot_journal_header(prefix: &[u8], len: u64) -> bool {
    if len <= 512 || prefix.len() < 28 || !prefix.starts_with(&JOURNAL_MAGIC) {
        return false;
    }
    let sector_size = be_u32(&prefix[20..24]);
    let page_size = be_u32(&prefix[24..28]);
    sector_size.is_power_of_two()
        && (512..=65_536).contains(&sector_size)
        && page_size.is_power_of_two()
        && (512..=65_536).contains(&page_size)
        && len >= u64::from(sector_size)
}

fn super_journal_path(
    journal_path: &Path,
    file: &mut File,
    len: u64,
) -> io::Result<Option<PathBuf>> {
    if len < 16 {
        return Ok(None);
    }
    let trailer = read_at(file, len - 16, 16)?;
    if trailer[8..16] != JOURNAL_MAGIC {
        return Ok(None);
    }
    let name_len = u64::from(be_u32(&trailer[0..4]));
    if name_len == 0 || name_len > MAX_SUPER_JOURNAL_NAME_BYTES || name_len > len.saturating_sub(16)
    {
        return Ok(None);
    }
    let name = read_at(file, len - 16 - name_len, name_len as usize)?;
    let expected = be_u32(&trailer[4..8]);
    let actual = name.iter().fold(0_u32, |sum, byte| {
        sum.wrapping_add((*byte as i8 as i32) as u32)
    });
    if actual != expected || name.contains(&0) {
        return Ok(None);
    }
    let path = native_super_journal_path(name)?;
    if path.is_absolute() {
        Ok(Some(path))
    } else {
        Ok(Some(
            journal_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(path),
        ))
    }
}

#[cfg(unix)]
fn native_super_journal_path(name: Vec<u8>) -> io::Result<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    Ok(PathBuf::from(OsString::from_vec(name)))
}

#[cfg(windows)]
fn native_super_journal_path(name: Vec<u8>) -> io::Result<PathBuf> {
    let name = std::str::from_utf8(&name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "SQLite super-journal path is not valid native UTF-8",
        )
    })?;
    Ok(PathBuf::from(name))
}

#[cfg(not(any(unix, windows)))]
fn native_super_journal_path(name: Vec<u8>) -> io::Result<PathBuf> {
    let name = std::str::from_utf8(&name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "SQLite super-journal path is not valid native UTF-8",
        )
    })?;
    Ok(PathBuf::from(name))
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

fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0_u8; len];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap_or_default())
}

pub(crate) fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(test)]
mod tests {
    use std::{fs, fs::FileTimes, thread, time::Duration};

    use rusqlite::Connection;

    use super::*;

    #[test]
    fn real_wal_validates_supported_page_sizes_and_both_checksum_orders() {
        for page_size in [512_u32, 65_536] {
            let fixture = real_wal_fixture(page_size);
            let generation = observe_sqlite_source_generation(&fixture.db).unwrap();
            assert!(generation.requires_snapshot(), "page size {page_size}");
            let wal = generation.wal.as_ref().unwrap();
            assert!(wal.snapshot_len() <= wal.len());

            let alternate = fixture
                .temp
                .path()
                .join(format!("alternate-{page_size}.db"));
            fs::copy(&fixture.db, &alternate).unwrap();
            let mut wal_bytes = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
            let order = match be_u32(&wal_bytes[0..4]) {
                0x377f_0682 => WalChecksumOrder::BigEndian,
                0x377f_0683 => WalChecksumOrder::LittleEndian,
                magic => panic!("unexpected SQLite WAL magic {magic:#x}"),
            };
            rewrite_wal_checksum_order(&mut wal_bytes, order);
            fs::write(sidecar_path(&alternate, "-wal"), wal_bytes).unwrap();
            assert!(
                observe_sqlite_source_generation(&alternate)
                    .unwrap()
                    .requires_snapshot(),
                "alternate checksum order for page size {page_size}"
            );
        }
    }

    #[test]
    fn real_wal_rejects_bad_header_checksum_salt_frame_checksum_and_partial_commit() {
        let fixture = real_wal_fixture(512);
        let original = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        assert!(original.len() > WAL_HEADER_BYTES + WAL_FRAME_HEADER_BYTES);

        let corruptions: [(&str, fn(&mut Vec<u8>)); 4] = [
            ("header-checksum", |bytes: &mut Vec<u8>| bytes[24] ^= 0x01),
            ("salt", |bytes: &mut Vec<u8>| bytes[40] ^= 0x01),
            ("frame-checksum", |bytes: &mut Vec<u8>| bytes[48] ^= 0x01),
            ("partial-frame", |bytes: &mut Vec<u8>| {
                bytes.pop();
            }),
        ];
        for (label, mutate) in corruptions {
            let db = fixture.temp.path().join(format!("bad-{label}.db"));
            fs::copy(&fixture.db, &db).unwrap();
            let mut bytes = original.clone();
            mutate(&mut bytes);
            fs::write(sidecar_path(&db, "-wal"), bytes).unwrap();
            if label == "salt" {
                let generation = observe_sqlite_source_generation(&db).unwrap();
                assert!(!generation.requires_snapshot(), "{label}");
                assert!(generation.deferred_reason().is_none(), "{label}");
            } else {
                let error = observe_sqlite_source_generation(&db).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{label}");
            }
        }
    }

    #[test]
    fn wal_page_size_one_is_invalid() {
        let fixture = real_wal_fixture(512);
        let db = fixture.temp.path().join("page-size-one.db");
        fs::copy(&fixture.db, &db).unwrap();
        let mut wal = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        wal[8..12].copy_from_slice(&1_u32.to_be_bytes());
        let order = match be_u32(&wal[0..4]) {
            0x377f_0682 => WalChecksumOrder::LittleEndian,
            0x377f_0683 => WalChecksumOrder::BigEndian,
            _ => unreachable!(),
        };
        let checksum = wal_checksum(order, &wal[..24], [0, 0]);
        wal[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        wal[28..32].copy_from_slice(&checksum[1].to_be_bytes());
        fs::write(sidecar_path(&db, "-wal"), wal).unwrap();

        assert!(!observe_sqlite_source_generation(&db)
            .unwrap()
            .requires_snapshot());
    }

    #[test]
    fn wal_valid_prefix_ceiling_is_retryable() {
        let error =
            wal_frame_end_within_snapshot_ceiling(SQLITE_SNAPSHOT_MAX_BYTES, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(error.to_string(), WAL_RESOURCE_REASON);
    }

    #[test]
    fn wal_restart_ignores_stale_physical_frames_after_the_valid_prefix() {
        let fixture = real_wal_fixture(512);
        fixture
            .writer
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE extra (id INTEGER PRIMARY KEY, value BLOB);
                 INSERT INTO extra(value) VALUES (zeroblob(262144));
                 COMMIT;",
            )
            .unwrap();
        let wal_path = sidecar_path(&fixture.db, "-wal");
        let old_wal = fs::read(&wal_path).unwrap();
        fixture
            .writer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        fixture
            .writer
            .execute("UPDATE entries SET value = 'sigma' WHERE id = 1", [])
            .unwrap();
        let restarted_wal = fs::read(&wal_path).unwrap();
        assert!(restarted_wal.len() < old_wal.len());
        let mut reused_wal = restarted_wal.clone();
        reused_wal.extend_from_slice(&old_wal[restarted_wal.len()..]);
        fs::write(&wal_path, reused_wal).unwrap();

        let generation = observe_sqlite_source_generation(&fixture.db).unwrap();
        let wal = generation.wal.as_ref().unwrap();
        assert!(generation.requires_snapshot());
        assert_eq!(wal.len(), old_wal.len() as u64);
        assert_eq!(wal.snapshot_len(), restarted_wal.len() as u64);
        assert!(wal.snapshot_len() < wal.len());
    }

    #[test]
    fn rollback_journal_modes_leave_no_hot_generation_after_commit() {
        for mode in ["DELETE", "TRUNCATE", "PERSIST"] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join(format!("{mode}.db"));
            let conn = Connection::open(&db).unwrap();
            let actual: String = conn
                .query_row(&format!("PRAGMA journal_mode = {mode}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(actual.to_uppercase(), mode);
            conn.execute_batch(
                "CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'committed');",
            )
            .unwrap();

            let generation = observe_sqlite_source_generation(&db).unwrap();
            assert!(!generation.requires_snapshot(), "journal mode {mode}");
            assert!(
                generation.deferred_reason().is_none(),
                "journal mode {mode}"
            );
        }
    }

    #[test]
    fn super_journal_presence_controls_hot_child_journal_state() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("attached-main.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let super_journal_name = b"attached-main.db-mj H8a1";
        let super_journal = temp.path().join("attached-main.db-mj H8a1");
        fs::write(&super_journal, b"active multi-database commit").unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, super_journal_name);
        fs::write(&journal, bytes).unwrap();

        let present = observe_sqlite_source_generation(&db).unwrap();
        assert!(!present.requires_snapshot());
        assert_eq!(present.deferred_reason(), Some(SUPER_JOURNAL_REASON));

        fs::remove_file(super_journal).unwrap();
        let missing = observe_sqlite_source_generation(&db).unwrap();
        assert!(!missing.requires_snapshot());
        assert!(missing.deferred_reason().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn super_journal_uses_non_utf8_native_relative_path_without_loss() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("attached-main.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let name = b"attached-main.db-mj-\x80";
        fs::write(
            temp.path().join(OsString::from_vec(name.to_vec())),
            b"active",
        )
        .unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, name);
        fs::write(journal, bytes).unwrap();

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert_eq!(generation.deferred_reason(), Some(SUPER_JOURNAL_REASON));
    }

    #[test]
    fn same_stat_checkpointed_update_changes_main_file_identity() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("same-stat.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
             INSERT INTO entries VALUES (1, 'alpha');
             PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
        let before_metadata = fs::metadata(&db).unwrap();
        let before_modified = before_metadata.modified().unwrap();
        let before_header = fs::read(&db).unwrap()[..SQLITE_HEADER_BYTES].to_vec();
        let before = observe_sqlite_source_generation(&db).unwrap();
        thread::sleep(Duration::from_millis(2));

        conn.execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        File::options()
            .write(true)
            .open(&db)
            .unwrap()
            .set_times(FileTimes::new().set_modified(before_modified))
            .unwrap();

        let after_metadata = fs::metadata(&db).unwrap();
        assert_eq!(after_metadata.len(), before_metadata.len());
        assert_eq!(after_metadata.modified().unwrap(), before_modified);
        assert_eq!(
            &fs::read(&db).unwrap()[..SQLITE_HEADER_BYTES],
            before_header
        );
        let after = observe_sqlite_source_generation(&db).unwrap();
        assert_ne!(before.main().sentinel(), after.main().sentinel());
    }

    struct WalFixture {
        temp: tempfile::TempDir,
        db: PathBuf,
        writer: Connection,
    }

    fn real_wal_fixture(page_size: u32) -> WalFixture {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(format!("wal-{page_size}.db"));
        let writer = Connection::open(&db).unwrap();
        writer
            .execute_batch(&format!(
                "PRAGMA page_size = {page_size};
                 VACUUM;
                 CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 PRAGMA wal_checkpoint(TRUNCATE);"
            ))
            .unwrap();
        writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        assert!(sidecar_path(&db, "-wal").is_file());
        WalFixture { temp, db, writer }
    }

    fn rewrite_wal_checksum_order(bytes: &mut [u8], order: WalChecksumOrder) {
        let magic = match order {
            WalChecksumOrder::LittleEndian => 0x377f_0682_u32,
            WalChecksumOrder::BigEndian => 0x377f_0683_u32,
        };
        bytes[0..4].copy_from_slice(&magic.to_be_bytes());
        let mut checksum = wal_checksum(order, &bytes[..24], [0, 0]);
        bytes[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        bytes[28..32].copy_from_slice(&checksum[1].to_be_bytes());
        let page_size = be_u32(&bytes[8..12]) as usize;
        let frame_size = WAL_FRAME_HEADER_BYTES + page_size;
        for frame in bytes[WAL_HEADER_BYTES..].chunks_exact_mut(frame_size) {
            checksum = wal_checksum(order, &frame[..8], checksum);
            checksum = wal_checksum(order, &frame[WAL_FRAME_HEADER_BYTES..], checksum);
            frame[16..20].copy_from_slice(&checksum[0].to_be_bytes());
            frame[20..24].copy_from_slice(&checksum[1].to_be_bytes());
        }
    }

    fn real_hot_journal_bytes(page_size: u32) -> Vec<u8> {
        let sector_size = 512_u32;
        let mut bytes = vec![0_u8; sector_size as usize + page_size as usize + 8];
        bytes[..8].copy_from_slice(&JOURNAL_MAGIC);
        bytes[8..12].copy_from_slice(&1_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x1234_5678_u32.to_be_bytes());
        bytes[16..20].copy_from_slice(&1_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&sector_size.to_be_bytes());
        bytes[24..28].copy_from_slice(&page_size.to_be_bytes());
        bytes[sector_size as usize..sector_size as usize + 4].copy_from_slice(&1_u32.to_be_bytes());
        bytes
    }

    fn append_super_journal_trailer(journal: &mut Vec<u8>, name: &[u8]) {
        journal.extend_from_slice(&1_048_577_u32.to_be_bytes());
        journal.extend_from_slice(name);
        journal.extend_from_slice(&(name.len() as u32).to_be_bytes());
        let checksum = name.iter().fold(0_u32, |sum, byte| {
            sum.wrapping_add((*byte as i8 as i32) as u32)
        });
        journal.extend_from_slice(&checksum.to_be_bytes());
        journal.extend_from_slice(&JOURNAL_MAGIC);
    }
}
