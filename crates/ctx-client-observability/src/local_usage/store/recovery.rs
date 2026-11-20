use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use ctx_history_platform::platform_security::{
    restrict_private_directory, restrict_private_file_handle,
};
use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

use super::{
    configure_transient,
    file_family::{
        capture_checkpointed_image, capture_wal_family_images, deserialize_read_only,
        verify_private_directory_and_owner,
    },
    verify_supported_schema, FamilyGuard, UsageStoreError, BUSY_TIMEOUT, PAGE_SIZE_BYTES,
};

const WAL_HEADER_BYTES: usize = 32;
const WAL_MAGIC_LE: u32 = 0x377f_0682;
const WAL_MAGIC_BE: u32 = 0x377f_0683;
const WAL_FORMAT_VERSION: u32 = 3_007_000;

struct RemoveDirOnDrop(PathBuf);

impl Drop for RemoveDirOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Validates an existing local usage store before SQLite opens the live path
/// for writing.
///
/// When `-wal` is absent or empty, `main` is the complete database state and
/// is validated purely in memory, allowing SQLite to recover a stale or
/// unlocked `-shm` file on live open. When `-wal` is nonempty, both `main` and
/// `-wal` are snapshotted consistently and replayed inside an isolated private
/// staging directory so schema and row invariants are verified before SQLite
/// touches the live file family.
pub(super) fn validate_existing_family_detached(
    path: &Path,
    guard: &FamilyGuard,
) -> Result<(), UsageStoreError> {
    if !guard.has_nonempty_wal()? {
        let image = capture_checkpointed_image(path, guard, || {})?;
        let detached = deserialize_read_only(image)?;
        let schema_version = verify_supported_schema(&detached)?;
        super::super::report::validate_rows_for_schema(&detached, schema_version)?;
        return Ok(());
    }

    let (main_image, wal_image) = capture_wal_family_images(path, guard)?;
    validate_wal_header(&wal_image)?;
    validate_wal_replay_in_private_staging(&main_image, &wal_image)
}

fn validate_wal_header(wal: &[u8]) -> Result<(), UsageStoreError> {
    if wal.len() < WAL_HEADER_BYTES {
        return Err(UsageStoreError::UnsafeReadState);
    }
    let magic = u32::from_be_bytes([wal[0], wal[1], wal[2], wal[3]]);
    let format_version = u32::from_be_bytes([wal[4], wal[5], wal[6], wal[7]]);
    let page_size = u32::from_be_bytes([wal[8], wal[9], wal[10], wal[11]]);
    if !matches!(magic, WAL_MAGIC_LE | WAL_MAGIC_BE)
        || format_version != WAL_FORMAT_VERSION
        || i64::from(page_size) != PAGE_SIZE_BYTES
    {
        return Err(UsageStoreError::UnsafeReadState);
    }
    Ok(())
}

fn validate_wal_replay_in_private_staging(
    main_image: &[u8],
    wal_image: &[u8],
) -> Result<(), UsageStoreError> {
    let staging_dir = create_private_staging_dir()?;
    let _cleanup = RemoveDirOnDrop(staging_dir.clone());
    let staged_db = staging_dir.join("usage.sqlite");
    let staged_wal = staging_dir.join("usage.sqlite-wal");
    write_private_staging_file(&staged_db, main_image)?;
    write_private_staging_file(&staged_wal, wal_image)?;

    let conn = Connection::open_with_flags(
        &staged_db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_transient(&conn, BUSY_TIMEOUT)?;
    let schema_version = verify_supported_schema(&conn)?;
    super::super::report::validate_rows_for_schema(&conn, schema_version)?;
    drop(conn);
    Ok(())
}

fn create_private_staging_dir() -> Result<PathBuf, UsageStoreError> {
    let candidate = std::env::temp_dir().join(format!("ctx-usage-validate-{}", Uuid::new_v4()));
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&candidate)?;
    restrict_private_directory(&candidate)?;
    verify_private_directory_and_owner(&candidate)?;
    Ok(candidate)
}

fn write_private_staging_file(path: &Path, bytes: &[u8]) -> Result<(), UsageStoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    restrict_private_file_handle(&file)?;
    file.write_all(bytes)?;
    Ok(())
}
