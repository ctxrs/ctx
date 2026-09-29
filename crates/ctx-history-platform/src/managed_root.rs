//! Stable, home-anchored authority and admission for the movable managed root.
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{platform_security as security, PlatformError, Result};

const LOCATOR_LIMIT: u64 = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Locator {
    schema_version: u32,
    path: PathBuf,
    install_id: String,
}

/// This directory never follows XDG overrides or the selected data root.
pub fn managed_control_root() -> Result<PathBuf> {
    Ok(home()?.join(".ctx-control"))
}

fn home() -> Result<PathBuf> {
    dirs::home_dir().ok_or(PlatformError::MissingHome)
}

pub(crate) fn resolve() -> Result<PathBuf> {
    let home = home()?;
    resolve_in(&home).map_err(|source| PlatformError::ManagedRoot {
        path: home.join(".ctx-control/data-root.json"),
        source,
    })
}

fn resolve_in(home: &Path) -> io::Result<PathBuf> {
    let control = home.join(".ctx-control");
    let Some(locator) = read_locator(&control)? else {
        return Ok(home.join(".ctx"));
    };
    validate_target(&locator.path, &control)?;
    // An unmounted volume can leave an empty mountpoint behind.
    if crate::installation_identity::read_installation_id(&locator.path)? != locator.install_id {
        return Err(invalid(
            "managed data root identity does not match the locator",
        ));
    }
    Ok(locator.path)
}

fn read_locator(control: &Path) -> io::Result<Option<Locator>> {
    let path = control.join("data-root.json");
    match fs::symlink_metadata(control) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => security::verify_private_directory(control)?,
    }
    let file = match open_private_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(LOCATOR_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LOCATOR_LIMIT {
        return Err(invalid("managed data-root locator exceeds 16 KiB"));
    }
    let locator: Locator = serde_json::from_slice(&bytes).map_err(invalid)?;
    if locator.schema_version != 1 || !valid_install_id(&locator.install_id) {
        return Err(invalid("unsupported or invalid managed data-root locator"));
    }
    validate_absolute_target(&locator.path)?;
    Ok(Some(locator))
}

/// An explicit history override may not reactivate a retained managed copy.
/// Independent custom roots have distinct installation identities and remain usable,
/// even when the managed volume is unavailable. Call before writable bootstrap.
pub fn ensure_active_data_root(root: &Path) -> Result<()> {
    let control = managed_control_root()?;
    ensure_active_in(root, &control).map_err(|source| PlatformError::ManagedRoot {
        path: root.to_path_buf(),
        source,
    })
}

