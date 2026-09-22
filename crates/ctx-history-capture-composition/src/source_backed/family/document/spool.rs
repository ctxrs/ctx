use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};

use crate::provider::source_backed::{
    SourceBackedRouteByteReservation, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResourceKind, SourceBackedRouteResources, SourceBackedRouteResult,
};
use ctx_history_capture_runtime::DocumentRecordSpool;
use ctx_history_core::CoreRecord;
use ctx_history_source_io::PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES;

/// One logical source may stage no more Core records than the provider-neutral
/// source-inventory entry ceiling.
const LOGICAL_SNAPSHOT_SPOOL_MAX_CORE_RECORDS: usize =
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES;
/// This matches the existing bounded source-document catalog byte ceiling.
/// Independent file scans remain bounded per leaf and by the shared worker
/// cap; large database leaves use serial direct streaming instead.
const LOGICAL_SNAPSHOT_SPOOL_MAX_ENCODED_BYTES: usize = 256 * 1024 * 1024;

pub struct DeferredCoreRecords {
    file: std::fs::File,
    budget: DeferredCoreRecordBudget,
    resources: SourceBackedRouteResources,
    scratch: Vec<SourceBackedRouteByteReservation>,
    #[cfg(test)]
    cleanup_path: Option<tempfile::TempPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeferredCoreRecordLimits {
    core_records: usize,
    encoded_bytes: usize,
}

impl DeferredCoreRecordLimits {
    const PRODUCTION: Self = Self {
        core_records: LOGICAL_SNAPSHOT_SPOOL_MAX_CORE_RECORDS,
        encoded_bytes: LOGICAL_SNAPSHOT_SPOOL_MAX_ENCODED_BYTES,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredCoreRecordBound {
    CoreRecordCount,
    EncodedBytes,
}

impl std::fmt::Display for DeferredCoreRecordBound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CoreRecordCount => "core-record-count",
            Self::EncodedBytes => "encoded-byte",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum DeferredCoreRecordAdmissionError {
    #[error(
        "logical-snapshot Core-record spool {bound} bound exceeded: \
         maximum {maximum}, observed {observed}"
    )]
    Bounds {
        bound: DeferredCoreRecordBound,
        maximum: usize,
        observed: usize,
    },
    #[error("logical-snapshot Core-record spool {bound} accounting overflowed")]
    Arithmetic { bound: DeferredCoreRecordBound },
}

#[derive(Debug)]
struct DeferredCoreRecordBudget {
    limits: DeferredCoreRecordLimits,
    core_records: usize,
    encoded_bytes: usize,
}

impl DeferredCoreRecordBudget {
    fn new(limits: DeferredCoreRecordLimits) -> Self {
        Self {
            limits,
            core_records: 0,
            encoded_bytes: 0,
        }
    }

    fn admit_core_record(&mut self) -> Result<(), DeferredCoreRecordAdmissionError> {
        let observed = self.core_records.checked_add(1).ok_or(
            DeferredCoreRecordAdmissionError::Arithmetic {
                bound: DeferredCoreRecordBound::CoreRecordCount,
            },
        )?;
        if observed > self.limits.core_records {
            return Err(DeferredCoreRecordAdmissionError::Bounds {
                bound: DeferredCoreRecordBound::CoreRecordCount,
                maximum: self.limits.core_records,
                observed,
            });
        }
        self.core_records = observed;
        Ok(())
    }

    fn check_encoded_bytes(&self, bytes: usize) -> Result<usize, DeferredCoreRecordAdmissionError> {
        let observed = self.encoded_bytes.checked_add(bytes).ok_or(
            DeferredCoreRecordAdmissionError::Arithmetic {
                bound: DeferredCoreRecordBound::EncodedBytes,
            },
        )?;
        if observed > self.limits.encoded_bytes {
            return Err(DeferredCoreRecordAdmissionError::Bounds {
                bound: DeferredCoreRecordBound::EncodedBytes,
                maximum: self.limits.encoded_bytes,
                observed,
            });
        }
        Ok(observed)
    }

