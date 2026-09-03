use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read as _, Write as _},
    ops::Deref,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest as _, Sha256};

use super::{
    ManagedPairComponentIdentity, MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH,
    MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH, MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
    MANAGED_PAIR_STATE_RELATIVE_PATH,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    Core,
    Companion,
    Marker,
    Envelope,
    State,
    #[cfg(unix)]
    Integration,
}

impl Slot {
    pub(super) const ALL: [Self; 5] = [
        Self::Core,
        Self::Companion,
        Self::Marker,
        Self::Envelope,
        Self::State,
    ];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Core => "managed-pair Core component",
            Self::Companion => "managed-pair companion component",
            Self::Marker => "managed Core install marker",
            Self::Envelope => "managed-pair signed envelope",
            Self::State => "managed-pair state marker",
            #[cfg(unix)]
            Self::Integration => "managed integration ownership",
        }
    }

    fn relative_path(self) -> &'static str {
        match self {
            Self::Core if cfg!(windows) => "bin/ctx.exe",
            Self::Core => "bin/ctx",
            Self::Companion if cfg!(windows) => "libexec/ctx-pro.exe",
            Self::Companion => "libexec/ctx-pro",
            Self::Marker => MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH,
            Self::Envelope => MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
            Self::State => MANAGED_PAIR_STATE_RELATIVE_PATH,
            #[cfg(unix)]
            Self::Integration => "bin/ctx.install-integrations",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Layout {
    root: PathBuf,
    root_directory: Arc<SecureDirectory>,
    bin_directory: Arc<SecureDirectory>,
    libexec_directory: Arc<SecureDirectory>,
    share_directory: Arc<SecureDirectory>,
    ctx_directory: Arc<SecureDirectory>,
}

#[derive(Debug, Clone)]
pub(super) struct Entry {
    path: PathBuf,
    name: OsString,
    directory: Arc<SecureDirectory>,
}

impl Entry {
    fn new(path: PathBuf, directory: Arc<SecureDirectory>) -> Result<Self> {
        let name = file_name(&path, "managed-pair entry")?.to_os_string();
        Ok(Self {
            path,
            name,
            directory,
        })
    }

    pub(super) fn sibling(&self, name: OsString) -> Self {
        Self {
            path: self.path.with_file_name(&name),
            name,
            directory: Arc::clone(&self.directory),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Entry {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Deref for Entry {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl Layout {
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn open(root: &Path, create: bool) -> Result<Self> {
        validate_absolute_root(root, "managed-pair install root")?;
        if create {
            ensure_directory(root)?;
            ensure_directory(&root.join("bin"))?;
            ensure_directory(&root.join("libexec"))?;
            ensure_directory(&root.join("share"))?;
            ensure_directory(&root.join("share/ctx"))?;
        } else {
            validate_directory(root)?;
            validate_directory(&root.join("bin"))?;
            validate_directory(&root.join("libexec"))?;
            validate_directory(&root.join("share"))?;
            validate_directory(&root.join("share/ctx"))?;
        }
        Self::bind(root)
    }

    pub(super) fn open_candidate(root: &Path) -> Result<Self> {
        validate_absolute_root(root, "managed-pair candidate root")?;
        validate_directory(root)?;
        validate_directory(&root.join("bin"))?;
        validate_directory(&root.join("libexec"))?;
        validate_directory(&root.join("share"))?;
        validate_directory(&root.join("share/ctx"))?;
        Self::bind(root)
    }

    fn bind(root: &Path) -> Result<Self> {
        let root_directory = SecureDirectory::open(root)?;
        Self::bind_from_root(root, root_directory)
    }

    fn bind_from_root(root: &Path, root_directory: SecureDirectory) -> Result<Self> {
        let root_directory = Arc::new(root_directory);
        let bin_directory = Arc::new(root_directory.open_child_directory(OsStr::new("bin"))?);
        let libexec_directory =
            Arc::new(root_directory.open_child_directory(OsStr::new("libexec"))?);
        let share_directory = Arc::new(root_directory.open_child_directory(OsStr::new("share"))?);
        let ctx_directory = Arc::new(share_directory.open_child_directory(OsStr::new("ctx"))?);
        let layout = Self {
            root: root.to_path_buf(),
            root_directory,
            bin_directory,
            libexec_directory,
            share_directory,
            ctx_directory,
        };
        layout.revalidate()?;
        Ok(layout)
    }

    pub(super) fn open_apply_candidate(&self) -> Result<Self> {
        let root_directory = self
            .ctx_directory
            .open_child_directory(OsStr::new(candidate::APPLY_CANDIDATE_DIRECTORY))?;
        Self::bind_from_root(&candidate::apply_candidate_root(&self.root), root_directory)
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        for (directory, path) in [
            (&self.root_directory, self.root.clone()),
            (&self.bin_directory, self.root.join("bin")),
            (&self.libexec_directory, self.root.join("libexec")),
            (&self.share_directory, self.root.join("share")),
            (&self.ctx_directory, self.root.join("share/ctx")),
        ] {
            directory.require_path_identity(&path)?;
        }
        Ok(())
    }

    pub(super) fn target(&self, slot: Slot) -> Entry {
        let directory = match slot {
            Slot::Core | Slot::Marker => Arc::clone(&self.bin_directory),
            #[cfg(unix)]
            Slot::Integration => Arc::clone(&self.bin_directory),
            Slot::Companion => Arc::clone(&self.libexec_directory),
            Slot::Envelope | Slot::State => Arc::clone(&self.ctx_directory),
        };
        Entry::new(self.root.join(slot.relative_path()), directory)
            .expect("fixed managed-pair slot has a file name")
    }

    pub(super) fn staged(&self, slot: Slot, attempt_id: &str) -> Entry {
        candidate::transaction_sibling(&self.target(slot), attempt_id)
    }

    pub(super) fn active_transaction(&self) -> Entry {
        Entry::new(
            self.root
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH),
            Arc::clone(&self.bin_directory),
        )
        .expect("fixed ctx active transaction has a file name")
    }

    pub(super) fn active_transaction_temporary(&self) -> Entry {
        self.active_transaction()
            .sibling(OsString::from(".ctx.upgrade-install-transaction.json.tmp"))
    }

    pub(super) fn root_binding(&self) -> Result<(u64, u64)> {
        let (device, file, _) = file_information(
            &self.root_directory.file,
            "managed-pair retained candidate directory",
        )?;
        Ok((device, file))
    }
}

mod candidate;

pub(super) use candidate::{
    apply_candidate_exists, apply_candidate_root, create_apply_candidate, remove_apply_candidate,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileStamp {
    pub(super) device: u64,
    pub(super) file: u64,
    pub(super) size_bytes: u64,
    pub(super) sha256: String,
}

pub(super) struct ObservedFile {
    pub(super) bytes: Vec<u8>,
    pub(super) stamp: FileStamp,
}

pub(super) fn validate_absolute_root(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("{label} must be a safe absolute path: {}", path.display());
    }
    Ok(())
}

pub(super) fn external_entry(path: &Path, label: &str) -> Result<Entry> {
    validate_absolute_root(path, label)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("{label} has no parent directory"))?;
    let directory = Arc::new(SecureDirectory::open(parent)?);
    Entry::new(path.to_path_buf(), directory)
}

fn ensure_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow!("managed-pair directory has no parent"))?;
            if !parent.as_os_str().is_empty() && !parent.exists() {
                ensure_directory(parent)?;
            }
            validate_directory(parent)?;
            fs::create_dir(path)
                .with_context(|| format!("create managed-pair directory {}", path.display()))?;
            protect_directory(path)?;
            validate_directory(path)
        }
        Err(error) => Err(error).with_context(|| format!("inspect directory {}", path.display())),
    }
}