fn ensure_active_in(root: &Path, control: &Path) -> io::Result<()> {
    let Some(locator) = read_locator(control)? else {
        return Ok(());
    };
    if root == locator.path
        || fs::canonicalize(root)
            .ok()
            .zip(fs::canonicalize(&locator.path).ok())
            .is_some_and(|(selected, active)| selected == active)
    {
        validate_target(&locator.path, control)?;
        if crate::installation_identity::read_installation_id(&locator.path)? != locator.install_id
        {
            return Err(invalid(
                "managed data root identity does not match the locator",
            ));
        }
        return Ok(());
    }
    match crate::installation_identity::read_installation_id(root) {
        Ok(id) if id == locator.install_id => Err(invalid(format!(
            "retired managed data root {}; use the active root {} and remove the old data-root override",
            root.display(), locator.path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn valid_install_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| !id.is_nil() && id.hyphenated().to_string() == value)
}

fn validate_absolute_target(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(invalid(
            "managed data root must be an absolute, traversal-free path",
        ));
    }
    Ok(())
}

fn validate_target(path: &Path, control: &Path) -> io::Result<()> {
    validate_absolute_target(path)?;
    security::validate_provider_source_outside_data_root(path, control)?;
    security::verify_private_directory(path)
}

/// Read a private regular file without following links or repairing its policy.
pub(crate) fn open_private_file(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        security::open_verified_private_file(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        security::verify_private_file_handle(&file)?;
        Ok(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private files are unsupported",
        ))
    }
}

/// A process-local binding remembers whether a path selected managed history.
/// Validation never reclassifies that binding after the retained source is deleted.
#[derive(Debug, Clone)]
pub struct DataRootSelection {
    path: PathBuf,
    managed: bool,
}

impl DataRootSelection {
    /// Select without opening the managed volume, so independent tools can start
    /// offline. History access must validate this binding under admission.
    pub fn select(custom: Option<PathBuf>) -> Result<Self> {
        let home = home()?;
        Self::select_in(
            &home,
            custom.or_else(|| std::env::var_os("CTX_DATA_ROOT").map(PathBuf::from)),
        )
        .map_err(|source| PlatformError::ManagedRoot {
            path: home.join(".ctx-control/data-root.json"),
            source,
        })
    }

    fn select_in(home: &Path, custom: Option<PathBuf>) -> io::Result<Self> {
        let locator = read_locator(&home.join(".ctx-control"))?;
        let managed_path = locator
            .as_ref()
            .map_or_else(|| home.join(".ctx"), |locator| locator.path.clone());
        let Some(path) = custom else {
            return Ok(Self {
                path: managed_path,
                managed: true,
            });
        };
        let managed = same_path(&path, &managed_path)
            || locator.is_some_and(|locator| {
                crate::installation_identity::read_installation_id(&path)
                    .is_ok_and(|id| id == locator.install_id)
            });
        Ok(Self { path, managed })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_in(&home()?)
            .map_err(|source| PlatformError::ManagedRoot {
                path: self.path.clone(),
                source,
            })
    }

    fn validate_in(&self, home: &Path) -> io::Result<()> {
        let control = home.join(".ctx-control");
        if self.managed {
            let active =
                read_locator(&control)?.map_or_else(|| home.join(".ctx"), |locator| locator.path);
            if !same_path(&self.path, &active) {
                return Err(invalid(format!(
                    "retired managed data root {}; restart this command using {}",
                    self.path.display(),
                    active.display()
                )));
            }
            resolve_in(home)?;
            Ok(())
        } else {
            ensure_active_in(&self.path, &control)
        }
    }

    /// Hold through a history operation or its accounting writes. Independent
    /// custom roots do not establish managed control state on pristine homes.
    pub fn acquire(&self) -> Result<Option<ManagedRootUse>> {
        let lease = if self.managed {
            Some(ManagedRootUse::acquire()?)
        } else {
            ManagedRootUse::acquire_read_only()?
        };
        self.validate()?;
        Ok(lease)
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

/// Held for the whole history command, including daemon/MCP service lifetimes.
/// Admission and users are separate: relocation can stop existing daemons
/// while preventing new processes from resolving the old root.
pub struct ManagedRootUse {
    _users: File,
}

impl ManagedRootUse {
    pub fn acquire() -> Result<Self> {
        let control = managed_control_root()?;
        Self::acquire_in(&control).map_err(|source| PlatformError::ManagedRoot {
            path: control,
            source,
        })
    }

    pub fn acquire_read_only() -> Result<Option<Self>> {
        let control = managed_control_root()?;
        let read = || -> io::Result<Option<Self>> {
            match fs::symlink_metadata(&control) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
                Ok(_) => security::verify_private_directory(&control)?,
            }
            let admission = match open_lock_existing(&control.join("admission.lock")) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            admission.try_lock_shared().map_err(lock_error)?;
            let users = open_lock_existing(&control.join("users.lock"))?;
            users.try_lock_shared().map_err(lock_error)?;
            Ok(Some(Self { _users: users }))
        };
        read().map_err(|source| PlatformError::ManagedRoot {
            path: control,
            source,
        })
    }

    fn acquire_in(control: &Path) -> io::Result<Self> {
        let admission = open_lock(control, "admission.lock")?;
        admission.try_lock_shared().map_err(lock_error)?;
        let users = open_lock(control, "users.lock")?;
        users.try_lock_shared().map_err(lock_error)?;
        Ok(Self { _users: users })
    }
}

/// Exclusive admission belongs to one move. Existing commands must release
/// their user leases before data can be copied or the locator can change.
pub struct ManagedRootMove {
    control: PathBuf,
    _admission: File,
    users: Option<File>,
}

impl ManagedRootMove {
    pub fn acquire() -> Result<Self> {
        let control = managed_control_root()?;
        Self::acquire_in(control.clone()).map_err(|source| PlatformError::ManagedRoot {
            path: control,
            source,
        })
    }

    fn acquire_in(control: PathBuf) -> io::Result<Self> {
        let admission = open_lock(&control, "admission.lock")?;
        let _users = open_lock(&control, "users.lock")?;
        admission.try_lock().map_err(lock_error)?;
        Ok(Self {
            control,
            _admission: admission,
            users: None,
        })
    }

    /// Fail promptly for long-running foreground/MCP clients. Closing those
    /// clients and rerunning is safer than copying while they retain old paths.
    pub fn drain(&mut self) -> io::Result<()> {
        self.drain_with_timeout(std::time::Duration::from_secs(5))
    }

    fn drain_with_timeout(&mut self, timeout: std::time::Duration) -> io::Result<()> {
        let users = open_lock(&self.control, "users.lock")?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match users.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Err(io::Error::other(
                    "ctx commands still use the managed root; close running ctx/MCP clients and retry the move"
                )),
                Err(error) => return Err(error.into()),
            }
        }
        self.users = Some(users);
        Ok(())
    }

    /// Publish only after a complete offline copy, with the source retained.
    pub fn activate(&self, path: &Path, install_id: &str) -> io::Result<()> {
        if self.users.is_none() || !valid_install_id(install_id) {
            return Err(invalid(
                "activation requires quiescence and a valid root identity",
            ));
        }
        validate_target(path, &self.control)?;
        if crate::installation_identity::read_installation_id(path)? != install_id {
            return Err(invalid(
                "copied installation identity does not match the source",
            ));
        }
        let locator = Locator {
            schema_version: 1,
            path: path.to_path_buf(),
            install_id: install_id.to_owned(),
        };
        let temporary = self.control.join("data-root.json.new");
        // Only the admission owner can publish. A previous interrupted publish
        // leaves this disposable file, never an alternate authority.
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut file = security::create_private_file_new(&temporary)?;
        file.write_all(&serde_json::to_vec(&locator).map_err(invalid)?)?;
        file.sync_all()?;
        drop(file);
        replace_locator(&temporary, &self.control.join("data-root.json"))?;
        sync_directory(&self.control)
    }
}

