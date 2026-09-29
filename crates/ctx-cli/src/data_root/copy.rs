use super::*;
use ctx_history_platform::managed_root::sync_directory;
use std::io;

pub(super) fn validate_destination(source: &Path, destination: &Path) -> Result<()> {
    if !destination.is_absolute()
        || destination
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        bail!("destination must be an absolute, traversal-free path");
    }
    platform_security::verify_private_directory(source).context("inspect managed source root")?;
    platform_security::validate_provider_source_outside_data_root(destination, source)
        .context("source and destination must be separate directories without links")?;
    platform_security::validate_provider_source_outside_data_root(
        destination,
        &managed_root::managed_control_root()?,
    )
    .context("destination must not overlap the stable ctx control directory")?;
    let parent = destination.parent().context("destination has no parent")?;
    if !parent.is_dir() {
        bail!("destination parent is unavailable; mount the destination filesystem and create its parent first");
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            if fs::read_dir(destination)?.next().is_some() {
                bail!("destination must be empty; the retained source is unchanged");
            }
            platform_security::ensure_private_directory(destination)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn copy_root(source: &Path, destination: &Path) -> Result<()> {
    validate_destination(source, destination)?;
    managed_root::create_private_durable_directory(destination)?;
    // A directory that cannot implement a private OS lock is not suitable for
    // the existing ctx stores, regardless of its filesystem name.
    let probe_path = destination.join(".ctx-copy-lock-probe");
    let probe = platform_security::create_private_file_new(&probe_path)?;
    probe
        .try_lock()
        .context("destination does not support ctx file locking")?;
    let contender = fs::File::open(&probe_path)?;
    match contender.try_lock_shared() {
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(error) => return Err(error.into()),
        Ok(()) => bail!("destination does not enforce ctx file locks"),
    }
    drop(contender);
    drop(probe);
    fs::remove_file(probe_path)?;
    copy_directory(source, destination, Path::new(""))
}

fn copy_directory(source: &Path, destination: &Path, relative: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        if transient(&relative) {
            continue;
        }
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        reject_unsafe_entry(&from, &metadata)?;
        if metadata.is_dir() {
            platform_security::create_private_directory_all(&to)?;
            copy_directory(&from, &to, &relative)?;
        } else if metadata.is_file() {
            copy_file(&from, &to, &metadata)?;
        } else {
            bail!(
                "unsupported non-regular managed data entry: {}",
                from.display()
            );
        }
    }
    sync_directory(destination)?;
    Ok(())
}

fn transient(relative: &Path) -> bool {
    // Locks retain no data and must never be transferred as coordination
    // authority. The original inodes remain at the retained source.
    matches!(relative.components().next().map(|part| part.as_os_str()), Some(name) if name == "daemon" || name == "daemon-installations")
        || relative.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            matches!(
                name.as_ref(),
                ".config.mutation.lock"
                    | ".ctx-generation-writer.lock"
                    | ".ctx-generation-read-leases-v2.lock"
                    | ".ctx-generation-lease-coordinator-init-v2.lock"
                    | ".tantivy-meta.lock"
                    | ".tantivy-writer.lock"
                    | "flat_writer.lock"
                    | "flat_transaction.lock"
                    | "acquisition.lock"
            )
        })
}

fn reject_unsafe_entry(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        bail!("refusing to copy symbolic link {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!(
                "managed data entry is not owned by the current user: {}",
                path.display()
            );
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("refusing to copy reparse point {}", path.display());
        }
        if metadata.is_dir() {
            platform_security::verify_private_directory(path)?;
        } else {
            platform_security::verify_private_file(path)?;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path, metadata: &fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    let mut input = {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(source)?;
        reject_unsafe_entry(source, &file.metadata()?)?;
        file
    };
    #[cfg(windows)]
    let mut input = platform_security::open_verified_private_file(source)?;
    #[cfg(not(any(unix, windows)))]
    let mut input = fs::File::open(source)?;
    let mut output = platform_security::create_private_file_new(destination)?;
    io::copy(&mut input, &mut output).with_context(|| format!("copy {}", source.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o100 != 0 {
            output.set_permissions(fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    output.sync_all()?;
    Ok(())
}