fn validate_directory(path: &Path) -> Result<()> {
    let directory = SecureDirectory::open(path)?;
    let metadata = directory.file.metadata()?;
    if !metadata.is_dir() {
        bail!("managed-pair path is not a directory: {}", path.display());
    }
    Ok(())
}

fn protect_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    ctx_history_platform::platform_security::restrict_private_directory(path)?;
    Ok(())
}

pub(super) fn read_regular(entry: &Entry, max: u64, label: &str) -> Result<ObservedFile> {
    observe_regular(entry, max, label, true, false)
}

pub(super) fn read_temporary(entry: &Entry, max: u64, label: &str) -> Result<Option<ObservedFile>> {
    if entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .is_none()
    {
        return Ok(None);
    }
    observe_regular(entry, max, label, true, true).map(Some)
}

fn observe_regular(
    entry: &Entry,
    max: u64,
    label: &str,
    collect: bool,
    allow_empty: bool,
) -> Result<ObservedFile> {
    let mut file = open_owner_regular(entry, label)?;
    let (device, identity, size_bytes) = file_information(&file, label)?;
    if (!allow_empty && size_bytes == 0) || size_bytes > max {
        bail!("{label} size is outside its bound");
    }
    let mut bytes = if collect {
        let capacity = usize::try_from(size_bytes).context("managed-pair file is too large")?;
        Vec::with_capacity(capacity)
    } else {
        Vec::new()
    };
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count)?)
            .ok_or_else(|| anyhow!("{label} size overflow"))?;
        if total > size_bytes || total > max {
            bail!("{label} changed size while being read");
        }
        hasher.update(&buffer[..count]);
        if collect {
            bytes.extend_from_slice(&buffer[..count]);
        }
    }
    if total != size_bytes {
        bail!("{label} changed size while being read");
    }
    let stamp = FileStamp {
        device,
        file: identity,
        size_bytes,
        sha256: format!("{:x}", hasher.finalize()),
    };
    require_named_identity(entry, &stamp, label)?;
    Ok(ObservedFile { bytes, stamp })
}

