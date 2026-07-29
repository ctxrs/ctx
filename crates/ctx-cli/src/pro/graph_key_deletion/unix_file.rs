//! Delete-only routing for the private helper's owner-private Unix key store.
//!
//! The public host can remove one opaque graph-key record during uninstall, but
//! it cannot read the record or otherwise participate in private key storage.

use std::{
    ffi::{CString, OsStr},
    fs::File,
    io::{self, Read as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    },
    path::{Component, Path},
};

use fs2::FileExt as _;

use crate::pro::credential_vault::CredentialVaultError;

const STORE_DIRECTORY: &str = ".ctx-pro-key-store-v1";
const RECORDS_DIRECTORY: &str = "records";
const BACKEND_MARKER: &str = "backend";
const LOCK_FILE: &str = "lock";
const FILE_SELECTION: &[u8; 12] = b"CTXKSB01FILE";
const RECORD_BYTES: usize = 8 + 32 + 32;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackendSelection {
    File,
    Native,
}

pub(super) fn delete(
    pro_root: &Path,
    account: &str,
    native_selection: &'static [u8; 12],
    native_delete: impl FnOnce() -> Result<(), CredentialVaultError>,
) -> Result<(), CredentialVaultError> {
    let Some(store) = Store::open_existing(pro_root, native_selection)? else {
        return native_delete();
    };
    let initial_selection = store.read_selection()?;
    if initial_selection.is_none() {
        store.validate_unselected_file_state()?;
    }
    let Some(lock) = store.open_existing_lock()? else {
        return if initial_selection.is_some() {
            Err(CredentialVaultError::Corrupt)
        } else {
            native_delete()
        };
    };
    lock.lock_exclusive().map_err(|error| map_io(&error))?;
    let result = (|| {
        store.validate_layout()?;
        validate_named_file(
            &store.root,
            OsStr::new(LOCK_FILE),
            &lock,
            store.owner_uid,
            None,
        )?;
        match store.read_selection()? {
            Some(BackendSelection::File) => store.delete_record(account),
            Some(BackendSelection::Native) => native_delete(),
            None => {
                store.validate_unselected_file_state()?;
                native_delete()
            }
        }
    })();
    let final_validation = store.validate_layout().and_then(|()| {
        validate_named_file(
            &store.root,
            OsStr::new(LOCK_FILE),
            &lock,
            store.owner_uid,
            None,
        )
    });
    let unlock = fs2::FileExt::unlock(&lock).map_err(|error| map_io(&error));
    match (result, final_validation, unlock) {
        (Ok(()), Ok(()), Ok(())) => Ok(()),
        (Err(error), _, _) | (Ok(()), Err(error), _) | (Ok(()), Ok(()), Err(error)) => Err(error),
    }
}

struct Store {
    pro_root: File,
    root: File,
    records: File,
    owner_uid: u32,
    native_selection: &'static [u8; 12],
}

