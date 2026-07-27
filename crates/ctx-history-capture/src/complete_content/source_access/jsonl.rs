//! Broker-owned JSONL admission and bounded record reads.
//!
//! The broker retains the admitted file handle and all auxiliary capabilities.
//! Provider resolvers receive only [`BrokeredSourceAccess`]; no path-bearing
//! state crosses the resolver boundary.

use std::{fs::File, io, path::PathBuf};

use uuid::Uuid;

use crate::complete_content::{
    jsonl::ExactJsonlSourceBinding, CompleteContentBodyDigest, CompleteContentError,
    CompleteContentErrorKind, COMPLETE_CONTENT_MAX_BODY_BYTES,
};

use super::{
    jsonl_auxiliary::{
        admit_exact_jsonl_binding, revalidate_brokered_regular_file, BrokeredJsonlAuxiliary,
    },
    validate_source_snapshot, AuthorizedSourceRoute, BrokeredSource, BrokeredSourceAccess,
    FrozenFile,
};

#[cfg(target_os = "windows")]
use super::windows;
#[cfg(unix)]
use super::{map_io_error, open_brokered_file};

pub(super) struct BrokeredJsonlSource {
    file: File,
    selected_path: PathBuf,
    frozen: FrozenFile,
    admitted_size: Option<u64>,
    exact_binding: Option<ExactJsonlSourceBinding>,
    route_root: Option<PathBuf>,
    #[cfg(target_os = "windows")]
    admitted_identity: windows::WindowsFileIdentity,
    auxiliaries: Vec<BrokeredJsonlAuxiliary>,
}

