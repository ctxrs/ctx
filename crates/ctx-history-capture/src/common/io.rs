use std::{
    fs::{self, File},
    io::{self, BufRead, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{CaptureError, ProviderImportSummary, Result, MAX_PROVIDER_JSONL_LINE_BYTES};

pub(crate) fn collect_jsonl_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    visit_jsonl_paths(root, &mut |path| {
        paths.push(path.to_path_buf());
        Ok(())
    })
}

pub(crate) fn visit_jsonl_paths(
    root: &Path,
    visitor: &mut impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        // Explicit provider files already carry an adapter format. Extension
        // filtering applies only while discovering children of a directory.
        ensure_regular_provider_transcript_file(root)?;
        visitor(root)?;
        return Ok(());
    }
    if !file_type.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visit_jsonl_paths(&path, visitor)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            ensure_regular_provider_transcript_file(&path)?;
            visitor(&path)?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_regular_provider_transcript_file(path: &Path) -> Result<()> {
    drop(open_regular_provider_transcript_file(path)?);
    Ok(())
}

/// Opens a provider transcript through each already-opened parent directory.
/// The returned handle is the same handle whose type and link status were
/// validated, so callers never validate one pathname object and read another.
pub(crate) fn open_regular_provider_transcript_file(path: &Path) -> Result<File> {
    open_regular_provider_transcript_file_impl(path).map_err(|failure| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: failure.reason(),
        }
    })
}

#[derive(Debug, Clone, Copy)]
enum SecureOpenFailure {
    Parent,
    FinalLinkOrType,
    Io(io::ErrorKind),
}

impl SecureOpenFailure {
    fn reason(self) -> &'static str {
        match self {
            Self::Parent => "symlinked provider transcript path components are rejected",
            Self::FinalLinkOrType => "symlinked provider transcript files are rejected",
            Self::Io(io::ErrorKind::NotFound) => "provider transcript path does not exist",
            Self::Io(_) => "provider transcript path could not be opened securely",
        }
    }
}

#[cfg(unix)]
fn open_regular_provider_transcript_file_impl(
    path: &Path,
) -> std::result::Result<File, SecureOpenFailure> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
        path::Component,
    };

    let components = path
        .components()
        .filter_map(|component| match component {
            Component::RootDir | Component::Prefix(_) => None,
            Component::CurDir | Component::ParentDir | Component::Normal(_) => {
                Some(component.as_os_str())
            }
        })
        .collect::<Vec<_>>();
    let Some((file_name, parents)) = components.split_last() else {
        return Err(SecureOpenFailure::FinalLinkOrType);
    };
    let base = if path.is_absolute() { b"/\0" } else { b".\0" };
    let base_fd = unsafe {
        libc::open(
            base.as_ptr().cast(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if base_fd < 0 {
        return Err(SecureOpenFailure::Io(io::Error::last_os_error().kind()));
    }
    let mut directory = unsafe { File::from_raw_fd(base_fd) };
    let mut opened_path = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::from(".")
    };
    for component in parents {
        let component =
            CString::new(component.as_bytes()).map_err(|_| SecureOpenFailure::Parent)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(SecureOpenFailure::Parent);
        }
        directory = unsafe { File::from_raw_fd(fd) };
        opened_path.push(std::ffi::OsStr::from_bytes(component.as_bytes()));
        run_secure_open_test_hook(&opened_path, SecureOpenTestPhase::AfterParentOpen);
    }
    let file_name =
        CString::new(file_name.as_bytes()).map_err(|_| SecureOpenFailure::FinalLinkOrType)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            file_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return Err(if error.kind() == io::ErrorKind::NotFound {
            SecureOpenFailure::Io(error.kind())
        } else {
            SecureOpenFailure::FinalLinkOrType
        });
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file
        .metadata()
        .map_err(|error| SecureOpenFailure::Io(error.kind()))?
        .file_type()
        .is_file()
    {
        return Err(SecureOpenFailure::FinalLinkOrType);
    }
    opened_path.push(std::ffi::OsStr::from_bytes(file_name.as_bytes()));
    run_secure_open_test_hook(&opened_path, SecureOpenTestPhase::AfterFinalOpen);
    Ok(file)
}

