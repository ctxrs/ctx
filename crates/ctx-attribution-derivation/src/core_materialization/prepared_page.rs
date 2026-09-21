use super::canonical::{
    CanonicalCoreEventDeltaPage, CanonicalCoreEventDeltaPageEncoding, canonical_page_overflow_error,
};
use super::preparation::prepared_output_overflow_error;
use super::*;

#[cfg(any(test, feature = "test-support"))]
#[path = "prepared_page/test_support.rs"]
mod test_support;

#[derive(Clone, Copy)]
pub(super) struct PreparedCoreUnitEncoding {
    pub(super) unit_len: usize,
    pub(super) entry_len_with_separator: usize,
}

impl PreparedCoreUnitEncoding {
    pub(super) fn entry_bytes(self, needs_separator: bool) -> Result<usize, ProtocolError> {
        if needs_separator {
            Ok(self.entry_len_with_separator)
        } else {
            self.entry_len_with_separator
                .checked_sub(1)
                .ok_or_else(prepared_output_overflow_error)
        }
    }
}

pub(super) struct SizedPreparedCoreUnit {
    pub(super) unit: PreparedCoreUnit,
    pub(super) encoding: PreparedCoreUnitEncoding,
}

pub(super) struct PreparedCoreEventDeltaPageAccumulator {
    pub(super) units: BTreeMap<String, PreparedCoreUnit>,
    pub(super) encoded_len: usize,
    pub(super) prepared_unit_sizing_traversals: u64,
    pub(super) prepared_unit_sizing_bytes: u64,
}

impl PreparedCoreEventDeltaPageAccumulator {
    pub(super) fn new(page: &CanonicalCoreEventDeltaPage) -> Self {
        Self {
            units: BTreeMap::new(),
            encoded_len: page.prepared_output_base_len(),
            prepared_unit_sizing_traversals: 0,
            prepared_unit_sizing_bytes: 0,
        }
    }

    pub(super) fn retain(
        &mut self,
        key: String,
        unit: PreparedCoreUnit,
        encoding: PreparedCoreUnitEncoding,
        entry_bytes: usize,
    ) -> Result<(), ProtocolError> {
        self.encoded_len = self
            .encoded_len
            .checked_add(entry_bytes)
            .ok_or_else(prepared_output_overflow_error)?;
        self.prepared_unit_sizing_traversals = self
            .prepared_unit_sizing_traversals
            .checked_add(1)
            .ok_or_else(canonical_page_overflow_error)?;
        self.prepared_unit_sizing_bytes = self
            .prepared_unit_sizing_bytes
            .checked_add(
                u64::try_from(encoding.unit_len).map_err(|_| canonical_page_overflow_error())?,
            )
            .ok_or_else(canonical_page_overflow_error)?;
        let replaced = self.units.insert(key, unit);
        debug_assert!(replaced.is_none());
        Ok(())
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct PreparedCoreEventDeltaPage {
    request_sha256: String,
    page: CoreEventDeltaPage,
    pub units: BTreeMap<String, PreparedCoreUnit>,
    #[serde(skip)]
    canonical_json: Arc<[u8]>,
    #[serde(skip)]
    canonical_encoding: CanonicalCoreEventDeltaPageEncoding,
    #[serde(skip)]
    prepared_output_encoded_len: usize,
    #[serde(skip)]
    record_sha256: Vec<(crate::protocol::StableEntityId, String)>,
    #[serde(skip)]
    record_leaf_sha256: Vec<(crate::protocol::StableEntityId, String)>,
    #[serde(skip)]
    serialization_metrics: CoreEventJsonSerializationMetrics,
}

impl PreparedCoreEventDeltaPage {
    pub(super) fn new(
        canonical: CanonicalCoreEventDeltaPage,
        accumulated: PreparedCoreEventDeltaPageAccumulator,
    ) -> Self {
        let CanonicalCoreEventDeltaPage {
            page,
            canonical_json,
            request_sha256,
            encoding,
            record_sha256,
            record_leaf_sha256,
            mut serialization_metrics,
        } = canonical;
        serialization_metrics.prepared_unit_sizing_traversals =
            accumulated.prepared_unit_sizing_traversals;
        serialization_metrics.prepared_unit_sizing_bytes = accumulated.prepared_unit_sizing_bytes;
        Self {
            request_sha256,
            page,
            units: accumulated.units,
            canonical_json,
            canonical_encoding: encoding,
            prepared_output_encoded_len: accumulated.encoded_len,
            record_sha256,
            record_leaf_sha256,
            serialization_metrics,
        }
    }

    #[must_use]
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical_json
    }

    #[must_use]
    pub fn page(&self) -> &CoreEventDeltaPage {
        &self.page
    }

    #[must_use]
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }

    #[must_use]
    pub fn serialization_metrics(&self) -> CoreEventJsonSerializationMetrics {
        self.serialization_metrics
    }

    #[must_use]
    pub fn prepared_output_encoded_len(&self) -> usize {
        self.prepared_output_encoded_len
    }

    pub fn core_record_sha256(&self, event_id: crate::protocol::StableEntityId) -> Option<&str> {
        self.record_sha256
            .iter()
            .find_map(|(candidate, digest)| (*candidate == event_id).then_some(digest.as_str()))
    }

    pub fn core_record_leaf_sha256(
        &self,
        event_id: crate::protocol::StableEntityId,
    ) -> Option<&str> {
        self.record_leaf_sha256
            .iter()
            .find_map(|(candidate, digest)| (*candidate == event_id).then_some(digest.as_str()))
    }

    pub fn validate_cached_integrity(&self) -> Result<(), ProtocolError> {
        self.page.validate()?;
        let mut canonical_sha256_hex = [0_u8; 64];
        let canonical_sha256_encoded =
            hex::encode_to_slice(self.canonical_encoding.sha256, &mut canonical_sha256_hex).is_ok();
        if self.canonical_json.len() != self.canonical_encoding.json_len
            || self.canonical_json.len() > crate::protocol::MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES
            || !canonical_sha256_encoded
            || self.request_sha256.as_bytes() != canonical_sha256_hex
            || self.prepared_output_encoded_len < self.canonical_encoding.prepared_output_base_len
            || self.prepared_output_encoded_len > MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
            || self.record_sha256.len()
                != self
                    .page
                    .deltas
                    .iter()
                    .filter(|delta| delta.record().is_some())
                    .count()
            || self.record_leaf_sha256.len() != self.record_sha256.len()
        {
            return Err(ProtocolError::new(
                ErrorClass::Internal,
                "prepared Core event page canonical metadata is inconsistent",
            ));
        }
        Ok(())
    }
}

impl PartialEq for PreparedCoreEventDeltaPage {
    fn eq(&self, other: &Self) -> bool {
        self.request_sha256 == other.request_sha256
            && self.page == other.page
            && self.units == other.units
    }
}

impl Eq for PreparedCoreEventDeltaPage {}
