#[cfg(unix)]
use std::io;
use std::path::Path;
use uuid::Uuid;

use super::{
    bounded_sqlite_component_bytes, ctx_sqlite_snapshot_tempdir, map_capture_error,
    open_ctx_owned_sqlite_read_snapshot, sqlite_sidecar_path,
    validate_observed_snapshot_reservation, validate_source_snapshot, AuthorizedSourceRoute,
    BrokeredSqliteSource, CompleteContentError, CompleteContentErrorKind,
    CtxOwnedSqliteReadSnapshot, ReadOnlySqliteConnection,
};
#[cfg(unix)]
use super::{
    copy_bounded_handle, map_io_error, open_brokered_file, revalidate_opened_file, FrozenFile,
};

#[cfg(unix)]
pub(super) fn admit(
    data_root: &Path,
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    reserved_snapshot_bytes: Option<u64>,
    event_id: Uuid,
) -> Result<BrokeredSqliteSource, CompleteContentError> {
    use std::os::unix::fs::PermissionsExt;

    let main = open_brokered_file(selected_path).map_err(|cause| map_io_error(event_id, cause))?;
    let metadata = main
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    if metadata.permissions().mode() & 0o444 == 0 {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceUnreadable,
            event_id,
        ));
    }
    validate_source_snapshot(&route.source_snapshot, &metadata, event_id)?;
    let main_frozen = FrozenFile::from_metadata(&metadata).map_err(|_| {
        CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
    })?;
    let mut sidecars = Vec::new();
    let mut snapshot_bytes = bounded_sqlite_component_bytes(&metadata, event_id)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let source_path = sqlite_sidecar_path(selected_path, suffix);
        match open_brokered_file(&source_path) {
            Ok(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|cause| map_io_error(event_id, cause))?;
                snapshot_bytes = snapshot_bytes
                    .checked_add(bounded_sqlite_component_bytes(&metadata, event_id)?)
                    .ok_or_else(|| {
                        CompleteContentError::new(
                            CompleteContentErrorKind::ContentTooLarge,
                            event_id,
                        )
                    })?;
                let frozen = FrozenFile::from_metadata(&metadata)
                    .map_err(|cause| map_io_error(event_id, cause))?;
                sidecars.push((suffix, source_path, file, frozen));
            }
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
            Err(cause) => return Err(map_io_error(event_id, cause)),
        }
    }
    validate_observed_snapshot_reservation(reserved_snapshot_bytes, snapshot_bytes, event_id)?;
    if sidecars
        .iter()
        .any(|(suffix, _, _, _)| *suffix == "-journal")
    {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceUnreadable,
            event_id,
        ));
    }
    if sidecars.is_empty() {
        if !revalidate_opened_file(selected_path, &main, &main_frozen) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            match open_brokered_file(&sqlite_sidecar_path(selected_path, suffix)) {
                Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(CompleteContentError::new(
                        CompleteContentErrorKind::SourceChanged,
                        event_id,
                    ));
                }
                Err(cause) => return Err(map_io_error(event_id, cause)),
            }
        }
        let evidence = super::open_provider_sqlite_readonly(data_root, selected_path)
            .and_then(ReadOnlySqliteConnection::finish)
            .map_err(|cause| map_capture_error(event_id, cause))?;
        if !revalidate_opened_file(selected_path, &main, &main_frozen) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            match open_brokered_file(&sqlite_sidecar_path(selected_path, suffix)) {
                Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(CompleteContentError::new(
                        CompleteContentErrorKind::SourceChanged,
                        event_id,
                    ));
                }
                Err(cause) => return Err(map_io_error(event_id, cause)),
            }
        }
        return Ok(BrokeredSqliteSource::Provider {
            data_root: std::sync::Arc::new(data_root.to_path_buf()),
            path: selected_path.to_path_buf(),
            evidence,
        });
    }
    let dir = ctx_sqlite_snapshot_tempdir(data_root, event_id)?;
    let path = dir.path().join("source.sqlite");
    copy_bounded_handle(&main, &path, main_frozen.length, event_id)?;
    for (suffix, _, file, frozen) in &sidecars {
        if *suffix == "-shm" {
            continue;
        }
        copy_bounded_handle(
            file,
            &sqlite_sidecar_path(&path, suffix),
            frozen.length,
            event_id,
        )?;
    }
    if !revalidate_opened_file(selected_path, &main, &main_frozen) {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceChanged,
            event_id,
        ));
    }
    for (suffix, source_path, file, frozen) in &sidecars {
        if !revalidate_opened_file(source_path, file, frozen) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        debug_assert_eq!(source_path, &sqlite_sidecar_path(selected_path, suffix));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let source_path = sqlite_sidecar_path(selected_path, suffix);
        let existed = sidecars
            .iter()
            .any(|(observed, _, _, _)| *observed == suffix);
        match open_brokered_file(&source_path) {
            Ok(_) if !existed => {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
            Ok(_) => {}
            Err(cause) if cause.kind() == io::ErrorKind::NotFound && !existed => {}
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
            Err(cause) => return Err(map_io_error(event_id, cause)),
        }
    }
    // Opening now proves that the copied DB/WAL set is coherent. Resolvers open
    // their own bounded read-only connection against the same ctx-owned snapshot.
    open_ctx_owned_sqlite_read_snapshot(&path)
        .and_then(CtxOwnedSqliteReadSnapshot::finish)
        .map_err(super::map_sqlite_source_access_error)
        .map_err(|cause| map_capture_error(event_id, cause))?;
    Ok(BrokeredSqliteSource::CtxOwned { _dir: dir, path })
}