#[cfg(windows)]
fn open_regular_provider_transcript_file_impl(
    path: &Path,
) -> std::result::Result<File, SecureOpenFailure> {
    use std::{
        ffi::OsString,
        path::{Component, Prefix},
    };

    let absolute =
        std::path::absolute(path).map_err(|error| SecureOpenFailure::Io(error.kind()))?;
    let mut components = absolute.components();
    let prefix = match components.next() {
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                Prefix::Disk(_)
                    | Prefix::VerbatimDisk(_)
                    | Prefix::UNC(_, _)
                    | Prefix::VerbatimUNC(_, _)
            ) =>
        {
            prefix
        }
        _ => return Err(SecureOpenFailure::Parent),
    };
    let Some(Component::RootDir) = components.next() else {
        return Err(SecureOpenFailure::Parent);
    };
    let mut root_path = PathBuf::from(prefix.as_os_str());
    root_path.push(Component::RootDir.as_os_str());
    let names = components
        .map(|component| match component {
            Component::Normal(name) => Ok(OsString::from(name)),
            _ => Err(SecureOpenFailure::Parent),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let Some((file_name, parents)) = names.split_last() else {
        return Err(SecureOpenFailure::FinalLinkOrType);
    };

    let mut directory = open_windows_transcript_root(&root_path)?;
    validate_windows_transcript_component(&directory, true)
        .map_err(|_| SecureOpenFailure::Parent)?;
    let mut guards = Vec::with_capacity(parents.len() + 1);
    let mut opened_path = root_path;
    for component in parents {
        let next = open_windows_transcript_component(&directory, component, true)
            .map_err(|_| SecureOpenFailure::Parent)?;
        validate_windows_transcript_component(&next, true)
            .map_err(|_| SecureOpenFailure::Parent)?;
        guards.push(directory);
        directory = next;
        opened_path.push(component);
        run_secure_open_test_hook(&opened_path, SecureOpenTestPhase::AfterParentOpen);
    }
    let file = open_windows_transcript_component(&directory, file_name, false)
        .map_err(|error| SecureOpenFailure::Io(error.kind()))?;
    validate_windows_transcript_component(&file, false)
        .map_err(|_| SecureOpenFailure::FinalLinkOrType)?;
    guards.push(directory);
    opened_path.push(file_name);
    run_secure_open_test_hook(&opened_path, SecureOpenTestPhase::AfterFinalOpen);
    drop(guards);
    Ok(file)
}

#[cfg(windows)]
fn open_windows_transcript_root(path: &Path) -> std::result::Result<File, SecureOpenFailure> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE,
    };

    let mut options = fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    options
        .open(path)
        .map_err(|error| SecureOpenFailure::Io(error.kind()))
}

#[cfg(windows)]
fn open_windows_transcript_component(
    parent: &File,
    name: &std::ffi::OsStr,
    directory: bool,
) -> io::Result<File> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, io::AsRawHandle, io::FromRawHandle},
        ptr,
    };
    use windows_sys::{
        Wdk::{
            Foundation::OBJECT_ATTRIBUTES,
            Storage::FileSystem::{
                NtCreateFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
                FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
            },
        },
        Win32::{
            Foundation::{RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
            Storage::FileSystem::{
                FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
                FILE_TRAVERSE, SYNCHRONIZE,
            },
            System::IO::IO_STATUS_BLOCK,
        },
    };

    let mut name = name.encode_wide().collect::<Vec<_>>();
    let byte_len = name
        .len()
        .checked_mul(mem::size_of::<u16>())
        .and_then(|len| u16::try_from(len).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    if name.is_empty() || name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path component is empty or contains a NUL byte",
        ));
    }
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle: HANDLE = ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    let desired_access = if directory {
        FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE
    } else {
        FILE_GENERIC_READ
    };
    let create_options = FILE_OPEN_REPARSE_POINT
        | FILE_SYNCHRONOUS_IO_NONALERT
        | if directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &attributes,
            &mut status_block,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            create_options,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(unsafe {
            RtlNtStatusToDosError(status) as i32
        }));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(windows)]
fn validate_windows_transcript_component(file: &File, directory: bool) -> io::Result<()> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_TAG_INFO,
    };

    let metadata = file.metadata()?;
    if metadata.file_type().is_dir() != directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid component type",
        ));
    }
    let mut attributes = MaybeUninit::<FILE_ATTRIBUTE_TAG_INFO>::zeroed();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
            FileAttributeTagInfo,
            attributes.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { attributes.assume_init() }.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reparse point rejected",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn open_regular_provider_transcript_file_impl(
    _path: &Path,
) -> std::result::Result<File, SecureOpenFailure> {
    Err(SecureOpenFailure::Io(io::ErrorKind::Unsupported))
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecureOpenTestPhase {
    AfterParentOpen,
    AfterFinalOpen,
}

#[cfg(not(test))]
#[derive(Debug, Clone, Copy)]
enum SecureOpenTestPhase {
    AfterParentOpen,
    AfterFinalOpen,
}

#[cfg(test)]
type SecureOpenTestHook = Box<dyn FnMut(&Path, SecureOpenTestPhase)>;

#[cfg(test)]
thread_local! {
    static SECURE_OPEN_TEST_HOOK: std::cell::RefCell<Option<SecureOpenTestHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct SecureOpenTestHookGuard;

#[cfg(test)]
impl Drop for SecureOpenTestHookGuard {
    fn drop(&mut self) {
        SECURE_OPEN_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn install_secure_open_test_hook(
    hook: impl FnMut(&Path, SecureOpenTestPhase) + 'static,
) -> SecureOpenTestHookGuard {
    SECURE_OPEN_TEST_HOOK.with(|installed| *installed.borrow_mut() = Some(Box::new(hook)));
    SecureOpenTestHookGuard
}

#[cfg(test)]
fn run_secure_open_test_hook(path: &Path, phase: SecureOpenTestPhase) {
    SECURE_OPEN_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path, phase);
        }
    });
}

