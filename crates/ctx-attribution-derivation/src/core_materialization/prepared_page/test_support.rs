use super::super::canonical::CanonicalCoreEventDeltaPage;
use super::super::preparation::{
    checked_prepared_output_bytes, prepared_unit_encoding, validate_prepared_unit_hard_max,
};
use super::{PreparedCoreEventDeltaPage, PreparedCoreEventDeltaPageAccumulator};
use crate::ingest::PreparedCoreUnit;
use crate::protocol::{CoreEventDeltaPage, ProtocolError};
use std::collections::BTreeMap;

impl PreparedCoreEventDeltaPage {
    pub fn page_mut(&mut self) -> &mut CoreEventDeltaPage {
        &mut self.page
    }

    pub fn set_request_sha256(&mut self, request_sha256: String) {
        self.request_sha256 = request_sha256;
    }

    pub fn set_canonical_json_len(&mut self, canonical_json_len: usize) {
        self.canonical_encoding.json_len = canonical_json_len;
    }

    pub fn set_prepared_output_encoded_len(&mut self, encoded_len: usize) {
        self.prepared_output_encoded_len = encoded_len;
    }

    pub fn for_test(
        page: CoreEventDeltaPage,
        units: BTreeMap<String, PreparedCoreUnit>,
    ) -> Result<Self, ProtocolError> {
        page.validate()?;
        let canonical = CanonicalCoreEventDeltaPage::from_typed(page)?;
        let mut accumulated = PreparedCoreEventDeltaPageAccumulator::new(&canonical);
        for (key, unit) in units {
            let encoding = prepared_unit_encoding(&key, &unit)?;
            validate_prepared_unit_hard_max(encoding)?;
            let entry_bytes = encoding.entry_bytes(!accumulated.units.is_empty())?;
            checked_prepared_output_bytes(accumulated.encoded_len, entry_bytes)?;
            accumulated.retain(key, unit, encoding, entry_bytes)?;
        }
        Ok(Self::new(canonical, accumulated))
    }
}