pub(super) fn copy_verified(
    source: &Entry,
    target: &Entry,
    expected: &ManagedPairComponentIdentity,
    executable: bool,
    label: &str,
) -> Result<FileStamp> {
    let mut source_file = open_owner_regular(source, label)?;
    let (source_device, source_identity, source_size) = file_information(&source_file, label)?;
    if source_size != expected.size_bytes() {
        bail!("{label} size does not match the verified managed-pair identity");
    }
    let mut target_file = create_new_file(target, executable, label)?;
    let copy_result = (|| {
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let count = source_file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(count)?)
                .ok_or_else(|| anyhow!("{label} size overflow"))?;
            if total > expected.size_bytes() {
                bail!("{label} grew while being copied");
            }
            hasher.update(&buffer[..count]);
            target_file.write_all(&buffer[..count])?;
        }
        if total != expected.size_bytes() || format!("{:x}", hasher.finalize()) != expected.sha256()
        {
            bail!("{label} does not match the verified managed-pair identity");
        }
        target_file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = copy_result {
        drop(target_file);
        remove_untrusted_new_file(target);
        return Err(error);
    }
    drop(target_file);
    sync_parent(target)?;
    let source_stamp = FileStamp {
        device: source_device,
        file: source_identity,
        size_bytes: source_size,
        sha256: expected.sha256().to_owned(),
    };
    require_named_identity(source, &source_stamp, label)?;
    let target_stamp = observe_regular(target, expected.size_bytes(), label, false, false)?.stamp;
    if target_stamp.size_bytes != expected.size_bytes() || target_stamp.sha256 != expected.sha256()
    {
        bail!("staged {label} changed after its verified copy");
    }
    Ok(target_stamp)
}

pub(super) fn copy_exact(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    executable: bool,
    label: &str,
) -> Result<FileStamp> {
    require_stamp(source, expected, max, label)?;
    let mut source_file = open_owner_regular(source, label)?;
    require_file_identity(&source_file, expected, label)?;
    let mut target_file = create_new_file(target, executable, label)?;
    let result = (|| {
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let count = source_file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(count)?)
                .ok_or_else(|| anyhow!("{label} copy size overflow"))?;
            if total > expected.size_bytes || total > max {
                bail!("{label} changed while its exact copy was made");
            }
            hasher.update(&buffer[..count]);
            target_file.write_all(&buffer[..count])?;
        }
        if total != expected.size_bytes || format!("{:x}", hasher.finalize()) != expected.sha256 {
            bail!("{label} changed while its exact copy was made");
        }
        target_file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = result {
        drop(target_file);
        remove_untrusted_new_file(target);
        return Err(error);
    }
    drop(target_file);
    sync_parent(target)?;
    require_stamp(source, expected, max, label)?;
    let copied = observe_regular(target, max, label, false, false)?.stamp;
    if copied.size_bytes != expected.size_bytes || copied.sha256 != expected.sha256 {
        bail!("copied {label} does not match the source bytes");
    }
    Ok(copied)
}