    fn commit_encoded_bytes(
        &mut self,
        bytes: usize,
    ) -> Result<(), DeferredCoreRecordAdmissionError> {
        self.encoded_bytes = self.check_encoded_bytes(bytes)?;
        Ok(())
    }
}

impl DocumentRecordSpool for DeferredCoreRecords {
    fn new(resources: SourceBackedRouteResources) -> SourceBackedRouteResult<Self> {
        let file = tempfile::tempfile().map_err(|error| {
            document_internal(format!(
                "could not create private logical-snapshot staging file: {error}"
            ))
        })?;
        Ok(Self {
            file,
            budget: DeferredCoreRecordBudget::new(DeferredCoreRecordLimits::PRODUCTION),
            resources,
            scratch: Vec::new(),
            #[cfg(test)]
            cleanup_path: None,
        })
    }

    fn push(&mut self, record: CoreRecord) -> SourceBackedRouteResult<()> {
        self.budget
            .admit_core_record()
            .map_err(document_spool_admission_error)?;
        let encoded = record.encode_stored().map_err(|error| {
            document_contract_error(format!(
                "could not encode logical-snapshot staging Core record: {error}"
            ))
        })?;
        let framed = encoded
            .len()
            .checked_add(1)
            .ok_or_else(|| document_internal("logical-snapshot staging length overflowed"))?;
        self.budget
            .check_encoded_bytes(framed)
            .map_err(document_spool_admission_error)?;
        let scratch = self
            .resources
            .reserve(SourceBackedRouteResourceKind::LogicalSourceScratch, framed)?;
        self.file
            .write_all(&encoded)
            .and_then(|()| self.file.write_all(b"\n"))
            .map_err(|error| {
                document_internal(format!(
                    "could not write logical-snapshot staging Core record: {error}"
                ))
            })?;
        self.budget
            .commit_encoded_bytes(framed)
            .map_err(document_spool_admission_error)?;
        self.scratch.push(scratch);
        Ok(())
    }

