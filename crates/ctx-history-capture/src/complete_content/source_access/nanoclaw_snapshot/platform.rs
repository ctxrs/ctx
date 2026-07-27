//! Platform-bound capability admission for NanoClaw snapshot inputs.

use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::io;

use uuid::Uuid;

use super::super::{map_io_error, CompleteContentError, CompleteContentErrorKind, FrozenFile};
use super::content_error;

#[cfg(target_os = "windows")]
use super::super::SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES;

#[cfg(unix)]
pub(super) struct AdmittedRoot {
    file: File,
    frozen: FrozenFile,
}

#[cfg(unix)]
pub(super) fn admit_root(
    path: &Path,
    _containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<AdmittedRoot, CompleteContentError> {
    let file = super::super::open_brokered_directory(path)
        .map_err(|cause| map_io_error(event_id, cause))?;
    let metadata = file
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let frozen =
        FrozenFile::from_file(&file, &metadata).map_err(|cause| map_io_error(event_id, cause))?;
    Ok(AdmittedRoot { file, frozen })
}

#[cfg(unix)]
impl AdmittedRoot {
    pub(super) fn revalidate(
        &self,
        path: &Path,
        _containment_root: Option<&Path>,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        let held = self
            .file
            .metadata()
            .ok()
            .and_then(|metadata| FrozenFile::from_file(&self.file, &metadata).ok());
        let named = super::super::open_brokered_directory(path)
            .ok()
            .and_then(|file| file.metadata().ok())
            .and_then(|metadata| FrozenFile::from_metadata(&metadata).ok());
        if held.as_ref() != Some(&self.frozen) || named.as_ref() != Some(&self.frozen) {
            return Err(content_error(
                event_id,
                CompleteContentErrorKind::SourceChanged,
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub(super) struct AdmittedRoot {
    identity: super::super::windows::WindowsFileIdentity,
}

#[cfg(target_os = "windows")]
pub(super) fn admit_root(
    path: &Path,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<AdmittedRoot, CompleteContentError> {
    let admitted = super::super::windows::admit_directory(path, containment_root, event_id)?;
    Ok(AdmittedRoot {
        identity: admitted.identity,
    })
}

#[cfg(target_os = "windows")]
impl AdmittedRoot {
    pub(super) fn revalidate(
        &self,
        path: &Path,
        containment_root: Option<&Path>,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        super::super::windows::verify_named_directory_still_matches_within(
            path,
            containment_root,
            &self.identity,
            event_id,
        )
    }
}

pub(super) struct AdmittedFile {
    source_path: PathBuf,
    file: File,
    pub(super) frozen: FrozenFile,
    #[cfg(target_os = "windows")]
    identity: super::super::windows::WindowsFileIdentity,
}

impl AdmittedFile {
    pub(super) fn copy_to(
        &self,
        destination: &Path,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        #[cfg(unix)]
        {
            super::super::copy_bounded_handle(&self.file, destination, event_id)
        }
        #[cfg(target_os = "windows")]
        {
            let admitted = super::super::windows::AdmittedWindowsFile {
                file: self
                    .file
                    .try_clone()
                    .map_err(|cause| map_io_error(event_id, cause))?,
                metadata: self
                    .file
                    .metadata()
                    .map_err(|cause| map_io_error(event_id, cause))?,
                identity: self.identity.clone(),
            };
            super::super::windows::copy_bounded_handle(
                &admitted,
                destination,
                SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
                event_id,
            )
        }
    }

    pub(super) fn revalidate(
        &self,
        containment_root: Option<&Path>,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        #[cfg(unix)]
        {
            let _ = containment_root;
            if !super::super::revalidate_opened_file(&self.source_path, &self.file, &self.frozen) {
                return Err(content_error(
                    event_id,
                    CompleteContentErrorKind::SourceChanged,
                ));
            }
            Ok(())
        }
        #[cfg(target_os = "windows")]
        {
            super::super::windows::verify_named_file_still_matches(
                &self.source_path,
                containment_root,
                &self.identity,
                event_id,
            )
        }
    }
}

#[cfg(unix)]
pub(super) fn admit_optional_file(
    path: &Path,
    _containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<Option<AdmittedFile>, CompleteContentError> {
    let file = match super::super::open_brokered_file(path) {
        Ok(file) => file,
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => return Err(map_io_error(event_id, cause)),
    };
    let metadata = file
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let frozen =
        FrozenFile::from_file(&file, &metadata).map_err(|cause| map_io_error(event_id, cause))?;
    Ok(Some(AdmittedFile {
        source_path: path.to_path_buf(),
        file,
        frozen,
    }))
}

#[cfg(target_os = "windows")]
pub(super) fn admit_optional_file(
    path: &Path,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<Option<AdmittedFile>, CompleteContentError> {
    let admitted =
        super::super::windows::admit_optional_regular_file(path, containment_root, event_id)?;
    admitted
        .map(|admitted| {
            let frozen = FrozenFile::from_file(&admitted.file, &admitted.metadata)
                .map_err(|cause| map_io_error(event_id, cause))?;
            Ok(AdmittedFile {
                source_path: path.to_path_buf(),
                file: admitted.file,
                frozen,
                identity: admitted.identity,
            })
        })
        .transpose()
}

pub(super) fn revalidate_optional(
    path: &Path,
    observed: Option<&AdmittedFile>,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    match observed {
        Some(file) => file.revalidate(containment_root, event_id),
        None => {
            if admit_optional_file(path, containment_root, event_id)?.is_some() {
                Err(content_error(
                    event_id,
                    CompleteContentErrorKind::SourceChanged,
                ))
            } else {
                Ok(())
            }
        }
    }
}