impl BrokeredSourceAccess {
    pub(crate) fn exact_jsonl_binding(&self) -> Option<&ExactJsonlSourceBinding> {
        match self.inner.as_ref() {
            BrokeredSource::Jsonl(source) => source.exact_binding.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn read_jsonl_record(
        &self,
        byte_start: u64,
        byte_end_exclusive: u64,
        expected_record_digest: &CompleteContentBodyDigest,
        event_id: Uuid,
    ) -> Result<Vec<u8>, CompleteContentError> {
        let BrokeredSource::Jsonl(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        source.read_record(
            byte_start,
            byte_end_exclusive,
            Some(expected_record_digest),
            event_id,
        )
    }

    /// Reads one broker-admitted JSONL record for a compound locator whose
    /// resolver verifies a domain-separated digest over the complete record set.
    pub(crate) fn read_jsonl_record_for_aggregate(
        &self,
        byte_start: u64,
        byte_end_exclusive: u64,
        event_id: Uuid,
    ) -> Result<Vec<u8>, CompleteContentError> {
        let BrokeredSource::Jsonl(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        source.read_record(byte_start, byte_end_exclusive, None, event_id)
    }

    pub(crate) fn read_jsonl_snapshot(
        &self,
        expected_digest: &CompleteContentBodyDigest,
        event_id: Uuid,
    ) -> Result<Vec<u8>, CompleteContentError> {
        let BrokeredSource::Jsonl(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        source.read_snapshot(expected_digest, event_id)
    }

    pub(crate) fn revalidate_jsonl(&self, event_id: Uuid) -> Result<(), CompleteContentError> {
        let BrokeredSource::Jsonl(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        source.revalidate(event_id)
    }
}

impl BrokeredJsonlSource {
    fn read_snapshot(
        &self,
        expected_digest: &CompleteContentBodyDigest,
        event_id: Uuid,
    ) -> Result<Vec<u8>, CompleteContentError> {
        let length = usize::try_from(self.frozen.length)
            .ok()
            .filter(|length| *length <= COMPLETE_CONTENT_MAX_BODY_BYTES)
            .ok_or_else(|| {
                CompleteContentError::new(CompleteContentErrorKind::ContentTooLarge, event_id)
            })?;
        if self
            .admitted_size
            .is_some_and(|admitted| admitted != self.frozen.length)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        let mut record = vec![0_u8; length];
        read_exact_at(&self.file, &mut record, 0).map_err(|_| {
            CompleteContentError::new(CompleteContentErrorKind::SourceChanged, event_id)
        })?;
        if &CompleteContentBodyDigest::from_bytes(&record) != expected_digest {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        Ok(record)
    }

    fn read_record(
        &self,
        byte_start: u64,
        byte_end_exclusive: u64,
        expected_record_digest: Option<&CompleteContentBodyDigest>,
        event_id: Uuid,
    ) -> Result<Vec<u8>, CompleteContentError> {
        let length = byte_end_exclusive
            .checked_sub(byte_start)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value <= COMPLETE_CONTENT_MAX_BODY_BYTES)
            .ok_or_else(|| {
                CompleteContentError::new(CompleteContentErrorKind::ContentTooLarge, event_id)
            })?;
        if byte_start >= byte_end_exclusive || byte_end_exclusive > self.frozen.length {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceRecordMissing,
                event_id,
            ));
        }
        if self
            .admitted_size
            .is_some_and(|size| byte_end_exclusive > size)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        if byte_start > 0 {
            let mut boundary = [0_u8; 1];
            read_exact_at(&self.file, &mut boundary, byte_start - 1).map_err(|_| {
                CompleteContentError::new(CompleteContentErrorKind::SourceChanged, event_id)
            })?;
            if boundary[0] != b'\n' {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
        }
        let mut record = vec![0_u8; length];
        read_exact_at(&self.file, &mut record, byte_start).map_err(|cause| {
            CompleteContentError::new(
                if cause.kind() == io::ErrorKind::UnexpectedEof {
                    CompleteContentErrorKind::SourceRecordMissing
                } else {
                    CompleteContentErrorKind::SourceUnreadable
                },
                event_id,
            )
        })?;
        let first_newline = record.iter().position(|byte| *byte == b'\n');
        if first_newline.is_some_and(|position| position + 1 != record.len())
            || (first_newline.is_none() && byte_end_exclusive != self.frozen.length)
        {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        let payload = record
            .strip_suffix(b"\n")
            .unwrap_or(&record)
            .strip_suffix(b"\r")
            .unwrap_or_else(|| record.strip_suffix(b"\n").unwrap_or(&record));
        let observed = CompleteContentBodyDigest::from_bytes(payload);
        if expected_record_digest.is_some_and(|expected| &observed != expected) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        finish_jsonl_read(
            || Ok(record),
            || self.verify_named_route_after_read(event_id),
        )
    }

    fn revalidate(&self, event_id: Uuid) -> Result<(), CompleteContentError> {
        if !revalidate_brokered_regular_file(
            &self.selected_path,
            Some(&self.file),
            Some(&self.frozen),
            self.route_root.as_deref(),
            event_id,
        )? {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        self.verify_named_route_after_read(event_id)?;
        for auxiliary in &self.auxiliaries {
            if !auxiliary.revalidate(self.route_root.as_deref(), event_id)? {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
        }
        Ok(())
    }

    fn verify_named_route_after_read(&self, event_id: Uuid) -> Result<(), CompleteContentError> {
        #[cfg(target_os = "windows")]
        {
            windows::verify_named_file_still_matches(
                &self.selected_path,
                self.route_root.as_deref(),
                &self.admitted_identity,
                event_id,
            )
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = event_id;
            Ok(())
        }
    }
}

pub(super) fn finish_jsonl_read<T>(
    read: impl FnOnce() -> Result<T, CompleteContentError>,
    verify_named_route: impl FnOnce() -> Result<(), CompleteContentError>,
) -> Result<T, CompleteContentError> {
    let value = read()?;
    verify_named_route()?;
    Ok(value)
}

#[cfg(unix)]
pub(super) fn admit(
    route: AuthorizedSourceRoute,
    selected_path: PathBuf,
    event_id: Uuid,
) -> Result<BrokeredJsonlSource, CompleteContentError> {
    let file = open_brokered_file(&selected_path).map_err(|cause| map_io_error(event_id, cause))?;
    let metadata = file.metadata().map_err(|_| {
        CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
    })?;
    if !metadata.file_type().is_file() {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceUnreadable,
            event_id,
        ));
    }
    let frozen = FrozenFile::from_file(&file, &metadata).map_err(|_| {
        CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
    })?;
    validate_source_snapshot(&route.source_snapshot, &metadata, event_id)?;
    let (exact_binding, auxiliaries) =
        admit_exact_jsonl_binding(&route, &selected_path, &file, &metadata, event_id)?;
    Ok(BrokeredJsonlSource {
        file,
        selected_path,
        frozen,
        admitted_size: route.source_snapshot.size_bytes,
        exact_binding,
        route_root: route.source_root,
        auxiliaries,
    })
}

#[cfg(target_os = "windows")]
pub(super) fn admit(
    route: AuthorizedSourceRoute,
    selected_path: PathBuf,
    event_id: Uuid,
) -> Result<BrokeredJsonlSource, CompleteContentError> {
    let admitted =
        windows::admit_regular_file(&selected_path, route.source_root.as_deref(), event_id)?;
    validate_source_snapshot(&route.source_snapshot, &admitted.metadata, event_id)?;
    let (exact_binding, auxiliaries) = admit_exact_jsonl_binding(
        &route,
        &selected_path,
        &admitted.file,
        &admitted.metadata,
        event_id,
    )?;
    windows::verify_named_file_still_matches(
        &selected_path,
        route.source_root.as_deref(),
        &admitted.identity,
        event_id,
    )?;
    let frozen = FrozenFile::from_file(&admitted.file, &admitted.metadata).map_err(|_| {
        CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
    })?;
    Ok(BrokeredJsonlSource {
        file: admitted.file,
        selected_path,
        frozen,
        admitted_size: route.source_snapshot.size_bytes,
        exact_binding,
        route_root: route.source_root,
        admitted_identity: admitted.identity,
        auxiliaries,
    })
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(super) fn admit(
    _route: AuthorizedSourceRoute,
    _selected_path: PathBuf,
    event_id: Uuid,
) -> Result<BrokeredJsonlSource, CompleteContentError> {
    Err(CompleteContentError::new(
        CompleteContentErrorKind::HydrationUnsupported,
        event_id,
    ))
}

#[cfg(unix)]
pub(super) fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buffer.is_empty() {
        let read = file.read_at(buffer, offset)?;
        if read == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        offset = offset.saturating_add(read as u64);
        buffer = &mut buffer[read..];
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(super) fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    windows::read_exact_at(file, buffer, offset)
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(super) fn read_exact_at(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "brokered complete-content file reads are not implemented on this platform",
    ))
}
