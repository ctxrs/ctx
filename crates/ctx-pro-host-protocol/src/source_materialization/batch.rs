/// One atomic, bounded set of distinct-source materialization pages.
///
/// Each nested request retains the existing per-source CAS and frontier
/// contract. Helpers must commit every page or none of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializeSourcePagesRequest {
    pub pages: Vec<MaterializeSourcePageRequest>,
}

impl MaterializeSourcePagesRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.pages.is_empty() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source materialization batch must contain at least one page",
            ));
        }
        if self.pages.len() > MAX_SOURCE_MATERIALIZATION_BATCH_ITEMS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source materialization batch exceeds its page count bound",
            ));
        }
        let mut core_generation_id = None;
        let mut prior_source = None;
        let mut record_count = 0_usize;
        let mut content_bytes = 0_usize;
        for page in &self.pages {
            page.validate()?;
            if core_generation_id
                .is_some_and(|generation: &str| generation != page.core_generation_id)
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "source materialization batch mixes Core generations",
                ));
            }
            core_generation_id = Some(page.core_generation_id.as_str());
            let source_id = page.expected_prior.source.identity().digest();
            if prior_source.is_some_and(|prior| prior >= source_id) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source materialization batch pages must be sorted and unique by source identity",
                ));
            }
            prior_source = Some(source_id);
            record_count = record_count
                .checked_add(page.records.len())
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "source materialization batch record count overflowed",
                    )
                })?;
            if record_count > MAX_SOURCE_MATERIALIZATION_BATCH_RECORDS {
                return Err(ProtocolError::new(
                    ErrorClass::Bounds,
                    "source materialization batch exceeds its aggregate record count bound",
                ));
            }
            for record in &page.records {
                content_bytes = content_bytes
                    .checked_add(record.validate_and_count_bytes()?)
                    .ok_or_else(|| {
                        ProtocolError::new(
                            ErrorClass::Bounds,
                            "source materialization batch transient-content byte total overflowed",
                        )
                    })?;
                if content_bytes > MAX_SOURCE_MATERIALIZATION_BATCH_CONTENT_BYTES {
                    return Err(ProtocolError::new(
                        ErrorClass::Bounds,
                        "source materialization batch exceeds its aggregate transient-content byte bound",
                    ));
                }
            }
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_MATERIALIZATION_BATCH_WIRE_BYTES,
            "source materialization batch exceeds its encoded byte bound",
        )
    }
}
