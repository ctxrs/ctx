use super::preparation::{ordered_parallel_map_owned, prepared_page_base_encoded_len};
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreEventJsonSerializationMetrics {
    pub full_record_traversals: u64,
    pub full_record_bytes: u64,
    pub page_serialization_operations: u64,
    pub page_serialization_bytes: u64,
    pub page_digest_traversals: u64,
    pub page_digest_bytes: u64,
    pub prepared_unit_sizing_traversals: u64,
    pub prepared_unit_sizing_bytes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct CanonicalCoreEventDeltaPageEncoding {
    pub(super) json_len: usize,
    pub(super) sha256: [u8; 32],
    pub(super) prepared_output_base_len: usize,
}

#[derive(Debug)]
pub(super) struct CanonicalCoreEventDeltaPage {
    pub(super) page: CoreEventDeltaPage,
    pub(super) canonical_json: Arc<[u8]>,
    pub(super) request_sha256: String,
    pub(super) encoding: CanonicalCoreEventDeltaPageEncoding,
    pub(super) record_sha256: Vec<(crate::protocol::StableEntityId, String)>,
    pub(super) record_leaf_sha256: Vec<(crate::protocol::StableEntityId, String)>,
    pub(super) serialization_metrics: CoreEventJsonSerializationMetrics,
}

impl CanonicalCoreEventDeltaPage {
    pub(super) fn from_typed(page: CoreEventDeltaPage) -> Result<Self, ProtocolError> {
        page.validate()?;
        let json = serde_json::to_vec(&page).map_err(|_| {
            ProtocolError::new(ErrorClass::Internal, "Core event page encoding failed")
        })?;
        if json.len() > crate::protocol::MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "Core event delta page exceeds its wire bound",
            ));
        }
        let record_digests = record_sha256_from_canonical_page(&page, &json)?;
        let full_record_bytes =
            record_digests
                .iter()
                .try_fold(0_u64, |total, (_, _, _, bytes)| {
                    total
                        .checked_add(*bytes)
                        .ok_or_else(canonical_page_overflow_error)
                })?;
        let full_record_traversals =
            u64::try_from(record_digests.len()).map_err(|_| canonical_page_overflow_error())?;
        let mut record_sha256 = Vec::with_capacity(record_digests.len());
        let mut record_leaf_sha256 = Vec::with_capacity(record_digests.len());
        for (event_id, digest, leaf_digest, _) in record_digests {
            record_sha256.push((event_id, digest));
            record_leaf_sha256.push((event_id, leaf_digest));
        }
        let page_serialization_bytes =
            u64::try_from(json.len()).map_err(|_| canonical_page_overflow_error())?;
        let page_sha256: [u8; 32] = Sha256::digest(&json).into();
        let request_sha256 = hex::encode(page_sha256);
        let prepared_output_base_len = prepared_page_base_encoded_len(&request_sha256, json.len())?;
        Ok(Self {
            page,
            canonical_json: Arc::from(json),
            request_sha256,
            encoding: CanonicalCoreEventDeltaPageEncoding {
                json_len: usize::try_from(page_serialization_bytes)
                    .map_err(|_| canonical_page_overflow_error())?,
                sha256: page_sha256,
                prepared_output_base_len,
            },
            record_sha256,
            record_leaf_sha256,
            serialization_metrics: CoreEventJsonSerializationMetrics {
                full_record_traversals,
                full_record_bytes,
                page_serialization_operations: 1,
                page_serialization_bytes,
                page_digest_traversals: 1,
                page_digest_bytes: page_serialization_bytes,
                prepared_unit_sizing_traversals: 0,
                prepared_unit_sizing_bytes: 0,
            },
        })
    }

    pub(super) fn page(&self) -> &CoreEventDeltaPage {
        &self.page
    }

    pub(super) fn prepared_output_base_len(&self) -> usize {
        self.encoding.prepared_output_base_len
    }

    pub(super) fn record_sha256(&self, event_id: crate::protocol::StableEntityId) -> Option<&str> {
        self.record_sha256
            .iter()
            .find_map(|(candidate, digest)| (*candidate == event_id).then_some(digest.as_str()))
    }
}