    fn replay(
        mut self,
        mut emit: impl FnMut(CoreRecord) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<()> {
        self.file.flush().map_err(|error| {
            document_internal(format!(
                "could not flush logical-snapshot staging Core records: {error}"
            ))
        })?;
        self.file.seek(SeekFrom::Start(0)).map_err(|error| {
            document_internal(format!(
                "could not rewind logical-snapshot staging Core records: {error}"
            ))
        })?;
        #[cfg(test)]
        let _cleanup_path = self.cleanup_path.take();
        let scratch = std::mem::take(&mut self.scratch);
        let reserved_scratch_bytes = scratch.iter().try_fold(0_u64, |total, reservation| {
            total
                .checked_add(reservation.bytes())
                .ok_or_else(|| document_internal("logical-snapshot scratch accounting overflowed"))
        })?;
        let physical_bytes = self.file.metadata().map_err(|error| {
            document_internal(format!(
                "could not measure logical-snapshot staging file: {error}"
            ))
        })?;
        if physical_bytes.len() != reserved_scratch_bytes {
            return Err(document_internal(
                "logical-snapshot physical scratch did not match its exact reservations",
            ));
        }
        let mut reader = BufReader::new(self.file);
        let mut encoded = Vec::new();
        loop {
            encoded.clear();
            let read = reader.read_until(b'\n', &mut encoded).map_err(|error| {
                document_internal(format!(
                    "could not read logical-snapshot staging Core record: {error}"
                ))
            })?;
            if read == 0 {
                break;
            }
            if encoded.pop() != Some(b'\n') {
                return Err(document_internal(
                    "logical-snapshot staging Core record is missing its delimiter",
                ));
            }
            let record = CoreRecord::decode_stored(&encoded).map_err(|error| {
                document_internal(format!(
                    "could not decode logical-snapshot staging Core record: {error}"
                ))
            })?;
            emit(record)?;
        }
        drop(scratch);
        Ok(())
    }
}

#[cfg(test)]
impl DeferredCoreRecords {
    fn test_with_limits(
        directory: &std::path::Path,
        limits: DeferredCoreRecordLimits,
        resources: SourceBackedRouteResources,
    ) -> SourceBackedRouteResult<(Self, std::path::PathBuf)> {
        let named = tempfile::NamedTempFile::new_in(directory).map_err(|error| {
            document_internal(format!(
                "could not create test logical-snapshot staging file: {error}"
            ))
        })?;
        let path = named.path().to_path_buf();
        let (file, cleanup_path) = named.into_parts();
        Ok((
            Self {
                file,
                budget: DeferredCoreRecordBudget::new(limits),
                resources,
                scratch: Vec::new(),
                cleanup_path: Some(cleanup_path),
            },
            path,
        ))
    }
}

fn document_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn document_contract_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn document_spool_admission_error(
    error: DeferredCoreRecordAdmissionError,
) -> SourceBackedRouteError {
    let kind = match error {
        // This fixed ceiling belongs to one logical source. Classify it as a
        // recoverable source failure so the coordinator can retain that
        // source (when present) and still publish certified peers. Shared
        // route scratch exhaustion remains ResourceUnavailable at reserve().
        DeferredCoreRecordAdmissionError::Bounds { .. } => SourceBackedRouteErrorKind::Unavailable,
        DeferredCoreRecordAdmissionError::Arithmetic { .. } => SourceBackedRouteErrorKind::Internal,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput, NativeItemKey,
        NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    };

    use super::*;

    fn core_record(sequence: u64, body: &str) -> CoreRecord {
        let source = SourceKey::derive(
            CaptureProvider::Auggie.as_str(),
            "synthetic_logical_sqlite",
            "synthetic-logical-sqlite-v1",
            1,
            SourceAnchor::CatalogLineage([7; 32]),
        )
        .unwrap();
        let native_session_key =
            NativeSessionKey::native_id("synthetic.session", TypedKey::U64(1)).unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "synthetic-session",
            native_session_key: &native_session_key,
        })
        .unwrap();
        let native_item_key =
            NativeItemKey::native_id("synthetic.message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "synthetic-message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            source,
            sequence,
            "message",
            "synthetic-core-record-v1",
            body,
        )
        .unwrap();
        record.provider_session_id = Some("synthetic-session".to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.occurred_at_unix_ms = Some(sequence as i64);
        record.role = Some("user".to_owned());
        record.agent_scope = Some(ctx_history_core::AgentScope::Primary);
        record
    }

    fn encoded_frame_bytes(record: &CoreRecord) -> usize {
        record.encode_stored().unwrap().len() + 1
    }