#[cfg(target_os = "windows")]
pub(super) fn admit(
    data_root: &Path,
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    reserved_snapshot_bytes: Option<u64>,
    event_id: Uuid,
) -> Result<BrokeredSqliteSource, CompleteContentError> {
    let database =
        super::windows::admit_regular_file(selected_path, route.source_root.as_deref(), event_id)?;
    validate_source_snapshot(&route.source_snapshot, &database.metadata, event_id)?;
    let sidecar_paths =
        ["-wal", "-shm", "-journal"].map(|suffix| sqlite_sidecar_path(selected_path, suffix));
    let mut sidecars = Vec::with_capacity(sidecar_paths.len());
    for path in &sidecar_paths {
        sidecars.push(super::windows::admit_optional_regular_file(
            path,
            route.source_root.as_deref(),
            event_id,
        )?);
    }
    let mut snapshot_bytes = bounded_sqlite_component_bytes(&database.metadata, event_id)?;
    for admitted in sidecars.iter().flatten() {
        snapshot_bytes = snapshot_bytes
            .checked_add(bounded_sqlite_component_bytes(
                &admitted.metadata,
                event_id,
            )?)
            .ok_or_else(|| {
                CompleteContentError::new(CompleteContentErrorKind::ContentTooLarge, event_id)
            })?;
    }
    validate_observed_snapshot_reservation(reserved_snapshot_bytes, snapshot_bytes, event_id)?;
    if sidecars[2].is_some() {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceUnreadable,
            event_id,
        ));
    }
    if sidecars.iter().all(Option::is_none) {
        super::windows::verify_named_file_still_matches(
            selected_path,
            route.source_root.as_deref(),
            &database.identity,
            event_id,
        )?;
        for (source_path, admitted) in sidecar_paths.iter().zip(&sidecars) {
            super::windows::verify_optional_named_file_still_matches(
                source_path,
                route.source_root.as_deref(),
                admitted.as_ref().map(|file| &file.identity),
                event_id,
            )?;
        }
        let evidence = super::open_provider_sqlite_readonly(data_root, selected_path)
            .and_then(ReadOnlySqliteConnection::finish)
            .map_err(|cause| map_capture_error(event_id, cause))?;
        super::windows::verify_named_file_still_matches(
            selected_path,
            route.source_root.as_deref(),
            &database.identity,
            event_id,
        )?;
        for (source_path, admitted) in sidecar_paths.iter().zip(&sidecars) {
            super::windows::verify_optional_named_file_still_matches(
                source_path,
                route.source_root.as_deref(),
                admitted.as_ref().map(|file| &file.identity),
                event_id,
            )?;
        }
        return Ok(BrokeredSqliteSource::Provider {
            data_root: std::sync::Arc::new(data_root.to_path_buf()),
            path: selected_path.to_path_buf(),
            evidence,
        });
    }

    let dir = ctx_sqlite_snapshot_tempdir(data_root, event_id)?;
    let path = dir.path().join("source.sqlite");
    super::windows::copy_bounded_handle(&database, &path, database.metadata.len(), event_id)?;
    for ((suffix, admitted), _source_path) in ["-wal", "-shm", "-journal"]
        .into_iter()
        .zip(sidecars.iter())
        .zip(sidecar_paths.iter())
    {
        if suffix != "-shm" {
            let Some(admitted) = admitted else {
                continue;
            };
            let destination = sqlite_sidecar_path(&path, suffix);
            super::windows::copy_bounded_handle(
                admitted,
                &destination,
                admitted.metadata.len(),
                event_id,
            )?;
        }
    }

    super::windows::verify_named_file_still_matches(
        selected_path,
        route.source_root.as_deref(),
        &database.identity,
        event_id,
    )?;
    for (source_path, admitted) in sidecar_paths.iter().zip(&sidecars) {
        super::windows::verify_optional_named_file_still_matches(
            source_path,
            route.source_root.as_deref(),
            admitted.as_ref().map(|file| &file.identity),
            event_id,
        )?;
    }
    open_ctx_owned_sqlite_read_snapshot(&path)
        .and_then(CtxOwnedSqliteReadSnapshot::finish)
        .map_err(super::map_sqlite_source_access_error)
        .map_err(|cause| map_capture_error(event_id, cause))?;
    Ok(BrokeredSqliteSource::CtxOwned { _dir: dir, path })
}