#[cfg(not(test))]
fn run_secure_open_test_hook(_path: &Path, _phase: SecureOpenTestPhase) {}

pub(crate) fn ensure_provider_path_parents_are_not_symlinks(path: &Path) -> Result<()> {
    let parent_count = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for component in path.components().take(parent_count) {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript path components are rejected",
            });
        }
    }
    Ok(())
}

pub(crate) fn read_text_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
    let file = File::open(path)?;
    read_text_limited(file, max_bytes, label)
}

fn read_text_limited(reader: impl Read, max_bytes: usize, label: &str) -> Result<String> {
    let reader = crate::disk_io_pacing::PacedReader::new(reader);
    let mut reader = reader.take((max_bytes as u64).saturating_add(1));
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} exceeds max bytes ({max_bytes})"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|err| CaptureError::InvalidPayload(format!("{label} is not valid UTF-8: {err}")))
}

pub fn provider_jsonl_range_has_complete_line(
    path: &Path,
    offset: u64,
    observed_size: u64,
) -> Result<bool> {
    let mut file = open_regular_provider_transcript_file(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut remaining = observed_size.saturating_sub(offset);
    let mut scanned = 0usize;
    let scan_limit = MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1);
    let mut buffer = [0u8; 8 * 1024];
    while remaining > 0 && scanned < scan_limit {
        let budget = scan_limit.saturating_sub(scanned);
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .unwrap_or(buffer.len())
            .min(budget);
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Ok(false);
        }
        if buffer[..read].contains(&b'\n') {
            return Ok(true);
        }
        scanned = scanned.saturating_add(read);
        remaining = remaining.saturating_sub(read as u64);
    }
    if remaining > 0 || scanned > MAX_PROVIDER_JSONL_LINE_BYTES {
        return Err(provider_jsonl_line_too_large());
    }
    Ok(false)
}

pub(crate) fn read_provider_jsonl_line(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<bool> {
    match read_provider_jsonl_line_or_skip_oversized(reader, buffer)? {
        ProviderJsonlLineRead::Eof => Ok(false),
        ProviderJsonlLineRead::Line { .. } => Ok(true),
        ProviderJsonlLineRead::Oversized { .. } => Err(provider_jsonl_line_too_large()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderJsonlLineRead {
    Eof,
    Line {
        bytes: usize,
        newline_terminated: bool,
    },
    Oversized {
        bytes: usize,
        newline_terminated: bool,
    },
}

pub(crate) fn read_provider_jsonl_record_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    line_number: &mut usize,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    loop {
        match read_provider_jsonl_line_or_skip_oversized(reader, buffer)? {
            ProviderJsonlLineRead::Eof => return Ok(false),
            ProviderJsonlLineRead::Line { .. } => {
                *line_number = line_number.saturating_add(1);
                return Ok(true);
            }
            ProviderJsonlLineRead::Oversized { .. } => {
                *line_number = line_number.saturating_add(1);
                summary.skipped += 1;
                summary.skipped_events += 1;
            }
        }
    }
}

pub(crate) fn read_provider_jsonl_line_or_skip_oversized(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<ProviderJsonlLineRead> {
    buffer.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total > 0 {
                ProviderJsonlLineRead::Line {
                    bytes: total,
                    newline_terminated: false,
                }
            } else {
                ProviderJsonlLineRead::Eof
            });
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
                reader.consume(bytes_to_consume);
                buffer.clear();
                return Ok(ProviderJsonlLineRead::Oversized {
                    bytes: total.saturating_add(bytes_to_consume),
                    newline_terminated: true,
                });
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            return Ok(ProviderJsonlLineRead::Line {
                bytes: total.saturating_add(bytes_to_consume),
                newline_terminated: true,
            });
        }

        let bytes_to_consume = available.len();
        if total.saturating_add(bytes_to_consume) > MAX_PROVIDER_JSONL_LINE_BYTES {
            reader.consume(bytes_to_consume);
            let (discarded, newline_terminated) = discard_provider_jsonl_line(reader)?;
            buffer.clear();
            if !newline_terminated {
                return Err(provider_jsonl_line_too_large());
            }
            return Ok(ProviderJsonlLineRead::Oversized {
                bytes: total
                    .saturating_add(bytes_to_consume)
                    .saturating_add(discarded),
                newline_terminated,
            });
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
        total = total.saturating_add(bytes_to_consume);
    }
}

pub(crate) fn discard_provider_jsonl_line(reader: &mut impl BufRead) -> Result<(usize, bool)> {
    let mut discarded = 0usize;
    while discarded < MAX_PROVIDER_JSONL_LINE_BYTES {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((discarded, false));
        }
        let remaining = MAX_PROVIDER_JSONL_LINE_BYTES.saturating_sub(discarded);
        let bounded = &available[..available.len().min(remaining)];
        let bytes_to_consume = bounded
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(bounded.len());
        let found_newline = bounded
            .get(bytes_to_consume.saturating_sub(1))
            .is_some_and(|byte| *byte == b'\n');
        reader.consume(bytes_to_consume);
        discarded = discarded.saturating_add(bytes_to_consume);
        if found_newline {
            return Ok((discarded, true));
        }
    }
    Ok((discarded, false))
}

pub(crate) fn provider_jsonl_line_too_large() -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "provider JSONL line exceeds max bytes ({MAX_PROVIDER_JSONL_LINE_BYTES})"
    ))
}