fn open_lock(control: &Path, name: &str) -> io::Result<File> {
    create_private_durable_directory(control)?;
    let path = control.join(name);
    match security::create_private_file_new(&path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_lock_existing(&path),
        Err(error) => Err(error),
    }
}

fn open_lock_existing(path: &Path) -> io::Result<File> {
    #[cfg(not(windows))]
    {
        open_private_file(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        security::verify_private_file(path)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
            )
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        security::verify_private_file_handle(&file)?;
        Ok(file)
    }
}

fn lock_error(error: std::fs::TryLockError) -> io::Error {
    io::Error::other(format!(
        "managed data-root relocation is in progress; retry when it completes: {error}"
    ))
}

/// Establish a directory and its parent entry before publishing data beneath it.
/// The immediate parent must already exist (platform home or destination parent).
pub fn create_private_durable_directory(path: &Path) -> io::Result<()> {
    create_private_durable_directory_with(path, sync_directory)
}

fn create_private_durable_directory_with(
    path: &Path,
    mut sync: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("directory has no parent"))?;
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "directory parent is unavailable",
        ));
    }
    security::create_private_directory_all(path)?;
    sync(path)?;
    sync(parent)
}

pub fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let directory = File::open(path)?;
        // Directory metadata needs fsync. On macOS, File::sync_all instead
        // requests F_FULLFSYNC, a stronger operation not supported by all handles.
        if unsafe { libc::fsync(directory.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    // Windows has no portable directory-flush guarantee. Sync copied files and
    // publish the locator with write-through replacement instead.
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_locator(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_locator(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests;