impl Store {
    fn open_existing(
        pro_root: &Path,
        native_selection: &'static [u8; 12],
    ) -> Result<Option<Self>, CredentialVaultError> {
        let owner_uid = effective_uid();
        let Some(pro_root) = open_absolute_directory_if_exists(pro_root)? else {
            return Ok(None);
        };
        verify_private_directory(&pro_root, owner_uid)?;
        let root = match open_directory_at(&pro_root, OsStr::new(STORE_DIRECTORY)) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(map_path(&error)),
        };
        verify_private_directory(&root, owner_uid)?;
        let records = match open_directory_at(&root, OsStr::new(RECORDS_DIRECTORY)) {
            Ok(records) => records,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return if directory_has_unexpected_entries(&root, &[])? {
                    Err(CredentialVaultError::Corrupt)
                } else {
                    Ok(None)
                };
            }
            Err(error) => return Err(map_path(&error)),
        };
        verify_private_directory(&records, owner_uid)?;
        let store = Self {
            pro_root,
            root,
            records,
            owner_uid,
            native_selection,
        };
        store.validate_layout()?;
        Ok(Some(store))
    }

    fn validate_layout(&self) -> Result<(), CredentialVaultError> {
        verify_private_directory(&self.pro_root, self.owner_uid)?;
        validate_named_directory(
            &self.pro_root,
            OsStr::new(STORE_DIRECTORY),
            &self.root,
            self.owner_uid,
        )?;
        validate_named_directory(
            &self.root,
            OsStr::new(RECORDS_DIRECTORY),
            &self.records,
            self.owner_uid,
        )
    }

    fn open_existing_lock(&self) -> Result<Option<File>, CredentialVaultError> {
        open_existing_private_file(&self.root, OsStr::new(LOCK_FILE), self.owner_uid, None)
    }

    fn validate_unselected_file_state(&self) -> Result<(), CredentialVaultError> {
        self.validate_layout()?;
        if directory_has_unexpected_entries(&self.records, &[])?
            || directory_has_unexpected_entries(
                &self.root,
                &[RECORDS_DIRECTORY.as_bytes(), LOCK_FILE.as_bytes()],
            )?
        {
            Err(CredentialVaultError::Corrupt)
        } else {
            Ok(())
        }
    }

    fn read_selection(&self) -> Result<Option<BackendSelection>, CredentialVaultError> {
        let Some(mut marker) = open_existing_private_file(
            &self.root,
            OsStr::new(BACKEND_MARKER),
            self.owner_uid,
            Some(12),
        )?
        else {
            return Ok(None);
        };
        let mut bytes = [0_u8; 12];
        marker
            .read_exact(&mut bytes)
            .map_err(|_| CredentialVaultError::Corrupt)?;
        let mut extra = [0_u8; 1];
        if marker
            .read(&mut extra)
            .map_err(|_| CredentialVaultError::Corrupt)?
            != 0
        {
            return Err(CredentialVaultError::Corrupt);
        }
        validate_named_file(
            &self.root,
            OsStr::new(BACKEND_MARKER),
            &marker,
            self.owner_uid,
            Some(12),
        )?;
        if bytes == *FILE_SELECTION {
            Ok(Some(BackendSelection::File))
        } else if bytes == *self.native_selection {
            Ok(Some(BackendSelection::Native))
        } else {
            Err(CredentialVaultError::Corrupt)
        }
    }

    fn delete_record(&self, account: &str) -> Result<(), CredentialVaultError> {
        let name = record_name(account)?;
        let Some(record) = open_existing_private_file(
            &self.records,
            OsStr::new(&name),
            self.owner_uid,
            Some(RECORD_BYTES),
        )?
        else {
            return Err(CredentialVaultError::NotFound);
        };
        validate_named_file(
            &self.records,
            OsStr::new(&name),
            &record,
            self.owner_uid,
            Some(RECORD_BYTES),
        )?;
        unlink_file(&self.records, OsStr::new(&name))?;
        drop(record);
        self.records.sync_all().map_err(|error| map_io(&error))?;
        if entry_exists(&self.records, OsStr::new(&name))? {
            Err(CredentialVaultError::Backend)
        } else {
            Ok(())
        }
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: the pointer is created by `fdopendir`, remains exclusively
        // owned by this guard, and is closed exactly once.
        unsafe {
            libc::closedir(self.0);
        }
    }
}