pub(super) fn write_new(
    entry: &Entry,
    bytes: &[u8],
    executable: bool,
    label: &str,
) -> Result<FileStamp> {
    if bytes.is_empty() {
        bail!("{label} must not be empty");
    }
    let mut file = create_new_file(entry, executable, label)?;
    let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = write_result {
        drop(file);
        remove_untrusted_new_file(entry);
        return Err(error).with_context(|| format!("write staged {label}"));
    }
    drop(file);
    sync_parent(entry)?;
    let observed = observe_regular(
        entry,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        label,
        true,
        false,
    )?;
    let expected_sha = format!("{:x}", Sha256::digest(bytes));
    if observed.bytes != bytes || observed.stamp.sha256 != expected_sha {
        bail!("staged {label} changed while being written");
    }
    Ok(observed.stamp)
}

fn create_new_file(entry: &Entry, executable: bool, label: &str) -> Result<File> {
    let file = entry
        .directory
        .create_new(&entry.name, entry.path(), executable)
        .with_context(|| format!("create staged {label} {}", entry.display()))?;
    if let Err(error) = protect_file_handle(&file, executable) {
        drop(file);
        remove_untrusted_new_file(entry);
        return Err(error).with_context(|| format!("protect staged {label}"));
    }
    Ok(file)
}

fn remove_untrusted_new_file(entry: &Entry) {
    let _ = entry.directory.remove_file(&entry.name, entry.path());
}

fn protect_file_handle(file: &File, executable: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(if executable {
            0o700
        } else {
            0o600
        }))?;
    }
    #[cfg(windows)]
    {
        let _ = executable;
        ctx_history_platform::platform_security::restrict_private_file_handle(file)?;
    }
    Ok(())
}

pub(super) fn verify_content(
    entry: &Entry,
    expected: &ManagedPairComponentIdentity,
    label: &str,
) -> Result<()> {
    let observed = observe_regular(entry, expected.size_bytes(), label, false, false)?;
    if observed.stamp.size_bytes != expected.size_bytes()
        || observed.stamp.sha256 != expected.sha256()
    {
        bail!("{label} does not match its verified managed-pair identity");
    }
    Ok(())
}

pub(super) fn protect_regular(entry: &Entry, executable: bool, label: &str) -> Result<()> {
    let file = open_owner_regular(entry, label)?;
    protect_file_handle(&file, executable)?;
    validate_open_owner_regular(entry, &file, label)
}

pub(super) fn stamp_optional(entry: &Entry, max: u64, label: &str) -> Result<Option<FileStamp>> {
    stamp_optional_impl(entry, max, label, false)
}

pub(super) fn stamp_temporary_optional(
    entry: &Entry,
    max: u64,
    label: &str,
) -> Result<Option<FileStamp>> {
    stamp_optional_impl(entry, max, label, true)
}

fn stamp_optional_impl(
    entry: &Entry,
    max: u64,
    label: &str,
    allow_empty: bool,
) -> Result<Option<FileStamp>> {
    if entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(
        observe_regular(entry, max, label, false, allow_empty)?.stamp,
    ))
}

pub(super) fn matches_stamp(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<bool> {
    Ok(stamp_optional(entry, max, label)?.as_ref() == Some(expected))
}

pub(super) fn require_stamp(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    if !matches_stamp(entry, expected, max, label)? {
        bail!("{label} was substituted at {}", entry.display());
    }
    Ok(())
}

pub(super) fn require_absent(entry: &Entry, label: &str) -> Result<()> {
    if entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .is_some()
    {
        bail!("unexpected {label} exists at {}", entry.display());
    }
    Ok(())
}

pub(super) fn remove_if_exact(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    remove_if_exact_impl(entry, expected, max, label, false)
}

pub(super) fn remove_temporary_exact(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    remove_if_exact_impl(entry, expected, max, label, true)
}

fn remove_if_exact_impl(
    entry: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
    allow_empty: bool,
) -> Result<()> {
    let Some(actual) = stamp_optional_impl(entry, max, label, allow_empty)? else {
        return Ok(());
    };
    if &actual != expected {
        bail!(
            "refusing to remove substituted {label} at {}",
            entry.display()
        );
    }
    require_named_identity(entry, expected, label)?;
    remove_entry_exact(entry, expected, label)
        .with_context(|| format!("remove {label} {}", entry.display()))?;
    entry.directory.sync()
}

#[cfg(unix)]
fn remove_entry_exact(entry: &Entry, _expected: &FileStamp, _label: &str) -> Result<()> {
    entry.directory.remove_file(&entry.name, entry.path())
}

#[cfg(windows)]
fn remove_entry_exact(entry: &Entry, expected: &FileStamp, label: &str) -> Result<()> {
    use std::{mem::size_of, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };

    let file = open_owner_regular_for_delete(entry, label)?;
    require_file_identity(&file, expected, label)?;
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            u32::try_from(size_of::<FILE_DISPOSITION_INFO>())?,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("unlink managed-pair file by handle");
    }
    Ok(())
}

