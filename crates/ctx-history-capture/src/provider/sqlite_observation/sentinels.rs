fn main_header_sentinel(file: &mut File, metadata: &Metadata) -> io::Result<(Vec<u8>, bool)> {
    let header = read_prefix(file, SQLITE_HEADER_BYTES)?;
    let uses_wal_mode = main_header_uses_wal_mode(&header);
    let mut sentinel = b"sqlite-main-v2".to_vec();
    if header.starts_with(b"SQLite format 3\0") && header.len() >= SQLITE_HEADER_BYTES {
        for range in [24..32, 40..48, 60..64, 92..100] {
            sentinel.extend_from_slice(&header[range]);
        }
    } else {
        sentinel.extend_from_slice(&header);
    }
    append_main_file_identity(&mut sentinel, file, metadata)?;
    Ok((sentinel, uses_wal_mode))
}

fn main_header_uses_wal_mode(header: &[u8]) -> bool {
    header.starts_with(b"SQLite format 3\0")
        && header.len() >= SQLITE_HEADER_BYTES
        && (header[18] == 2 || header[19] == 2)
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
        pace_current_disk_io(buffer.len() as u64);
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

enum WalSentinel {
    Observed(Vec<u8>, bool, u64, Option<&'static str>),
    Corrupt {
        reason: &'static str,
        fingerprint: Vec<u8>,
    },
}

fn wal_sentinel(path: &Path, file: &mut File, len: u64) -> io::Result<WalSentinel> {
    let header = read_prefix(file, WAL_HEADER_BYTES)?;
    let mut sentinel = b"sqlite-wal-v2".to_vec();
    sentinel.extend_from_slice(&header);
    let wal_header = match parse_wal_header(&header) {
        WalHeaderState::Valid(header) => header,
        WalHeaderState::Ignore => return Ok(WalSentinel::Observed(sentinel, false, 0, None)),
        WalHeaderState::Defer => {
            return Ok(WalSentinel::Observed(
                sentinel,
                false,
                0,
                Some(WAL_CHURN_REASON),
            ))
        }
        WalHeaderState::Corrupt(reason) => {
            return Ok(WalSentinel::Corrupt {
                reason,
                fingerprint: sentinel,
            })
        }
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
    let mut corrupt_suffix = None;
    for frame in 1..=physical_frames {
        let offset = wal_frame_offset(frame, frame_size)?;
        run_observation_test_hook(path, SqliteObservationTestPhase::BeforeWalFrameRead);
        let frame_header = read_wal_frame_header(file, offset)?;
        if frame_header[8..16] != wal_header.salts {
            stale_suffix = true;
            break;
        }
        if wal_frame_end_within_snapshot_ceiling(offset, frame_size).is_err() {
            sentinel.extend_from_slice(&valid_frames.to_le_bytes());
            return Ok(WalSentinel::Observed(
                sentinel,
                false,
                0,
                Some(WAL_RESOURCE_REASON),
            ));
        }
        read_exact_paced(file, &mut page)?;
        if be_u32(&frame_header[0..4]) == 0 {
            churning_suffix = true;
            break;
        }
        checksum = wal_checksum(wal_header.checksum_order, &frame_header[..8], checksum);
        checksum = wal_checksum(wal_header.checksum_order, &page, checksum);
        if checksum != [be_u32(&frame_header[16..20]), be_u32(&frame_header[20..24])] {
            let mut fingerprint = sentinel.clone();
            fingerprint.extend_from_slice(&frame.to_le_bytes());
            fingerprint.extend_from_slice(&frame_header);
            fingerprint.extend_from_slice(&checksum[0].to_le_bytes());
            fingerprint.extend_from_slice(&checksum[1].to_le_bytes());
            corrupt_suffix = Some(fingerprint);
            break;
        }
        valid_frames = frame;
        if be_u32(&frame_header[4..8]) != 0 {
            last_commit = Some((frame, checksum));
        }
    }

    sentinel.extend_from_slice(&valid_frames.to_le_bytes());
    if let Some((frame, checksum)) = last_commit {
        sentinel.extend_from_slice(&frame.to_le_bytes());
        sentinel.extend_from_slice(&checksum[0].to_le_bytes());
        sentinel.extend_from_slice(&checksum[1].to_le_bytes());
        let committed_len = wal_frame_offset(frame + 1, frame_size)?;
        return Ok(WalSentinel::Observed(sentinel, true, committed_len, None));
    }
    if let Some(fingerprint) = corrupt_suffix {
        return Ok(WalSentinel::Corrupt {
            reason: WAL_FRAME_CHECKSUM_REASON,
            fingerprint,
        });
    }
    if churning_suffix || (trailing_bytes != 0 && !stale_suffix) {
        return Ok(WalSentinel::Observed(
            sentinel,
            false,
            0,
            Some(WAL_CHURN_REASON),
        ));
    }
    Ok(WalSentinel::Observed(sentinel, false, 0, None))
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
    Corrupt(&'static str),
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
        return WalHeaderState::Corrupt(WAL_FORMAT_VERSION_REASON);
    }
    let page_size = be_u32(&header[8..12]);
    if !page_size.is_power_of_two() || !(512..=65_536).contains(&page_size) {
        return WalHeaderState::Ignore;
    }
    let checksum = wal_checksum(checksum_order, &header[..24], [0, 0]);
    if checksum != [be_u32(&header[24..28]), be_u32(&header[28..32])] {
        return WalHeaderState::Corrupt(WAL_HEADER_CHECKSUM_REASON);
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
    read_exact_paced(file, &mut header)?;
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
    run_observation_test_hook(
        journal_path,
        SqliteObservationTestPhase::BeforeJournalTailRead,
    );
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
    run_observation_test_hook(
        journal_path,
        SqliteObservationTestPhase::BeforeJournalTrailerRead,
    );
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
    pace_current_disk_io(bytes.len() as u64);
    let read = file.read(&mut bytes)?;
    bytes.truncate(read);
    Ok(bytes)
}

fn read_tail(file: &mut File, len: u64, limit: usize) -> io::Result<Vec<u8>> {
    let count = usize::try_from(len.min(limit as u64)).unwrap_or(limit);
    file.seek(SeekFrom::Start(len.saturating_sub(count as u64)))?;
    let mut bytes = vec![0_u8; count];
    read_exact_paced(file, &mut bytes)?;
    Ok(bytes)
}

fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0_u8; len];
    read_exact_paced(file, &mut bytes)?;
    Ok(bytes)
}

fn read_exact_paced(file: &mut File, bytes: &mut [u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        pace_current_disk_io((bytes.len() - offset) as u64);
        let read = file.read(&mut bytes[offset..])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SQLite source changed while reading observed bytes",
            ));
        }
        offset += read;
    }
    Ok(())
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap_or_default())
}

pub(crate) fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}