fn directory_has_unexpected_entries(
    directory: &File,
    allowed: &[&[u8]],
) -> Result<bool, CredentialVaultError> {
    // SAFETY: `directory` is live. `F_DUPFD_CLOEXEC` returns a new descriptor
    // whose ownership transfers to `fdopendir` on success.
    let duplicate = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(CredentialVaultError::Corrupt);
    }
    // SAFETY: `duplicate` is a live descriptor for a verified directory.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: ownership was not transferred when `fdopendir` failed.
        unsafe {
            libc::close(duplicate);
        }
        return Err(CredentialVaultError::Corrupt);
    }
    let stream = DirectoryStream(stream);
    loop {
        // POSIX requires clearing errno before `readdir` so EOF can be
        // distinguished from an enumeration failure.
        // SAFETY: the platform errno pointer is valid for this thread.
        unsafe {
            *errno_pointer() = 0;
        }
        // SAFETY: the directory stream is live and exclusively used here.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            // SAFETY: the platform errno pointer is valid for this thread.
            return if unsafe { *errno_pointer() } == 0 {
                Ok(false)
            } else {
                Err(CredentialVaultError::Corrupt)
            };
        }
        // SAFETY: `readdir` returned a live entry with a NUL-terminated name.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." || allowed.contains(&name) {
            continue;
        }
        return Ok(true);
    }
}

#[cfg(target_os = "linux")]
unsafe fn errno_pointer() -> *mut libc::c_int {
    // SAFETY: delegated to the caller; libc returns this thread's errno slot.
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn errno_pointer() -> *mut libc::c_int {
    // SAFETY: delegated to the caller; libc returns this thread's errno slot.
    unsafe { libc::__error() }
}

fn record_name(account: &str) -> Result<String, CredentialVaultError> {
    let suffix = account
        .strip_prefix("nvr1-g-")
        .ok_or(CredentialVaultError::Corrupt)?;
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CredentialVaultError::Corrupt);
    }
    Ok(format!("{account}.record"))
}

fn open_absolute_directory_if_exists(path: &Path) -> Result<Option<File>, CredentialVaultError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err(CredentialVaultError::InvalidDataRoot);
    }
    let root = CString::new("/").map_err(|_| CredentialVaultError::InvalidDataRoot)?;
    // SAFETY: AT_FDCWD and the static NUL-terminated root path are valid.
    let descriptor = unsafe {
        libc::openat(
            libc::AT_FDCWD,
            root.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    let mut current = file_from_descriptor(descriptor).map_err(|error| map_path(&error))?;
    for component in path.components() {
        if let Component::Normal(name) = component {
            current = match open_directory_at(&current, name) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(map_path(&error)),
            };
        }
    }
    Ok(Some(current))
}

fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    let name = io_component(name)?;
    // SAFETY: parent is live and name is one NUL-terminated component.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    file_from_descriptor(descriptor)
}

fn open_existing_private_file(
    parent: &File,
    name: &OsStr,
    owner_uid: u32,
    expected_size: Option<usize>,
) -> Result<Option<File>, CredentialVaultError> {
    let name_c = component(name)?;
    // SAFETY: parent is live and name is one NUL-terminated component.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name_c.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    let file = match file_from_descriptor(descriptor) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_path(&error)),
    };
    verify_private_file(&file, owner_uid, expected_size)?;
    Ok(Some(file))
}

fn validate_named_directory(
    parent: &File,
    name: &OsStr,
    opened: &File,
    owner_uid: u32,
) -> Result<(), CredentialVaultError> {
    verify_private_directory(opened, owner_uid)?;
    let named = open_directory_at(parent, name).map_err(|error| map_path(&error))?;
    verify_private_directory(&named, owner_uid)?;
    same_file(opened, &named)
}

fn validate_named_file(
    parent: &File,
    name: &OsStr,
    opened: &File,
    owner_uid: u32,
    expected_size: Option<usize>,
) -> Result<(), CredentialVaultError> {
    verify_private_file(opened, owner_uid, expected_size)?;
    let named = open_existing_private_file(parent, name, owner_uid, expected_size)?
        .ok_or(CredentialVaultError::Corrupt)?;
    same_file(opened, &named)
}