    #[test]
    fn logical_spool_admits_n_core_records_and_rejects_n_plus_one_before_writing() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: 2,
                encoded_bytes: 1024 * 1024,
            },
            SourceBackedRouteResources::for_test(2, u64::MAX, u64::MAX),
        )
        .unwrap();
        spool.push(core_record(1, "first")).unwrap();
        spool.push(core_record(2, "second")).unwrap();
        let admitted_bytes = std::fs::metadata(&path).unwrap().len();

        let error = spool.push(core_record(3, "not admitted")).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::Unavailable);
        assert!(error.kind.is_logical_source_failure());
        assert_eq!(
            error
                .kind
                .source_failure_class()
                .map(|class| class.as_str()),
            Some("unavailable")
        );
        assert!(error.detail.contains(
            "logical-snapshot Core-record spool core-record-count bound exceeded: \
             maximum 2, observed 3"
        ));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), admitted_bytes);

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_counts_framing_and_rejects_one_oversized_core_record() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let record = core_record(1, "frame must count");
        let encoded_record_bytes = serde_json::to_vec(&record).unwrap().len();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: 1,
                encoded_bytes: encoded_record_bytes,
            },
            SourceBackedRouteResources::for_test(1, u64::MAX, u64::MAX),
        )
        .unwrap();

        let error = spool.push(record).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::Unavailable);
        assert!(error.kind.is_logical_source_failure());
        assert_eq!(
            error
                .kind
                .source_failure_class()
                .map(|class| class.as_str()),
            Some("unavailable")
        );
        assert!(error.detail.contains(&format!(
            "logical-snapshot Core-record spool encoded-byte bound exceeded: maximum \
             {encoded_record_bytes}, observed {}",
            encoded_record_bytes + 1
        )));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn logical_spool_arithmetic_error_is_systemic_internal_and_cleans_up() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: 1,
                encoded_bytes: usize::MAX,
            },
            SourceBackedRouteResources::for_test(1, u64::MAX, u64::MAX),
        )
        .unwrap();
        spool.budget.encoded_bytes = usize::MAX;

        let error = spool.push(core_record(1, "overflow")).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::Internal);
        assert_eq!(
            error.detail,
            "logical-snapshot Core-record spool encoded-byte accounting overflowed"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        drop(spool);
        assert!(!path.exists());
    }

    #[test]
    fn aggregate_physical_scratch_rejects_exactly_one_over_without_shrinking_peer_files() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first_record = core_record(1, "first physical spool");
        let second_record = core_record(2, "second physical spool");
        let first_bytes = encoded_frame_bytes(&first_record);
        let second_bytes = encoded_frame_bytes(&second_record);
        let resources = SourceBackedRouteResources::for_test(
            4,
            u64::MAX,
            u64::try_from(first_bytes + second_bytes - 1).unwrap(),
        );
        let limits = DeferredCoreRecordLimits {
            core_records: 1,
            encoded_bytes: first_bytes.max(second_bytes),
        };
        let (mut first, first_path) =
            DeferredCoreRecords::test_with_limits(temp.path(), limits, resources.clone()).unwrap();
        let (mut second, second_path) =
            DeferredCoreRecords::test_with_limits(temp.path(), limits, resources.clone()).unwrap();

        first.push(first_record).unwrap();
        let error = second.push(second_record).unwrap_err();
        assert_eq!(error.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
        assert!(error.detail.contains(&format!(
            "maximum {}, observed {}",
            first_bytes + second_bytes - 1,
            first_bytes + second_bytes
        )));
        assert_eq!(
            std::fs::metadata(&first_path).unwrap().len(),
            first_bytes as u64
        );
        assert_eq!(std::fs::metadata(&second_path).unwrap().len(), 0);
        assert_eq!(
            resources.live_bytes(SourceBackedRouteResourceKind::LogicalSourceScratch),
            first_bytes as u64
        );

        drop(first);
        drop(second);
        assert_eq!(
            resources.live_bytes(SourceBackedRouteResourceKind::LogicalSourceScratch),
            0
        );
        assert!(!first_path.exists());
        assert!(!second_path.exists());
    }

    #[test]
    fn logical_spool_replays_streamed_core_records_at_the_exact_byte_bound() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let records = [
            core_record(1, "first replay"),
            core_record(2, "second replay"),
        ];
        let expected_bytes = records.iter().map(encoded_frame_bytes).sum();
        let (mut spool, path) = DeferredCoreRecords::test_with_limits(
            temp.path(),
            DeferredCoreRecordLimits {
                core_records: records.len(),
                encoded_bytes: expected_bytes,
            },
            SourceBackedRouteResources::for_test(2, u64::MAX, u64::MAX),
        )
        .unwrap();
        for record in &records {
            spool.push(record.clone()).unwrap();
        }
        assert_eq!(spool.budget.core_records, records.len());
        assert_eq!(spool.budget.encoded_bytes, expected_bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            expected_bytes as u64
        );

        let mut replayed = Vec::new();
        spool
            .replay(|record| {
                replayed.push((
                    record.event_id,
                    record.event_sequence,
                    record.content.normalized_body,
                ));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            replayed,
            records
                .iter()
                .map(|record| (
                    record.event_id,
                    record.event_sequence,
                    record.content.normalized_body.clone()
                ))
                .collect::<Vec<_>>()
        );
        assert!(!path.exists());
    }
}