pub(crate) fn read_json_file_limited(path: &Path, max_bytes: usize, label: &str) -> Result<Value> {
    let text = read_text_file_limited(path, max_bytes, label)?;
    serde_json::from_str(&text).map_err(CaptureError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    use crate::{install_disk_io_pacer, DiskIoPacer};

    struct ReservationObservedReader {
        pacer: DiskIoPacer,
        expected_reserved_bytes: u64,
    }

    impl Read for ReservationObservedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert_eq!(self.pacer.charged_bytes(), self.expected_reserved_bytes);
            buffer.fill(b'x');
            Ok(buffer.len())
        }
    }

    #[test]
    fn limited_text_reads_reserve_shared_budget_before_physical_reads() {
        let pacer = DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = install_disk_io_pacer(pacer.clone());

        for expected_reserved_bytes in [4, 8] {
            let error = read_text_limited(
                ReservationObservedReader {
                    pacer: pacer.clone(),
                    expected_reserved_bytes,
                },
                3,
                "provider fixture",
            )
            .expect_err("the max-plus-one byte must preserve the size error");

            assert!(matches!(error, CaptureError::InvalidPayload(_)));
        }

        assert_eq!(pacer.charged_bytes(), 8);
    }

    #[test]
    fn oversized_line_discard_is_bounded_without_a_newline() {
        let source = std::io::Read::take(
            std::io::repeat(b'x'),
            (MAX_PROVIDER_JSONL_LINE_BYTES as u64).saturating_mul(4),
        );
        let mut reader = BufReader::with_capacity(8 * 1024, source);

        let (discarded, newline_terminated) = discard_provider_jsonl_line(&mut reader).unwrap();

        assert_eq!(discarded, MAX_PROVIDER_JSONL_LINE_BYTES);
        assert!(!newline_terminated);
    }

    #[test]
    fn oversized_unterminated_line_stops_before_its_tail_can_be_reframed() {
        let source = std::io::Read::take(
            std::io::repeat(b'x'),
            (MAX_PROVIDER_JSONL_LINE_BYTES as u64).saturating_mul(4),
        );
        let mut reader = BufReader::with_capacity(8 * 1024, source);
        let mut buffer = Vec::new();

        let error = read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut buffer)
            .expect_err("unterminated oversized records must stop parsing");

        assert!(matches!(error, CaptureError::InvalidPayload(_)));
        assert!(buffer.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn append_range_probe_rejects_fifo_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let temp = tempfile::tempdir().expect("tempdir");
        let fifo = temp.path().join("transcript.jsonl");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

        let error = provider_jsonl_range_has_complete_line(&fifo, 0, 1)
            .expect_err("provider FIFOs must be rejected");

        assert!(matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { .. }
        ));
    }

    #[test]
    fn explicit_jsonl_file_does_not_require_an_extension() {
        let temp = tempfile::tempdir().expect("tempdir");
        let transcript = temp.path().join("materialized-provider-fixture");
        fs::write(&transcript, b"{}\n").expect("write transcript");

        let mut paths = Vec::new();
        collect_jsonl_paths(&transcript, &mut paths).expect("collect explicit transcript");

        assert_eq!(paths, vec![transcript]);
    }

    #[test]
    fn directory_discovery_still_filters_non_jsonl_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let transcript = temp.path().join("session.jsonl");
        fs::write(&transcript, b"{}\n").expect("write transcript");
        fs::write(temp.path().join("settings.json"), b"{}").expect("write settings");

        let mut paths = Vec::new();
        collect_jsonl_paths(temp.path(), &mut paths).expect("collect transcript tree");

        assert_eq!(paths, vec![transcript]);
    }
}