fn same_file(left: &File, right: &File) -> Result<(), CredentialVaultError> {
    let left = left.metadata().map_err(|error| map_io(&error))?;
    let right = right.metadata().map_err(|error| map_io(&error))?;
    if left.dev() == right.dev() && left.ino() == right.ino() {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

fn verify_private_directory(directory: &File, owner_uid: u32) -> Result<(), CredentialVaultError> {
    let metadata = directory.metadata().map_err(|error| map_io(&error))?;
    if metadata.is_dir()
        && metadata.uid() == owner_uid
        && metadata.mode() & 0o7777 == PRIVATE_DIRECTORY_MODE
    {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

fn verify_private_file(
    file: &File,
    owner_uid: u32,
    expected_size: Option<usize>,
) -> Result<(), CredentialVaultError> {
    let metadata = file.metadata().map_err(|error| map_io(&error))?;
    if metadata.is_file()
        && metadata.uid() == owner_uid
        && metadata.mode() & 0o7777 == PRIVATE_FILE_MODE
        && metadata.nlink() == 1
        && expected_size.is_none_or(|expected| metadata.len() == expected as u64)
    {
        Ok(())
    } else {
        Err(CredentialVaultError::Corrupt)
    }
}

fn entry_exists(parent: &File, name: &OsStr) -> Result<bool, CredentialVaultError> {
    let name = component(name)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent and name are valid; success initializes the stat structure.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(map_path(&error))
        }
    }
}

fn unlink_file(parent: &File, name: &OsStr) -> Result<(), CredentialVaultError> {
    let name = component(name)?;
    // SAFETY: parent and name are valid and deletion is restricted to one entry.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(map_path(&io::Error::last_os_error()))
    }
}

fn component(name: &OsStr) -> Result<CString, CredentialVaultError> {
    io_component(name).map_err(|_| CredentialVaultError::Corrupt)
}

fn io_component(name: &OsStr) -> io::Result<CString> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private key-store path must be one component",
        ));
    }
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn file_from_descriptor(descriptor: libc::c_int) -> io::Result<File> {
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: the descriptor was freshly returned and ownership transfers.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn map_path(error: &io::Error) -> CredentialVaultError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        CredentialVaultError::Locked
    } else if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
        CredentialVaultError::Corrupt
    } else {
        CredentialVaultError::Backend
    }
}

