use super::*;

const SQLITE_SOURCE_FAMILY_COPY_PROGRESS_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn copy_sqlite_member_with_progress<E>(
    member: &SqliteFamilyMember,
    destination: &Path,
    expected_length: u64,
    completed_bytes: &mut u64,
    last_reported_bytes: &mut u64,
    total_bytes: u64,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<(), SqliteSourceProgressError<E>> {
    let mut source_file =
        member
            .file()
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining a provider SQLite component for snapshot copy",
                path: member.path.clone(),
                source,
            })?;
    source_file
        .seek(SeekFrom::Start(0))
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "seeking a provider SQLite component for snapshot copy",
            path: member.path.clone(),
            source,
        })?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "creating a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    let mut remaining = expected_length;
    let mut buffer = [0_u8; SQLITE_COPY_BUFFER_BYTES];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let read = source_file
            .read(&mut buffer[..requested])
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading a provider SQLite snapshot component",
                path: member.path.clone(),
                source,
            })?;
        if read == 0 {
            return Err(SqliteSourceAccessError::SourceChanged.into());
        }
        destination_file
            .write_all(&buffer[..read])
            .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "writing a ctx-owned SQLite snapshot component",
                path: destination.to_path_buf(),
                source,
            })?;
        remaining -= read as u64;
        *completed_bytes = completed_bytes.checked_add(read as u64).ok_or_else(|| {
            SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the SQLite source-family copy progress count overflowed".to_owned(),
            }
        })?;
        if *completed_bytes == total_bytes
            || completed_bytes.saturating_sub(*last_reported_bytes)
                >= SQLITE_SOURCE_FAMILY_COPY_PROGRESS_BYTES
        {
            report_source_family_copy_progress(report_progress, *completed_bytes, total_bytes)?;
            *last_reported_bytes = *completed_bytes;
        }
    }
    let mut extra = [0_u8; 1];
    if source_file
        .read(&mut extra)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "certifying a provider SQLite snapshot component length",
            path: member.path.clone(),
            source,
        })?
        != 0
    {
        return Err(SqliteSourceAccessError::SourceChanged.into());
    }
    destination_file
        .flush()
        .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "flushing a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    Ok(())
}

pub(super) fn report_source_family_copy_progress<E>(
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
    completed_bytes: u64,
    total_bytes: u64,
) -> Result<(), SqliteSourceProgressError<E>> {
    let mut progress = SourceBackedCurrentSourceProgress::new(
        SourceBackedCurrentSourceProgressStage::SourceFamilyCopy,
    );
    progress.snapshot_bytes_completed = Some(completed_bytes);
    progress.snapshot_bytes_total = Some(total_bytes);
    report_progress(progress).map_err(SqliteSourceProgressError::Progress)
}