pub(super) fn canonicalize_event_delta_pages(
    preparer: &CoreProjectionPreparer,
    pages: Vec<CoreEventDeltaPage>,
) -> Result<Vec<CanonicalCoreEventDeltaPage>, ProtocolError> {
    if pages.is_empty() || pages.len() > crate::protocol::MAX_CORE_EVENT_DELTA_PAGES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "Core event delta batch must contain between one and sixteen pages",
        ));
    }
    ordered_parallel_map_owned(
        preparer,
        pages,
        crate::protocol::MAX_CORE_EVENT_DELTA_PAGES,
        CanonicalCoreEventDeltaPage::from_typed,
    )
}

#[derive(Deserialize)]
struct RawEventDeltaPage<'a> {
    #[serde(borrow)]
    deltas: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
struct RawEventDelta<'a> {
    kind: RawEventDeltaKind,
    #[serde(borrow)]
    value: &'a RawValue,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawEventDeltaKind {
    Added,
    Replaced,
    Tombstoned,
}

#[derive(Deserialize)]
struct RawEventReplacement<'a> {
    #[serde(borrow)]
    record: &'a RawValue,
}

fn record_sha256_from_canonical_page(
    page: &CoreEventDeltaPage,
    canonical_json: &[u8],
) -> Result<Vec<(crate::protocol::StableEntityId, String, String, u64)>, ProtocolError> {
    let raw_page: RawEventDeltaPage<'_> = serde_json::from_slice(canonical_json).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "canonical Core event page could not be decoded",
        )
    })?;
    if raw_page.deltas.len() != page.deltas.len() {
        return Err(ProtocolError::new(
            ErrorClass::Internal,
            "canonical Core event page delta count diverged",
        ));
    }
    let mut digests = Vec::new();
    for (delta, raw_delta) in page.deltas.iter().zip(raw_page.deltas) {
        let raw: RawEventDelta<'_> = serde_json::from_str(raw_delta.get()).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "canonical Core event delta could not be decoded",
            )
        })?;
        let raw_record = match (delta, raw.kind) {
            (crate::protocol::CoreEventDelta::Added(_), RawEventDeltaKind::Added) => {
                Some(raw.value)
            }
            (crate::protocol::CoreEventDelta::Replaced(_), RawEventDeltaKind::Replaced) => {
                let replacement: RawEventReplacement<'_> = serde_json::from_str(raw.value.get())
                    .map_err(|_| {
                        ProtocolError::new(
                            ErrorClass::Internal,
                            "canonical Core event replacement could not be decoded",
                        )
                    })?;
                Some(replacement.record)
            }
            (crate::protocol::CoreEventDelta::Tombstoned(_), RawEventDeltaKind::Tombstoned) => None,
            _ => {
                return Err(ProtocolError::new(
                    ErrorClass::Internal,
                    "canonical Core event delta kind diverged",
                ));
            }
        };
        if let Some(raw_record) = raw_record {
            let bytes = raw_record.get().as_bytes();
            let record = delta.record().ok_or_else(|| {
                ProtocolError::new(
                    ErrorClass::Internal,
                    "canonical Core event record payload diverged",
                )
            })?;
            let record_digests = crate::protocol::core_record_digests_from_encoded(record, bytes)?;
            digests.push((
                delta.event_id(),
                record_digests.core_record_sha256,
                record_digests.core_record_leaf_sha256,
                u64::try_from(bytes.len()).map_err(|_| canonical_page_overflow_error())?,
            ));
        }
    }
    Ok(digests)
}

pub(super) fn canonical_page_overflow_error() -> ProtocolError {
    ProtocolError::new(
        ErrorClass::Bounds,
        "canonical Core event page byte accounting overflowed",
    )
}
