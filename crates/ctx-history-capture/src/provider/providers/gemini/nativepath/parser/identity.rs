use super::*;

#[derive(Debug)]
pub(in super::super) struct GeminiNativeEventIds {
    first_raw_ordinals: BTreeMap<String, u64>,
    retained_bytes: usize,
    max_count: usize,
    max_bytes: usize,
}

impl Default for GeminiNativeEventIds {
    fn default() -> Self {
        Self {
            first_raw_ordinals: BTreeMap::new(),
            retained_bytes: 0,
            max_count: MAX_GEMINI_NATIVE_EVENT_IDS,
            max_bytes: MAX_GEMINI_NATIVE_EVENT_ID_BYTES,
        }
    }
}

impl GeminiNativeEventIds {
    #[cfg(test)]
    pub(in super::super) fn with_limits(max_count: usize, max_bytes: usize) -> Self {
        Self {
            first_raw_ordinals: BTreeMap::new(),
            retained_bytes: 0,
            max_count,
            max_bytes,
        }
    }

    #[cfg(test)]
    pub(in super::super) fn insert(
        &mut self,
        native_event_id: String,
        raw_ordinal: u64,
    ) -> GeminiScanResult<()> {
        self.validate(&native_event_id, raw_ordinal)?;
        self.commit_at(native_event_id, raw_ordinal);
        Ok(())
    }

    pub(super) fn validate(&self, native_event_id: &str, raw_ordinal: u64) -> GeminiScanResult<()> {
        if let Some(first_raw_ordinal) = self.first_raw_ordinals.get(native_event_id) {
            return Err(GeminiScanError::DuplicateNativeEventId {
                native_event_id: native_event_id.to_owned(),
                first_raw_ordinal: *first_raw_ordinal,
                duplicate_raw_ordinal: raw_ordinal,
            });
        }
        if self.first_raw_ordinals.len() >= self.max_count {
            return Err(GeminiScanError::NativeEventIdentityCountOverflow {
                limit: self.max_count,
            });
        }
        let next_bytes = self
            .retained_bytes
            .checked_add(native_event_id.len())
            .ok_or(GeminiScanError::NativeEventIdentityBytesOverflow {
                limit: self.max_bytes,
            })?;
        if next_bytes > self.max_bytes {
            return Err(GeminiScanError::NativeEventIdentityBytesOverflow {
                limit: self.max_bytes,
            });
        }
        Ok(())
    }

    pub(super) fn commit_at(&mut self, native_event_id: String, raw_ordinal: u64) {
        self.retained_bytes = self.retained_bytes.saturating_add(native_event_id.len());
        self.first_raw_ordinals.insert(native_event_id, raw_ordinal);
    }
}
