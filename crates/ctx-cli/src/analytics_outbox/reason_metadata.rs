//! Best-effort evidence, never authority for the shared v3 queue.
//! Every call is made while holding the existing outbox state lock.
use super::*;

const MAX_BYTES: u64 = 64 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u8,
    authority: Authority,
    roots: BTreeMap<String, Reason>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Authority {
    file_id: (u64, u64),
    modified: std::time::SystemTime,
    sha256: [u8; 32],
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reason {
    sequence: u64,
    class: AnalyticsDeliveryFailureClass,
    reason: AnalyticsDeliveryFailureReason,
}

fn path(queue: &Path) -> PathBuf {
    queue.with_extension("reasons.json")
}

fn temp_path(queue: &Path) -> PathBuf {
    queue.with_extension("reasons.tmp")
}

pub(super) fn discard(queue: &Path) {
    // Exact optional filenames only; never follow links or remove directories.
    let _ = fs::remove_file(path(queue));
    let _ = fs::remove_file(temp_path(queue));
}

fn authority(file: &fs::File, body: &[u8]) -> Option<Authority> {
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() != body.len() as u64 {
        return None;
    }
    Some(Authority {
        file_id: file_id(file, &metadata)?,
        modified: metadata.modified().ok()?,
        sha256: Sha256::digest(body).into(),
    })
}

#[cfg(unix)]
fn file_id(_file: &fs::File, metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_id(file: &fs::File, _metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
        },
    };
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: the file owns a live handle and info is a valid output buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, info.as_mut_ptr()) } == 0
    {
        return None;
    }
    // SAFETY: the successful call initialized info.
    let info = unsafe { info.assume_init() };
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return None;
    }
    Some((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_id(_file: &fs::File, _metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

fn read(queue: &Path) -> Option<Metadata> {
    let path = path(queue);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_BYTES {
        return None;
    }
    verify_private_file(&path).ok()?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&path).ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() || file_id(&file, &opened).is_none() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let metadata: Metadata = serde_json::from_slice(&bytes).ok()?;
    if metadata.version != 1 || metadata.roots.len() > OUTBOX_MAX_ENTRIES {
        return None;
    }
    Some(metadata)
}

pub(super) fn restore(queue: &Path, file: &fs::File, body: &[u8], state: &mut OutboxState) {
    let Some(current) = authority(file, body) else {
        return;
    };
    let Some(metadata) = read(queue).filter(|metadata| metadata.authority == current) else {
        return;
    };
    for (id, reason) in metadata.roots {
        let Some(root) = state.roots.get_mut(&id) else {
            continue;
        };
        if root.failure_sequence == reason.sequence
            && root.last_failure_class == Some(reason.class)
            && reason.reason.permits(reason.class)
        {
            root.last_failure_reason = Some(reason.reason);
        }
    }
}

pub(super) fn save(queue: &Path, body: &[u8], state: &OutboxState) {
    let _ = save_inner(queue, body, state);
}

fn save_inner(queue: &Path, body: &[u8], state: &OutboxState) -> Option<()> {
    let roots = state
        .roots
        .iter()
        .filter_map(|(id, root)| {
            let class = root.last_failure_class?;
            let reason = root
                .last_failure_reason
                .filter(|reason| reason.permits(class))?;
            Some((
                id.clone(),
                Reason {
                    sequence: root.failure_sequence,
                    class,
                    reason,
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();
    if roots.is_empty() {
        discard(queue);
        return Some(());
    }
    if roots.len() > OUTBOX_MAX_ENTRIES {
        return None;
    }
    let file = fs::File::open(queue).ok()?;
    let metadata = Metadata {
        version: 1,
        authority: authority(&file, body)?,
        roots,
    };
    let bytes = serde_json::to_vec(&metadata).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let temp = temp_path(queue);
    let _ = fs::remove_file(&temp);
    write_private_file_via(&path(queue), &bytes, &temp).ok()
}