fn open_owner_regular(entry: &Entry, label: &str) -> Result<File> {
    let file = entry
        .directory
        .open_file(&entry.name, entry.path())
        .with_context(|| format!("open {label} {}", entry.display()))?;
    validate_open_owner_regular(entry, &file, label)?;
    Ok(file)
}

#[cfg(windows)]
fn open_owner_regular_for_delete(entry: &Entry, label: &str) -> Result<File> {
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, READ_CONTROL, SYNCHRONIZE,
    };

    // First validate and pin the exact no-follow, owner-private object while
    // deletion is denied. The mutable reopen is relative to the same retained
    // directory and must resolve to that exact identity before it is used.
    let pinned = open_owner_regular(entry, label)?;
    let expected = file_information(&pinned, label)?;
    drop(pinned);
    let file = entry
        .directory
        .open_relative(
            &entry.name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL | DELETE | SYNCHRONIZE,
            FILE_SHARE_READ,
            windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
        )
        .with_context(|| format!("open mutable {label} {}", entry.display()))?;
    validate_open_owner_regular_handle(&file, label)?;
    if file_information(&file, label)? != expected {
        bail!("{label} was substituted before its mutable open");
    }
    Ok(file)
}

fn validate_open_owner_regular(entry: &Entry, file: &File, label: &str) -> Result<()> {
    let metadata = file.metadata()?;
    let named = entry
        .directory
        .entry_metadata(&entry.name, entry.path())?
        .ok_or_else(|| anyhow!("{label} disappeared while being opened"))?;
    if !metadata.is_file() || !named.is_file || named.is_symlink {
        bail!(
            "{label} is not a regular no-follow file: {}",
            entry.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.dev() != named.device
            || metadata.ino() != named.file
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            bail!(
                "{label} is not an owner-safe unique file: {}",
                entry.display()
            );
        }
    }
    #[cfg(windows)]
    {
        if named.attributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            bail!(
                "{label} traverses a Windows reparse point: {}",
                entry.display()
            );
        }
        let (device, identity, links) = windows_file_information(&file, label)?;
        if device != named.device || identity != named.file || links != 1 {
            bail!(
                "{label} is not an owner-safe unique Windows file: {}",
                entry.display()
            );
        }
        validate_open_owner_regular_handle(file, label)?;
    }
    Ok(())
}

#[cfg(windows)]
fn validate_open_owner_regular_handle(file: &File, label: &str) -> Result<()> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let metadata = file.metadata()?;
    let (_, _, links) = windows_file_information(file, label)?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || links != 1
    {
        bail!("{label} is not an owner-safe unique no-follow Windows file");
    }
    ctx_history_platform::platform_security::verify_private_file_handle(file)
        .with_context(|| format!("verify owner-safe {label}"))
}

fn require_named_identity(entry: &Entry, expected: &FileStamp, label: &str) -> Result<()> {
    let file = open_owner_regular(entry, label)?;
    require_file_identity(&file, expected, label)
}

fn require_file_identity(file: &File, expected: &FileStamp, label: &str) -> Result<()> {
    let (device, identity, size_bytes) = file_information(file, label)?;
    if device != expected.device || identity != expected.file || size_bytes != expected.size_bytes {
        bail!("{label} pathname changed while being verified");
    }
    Ok(())
}

fn file_name<'a>(path: &'a Path, label: &str) -> Result<&'a OsStr> {
    path.file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("{label} path has no file name"))
}

mod platform;
mod secure_directory;

pub(super) use platform::durable_replace;
#[cfg(windows)]
use platform::windows_file_information;
use platform::{file_information, sync_parent};
use secure_directory::SecureDirectory;