fn map_io(error: &io::Error) -> CredentialVaultError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        CredentialVaultError::Locked
    } else {
        CredentialVaultError::Backend
    }
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments and no failure mode on supported Unix.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt as _},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        },
        thread,
    };

    use tempfile::tempdir;

    use super::*;

    const NATIVE: &[u8; 12] = b"CTXKSB01SECR";

    fn private_dir(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    }

    fn private_file(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
    }

    fn file_layout() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let root = tempdir().unwrap();
        fs::set_permissions(
            root.path(),
            fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .unwrap();
        let pro = root.path().join("pro");
        private_dir(&pro);
        let store = pro.join(STORE_DIRECTORY);
        private_dir(&store);
        let records = store.join(RECORDS_DIRECTORY);
        private_dir(&records);
        private_file(&store.join(BACKEND_MARKER), FILE_SELECTION);
        private_file(&store.join(LOCK_FILE), b"");
        let account =
            "nvr1-g-12c2fbc8efe95366e7da4511ebe8b5c7e17a38321f4d92831d3a520ee5c7dc07".to_owned();
        private_file(
            &records.join(format!("{account}.record")),
            &[7_u8; RECORD_BYTES],
        );
        (root, pro, account)
    }

    #[test]
    fn file_selection_deletes_and_verifies_only_the_opaque_record() {
        let (_root, pro, account) = file_layout();
        let native_calls = AtomicUsize::new(0);
        assert_eq!(
            delete(&pro, &account, NATIVE, || {
                native_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Ok(())
        );
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
        assert!(!pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"))
            .exists());
    }

    #[test]
    fn pristine_inspection_uses_native_and_creates_nothing() {
        let root = tempdir().unwrap();
        let pro = root.path().join("missing-pro");
        let calls = AtomicUsize::new(0);
        assert_eq!(
            delete(
                &pro,
                "nvr1-g-0000000000000000000000000000000000000000000000000000000000000000",
                NATIVE,
                || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(CredentialVaultError::NotFound)
                }
            ),
            Err(CredentialVaultError::NotFound)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!pro.exists());
    }

    #[test]
    fn native_selection_never_downgrades_to_file_deletion() {
        let (_root, pro, account) = file_layout();
        private_file(&pro.join(STORE_DIRECTORY).join(BACKEND_MARKER), NATIVE);
        let calls = AtomicUsize::new(0);
        assert_eq!(
            delete(&pro, &account, NATIVE, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(CredentialVaultError::Unavailable { platform: "test" })
            }),
            Err(CredentialVaultError::Unavailable { platform: "test" })
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"))
            .exists());
    }

    #[test]
    fn markerless_file_records_and_selector_temps_fail_closed() {
        let (_root, pro, account) = file_layout();
        fs::remove_file(pro.join(STORE_DIRECTORY).join(BACKEND_MARKER)).unwrap();
        let native_calls = AtomicUsize::new(0);
        assert_eq!(
            delete(&pro, &account, NATIVE, || {
                native_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Err(CredentialVaultError::Corrupt)
        );
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);

        let (_root, pro, account) = file_layout();
        let store = pro.join(STORE_DIRECTORY);
        fs::remove_file(store.join(BACKEND_MARKER)).unwrap();
        fs::remove_file(
            store
                .join(RECORDS_DIRECTORY)
                .join(format!("{account}.record")),
        )
        .unwrap();
        private_file(&store.join(".tmp-interrupted-selection"), b"orphan");
        assert_eq!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        );
    }

    #[test]
    fn partial_store_root_with_unexpected_state_fails_closed() {
        let root = tempdir().unwrap();
        fs::set_permissions(
            root.path(),
            fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .unwrap();
        let pro = root.path().join("pro");
        private_dir(&pro);
        let store = pro.join(STORE_DIRECTORY);
        private_dir(&store);
        private_file(&store.join(".tmp-interrupted"), b"orphan");
        let native_calls = AtomicUsize::new(0);

        assert_eq!(
            delete(
                &pro,
                "nvr1-g-0000000000000000000000000000000000000000000000000000000000000000",
                NATIVE,
                || {
                    native_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            ),
            Err(CredentialVaultError::Corrupt)
        );
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn missing_lock_and_unsafe_records_fail_closed() {
        let (_root, pro, account) = file_layout();
        fs::remove_file(pro.join(STORE_DIRECTORY).join(LOCK_FILE)).unwrap();
        assert_eq!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        );

        let (root, pro, account) = file_layout();
        let record = pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"));
        fs::hard_link(&record, root.path().join("record-link")).unwrap();
        assert_eq!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        );

        let (_root, pro, account) = file_layout();
        let record = pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"));
        private_file(&record, &[0_u8; RECORD_BYTES + 1]);
        assert_eq!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        );

        let (_root, pro, account) = file_layout();
        let record = pro
            .join(STORE_DIRECTORY)
            .join(RECORDS_DIRECTORY)
            .join(format!("{account}.record"));
        fs::remove_file(&record).unwrap();
        symlink("/dev/null", &record).unwrap();
        assert!(matches!(
            delete(&pro, &account, NATIVE, || Ok(())),
            Err(CredentialVaultError::Corrupt)
        ));
    }

    #[test]
    fn concurrent_deletions_are_serialized_and_verified() {
        let (_root, pro, account) = file_layout();
        let pro = Arc::new(pro);
        let account = Arc::new(account);
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let pro = Arc::clone(&pro);
                let account = Arc::clone(&account);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    delete(&pro, &account, NATIVE, || Ok(()))
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok(())))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(CredentialVaultError::NotFound)))
                .count(),
            1
        );
    }
}
