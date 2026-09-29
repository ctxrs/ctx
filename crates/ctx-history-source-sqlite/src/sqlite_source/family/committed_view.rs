//! SQLite's two WAL-index headers certify the published commit, independently
//! of DB/WAL write timestamps. Reader marks and checkpoint counters follow them.
use super::*;

const HEADER_BYTES: usize = 48;
pub(super) type WalIndexHeader = [u8; HEADER_BYTES];

impl SqliteSourceFamily {
    pub(super) fn committed_wal_view(&self) -> SqliteSourceAccessResult<Option<WalIndexHeader>> {
        let Some(member) = &self.shared_memory else {
            return Ok(None);
        };
        read_header(member.opened.file()).map_err(|source| SqliteSourceAccessError::Io {
            operation: "reading the SQLite WAL-index header",
            path: member.path.clone(),
            source,
        })
    }
}

impl SqliteFamilyEvidence {
    pub(in super::super) fn copied_commit_matches(&self, directory: Option<&TempDir>) -> bool {
        if !self.has_wal() {
            return true;
        }
        let Some(expected) = self.committed_wal_view else {
            return false;
        };
        let Some(directory) = directory else {
            return false;
        };
        let Ok(file) = File::open(directory.path().join("source.sqlite-shm")) else {
            return false;
        };
        let Ok(Some(recovered)) = read_header(&file) else {
            return false;
        };
        // Recovery resets iChange and recalculates the header checksum. Compare
        // the actual commit frontier: checksum order, page size, mxFrame, nPage,
        // final-frame checksums and WAL salts. No other connection writes here.
        recovered[13..40] == expected[13..40]
    }
}

fn read_header(file: &File) -> std::io::Result<Option<WalIndexHeader>> {
    if file.metadata()?.len() < (2 * HEADER_BYTES) as u64 {
        return Ok(None);
    }
    let (first, second) = match read_copies(file) {
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    };
    Ok((first == second && valid_header(&first)).then_some(first))
}

#[cfg(not(windows))]
fn read_copies(file: &File) -> std::io::Result<(WalIndexHeader, WalIndexHeader)> {
    let mut file = file.try_clone()?;
    let mut first = [0; HEADER_BYTES];
    let mut second = [0; HEADER_BYTES];
    // SQLite writes copy 1 before copy 0; read in the opposite order, as
    // walIndexTryHdr does. Separate reads preserve that ordering. File reads
    // observe the same shared page cache as the producer's mmap on Linux.
    let read = (|| {
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut first)?;
        file.read_exact(&mut second)
    })();
    read?;
    Ok((first, second))
}

#[cfg(windows)]
fn read_copies(file: &File) -> std::io::Result<(WalIndexHeader, WalIndexHeader)> {
    use std::{
        os::windows::io::AsRawHandle,
        sync::atomic::{fence, Ordering},
    };
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Memory::{
            CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
        },
    };
    // Windows only guarantees coherence with SQLite's mapped SHM through
    // another mapped view. Ordinary ReadFile is not an equivalent observation.
    unsafe {
        let mapping = CreateFileMappingW(
            file.as_raw_handle(),
            ptr::null(),
            PAGE_READONLY,
            0,
            0,
            ptr::null(),
        );
        if mapping.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 2 * HEADER_BYTES);
        if view.Value.is_null() {
            let error = std::io::Error::last_os_error();
            CloseHandle(mapping);
            return Err(error);
        }
        let bytes = view.Value.cast::<u8>();
        let mut first = [0; HEADER_BYTES];
        let mut second = [0; HEADER_BYTES];
        for (offset, byte) in first.iter_mut().enumerate() {
            *byte = ptr::read_volatile(bytes.add(offset));
        }
        fence(Ordering::SeqCst);
        for (offset, byte) in second.iter_mut().enumerate() {
            *byte = ptr::read_volatile(bytes.add(HEADER_BYTES + offset));
        }
        UnmapViewOfFile(view);
        CloseHandle(mapping);
        Ok((first, second))
    }
}

fn native_word(bytes: &[u8]) -> u32 {
    u32::from_ne_bytes(bytes.try_into().expect("four-byte WAL-index word"))
}

fn valid_header(header: &WalIndexHeader) -> bool {
    // WalIndexHdr / walIndexTryHdr / walChecksumBytes in stock SQLite. The
    // header and its checksum use native byte order, regardless of WAL order.
    let page_size = u16::from_ne_bytes([header[14], header[15]]);
    if native_word(&header[..4]) != 3_007_000
        || header[12] != 1
        || header[13] > 1
        || !(page_size == 1 || (page_size >= 512 && page_size.is_power_of_two()))
    {
        return false;
    }
    let (mut first, mut second) = (0_u32, 0_u32);
    for pair in header[..40].chunks_exact(8) {
        first = first
            .wrapping_add(native_word(&pair[..4]))
            .wrapping_add(second);
        second = second
            .wrapping_add(native_word(&pair[4..]))
            .wrapping_add(first);
    }
    first == native_word(&header[40..44]) && second == native_word(&header[44..])
}
