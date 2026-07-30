use std::collections::BTreeSet;

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, HydratedProviderRecord, SourceRecordLocator,
};

#[cfg(test)]
use super::super::scanner::record_shelley_hydration_snapshot;
use super::{
    super::scanner::{message_units_for_rowids, SHELLEY_QUERY_BATCH_ROWS},
    *,
};

#[derive(Clone, Copy)]
struct ShelleyHydrationCoordinate {
    parent_bearing: bool,
    message_rowid: i64,
    conversation_rowid: i64,
    record_digest: [u8; 32],
}

impl ShelleySourceBackedAdapter {
    /// Reopens and verifies one exact compound message/conversation row.
    #[cfg(test)]
    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> ShelleySourceBackedResult<ShelleyHydratedMessage> {
        self.hydrate_locators(std::slice::from_ref(&locator))?
            .into_iter()
            .next()
            .ok_or(ShelleySourceBackedError::MissingRecord)
    }

    /// Hydrates one provider-grouped request through a single certified
    /// snapshot and bounded exact-row SQL sets.
    pub(crate) fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> ShelleySourceBackedResult<BatchHydrationResult> {
        let locators = request
            .events()
            .iter()
            .map(|event| event.locator())
            .collect::<Vec<_>>();
        let hydrated = self.hydrate_locators(&locators)?;
        let records = request
            .events()
            .iter()
            .zip(hydrated)
            .map(|(request, hydrated)| {
                if hydrated.event_id != request.event_id() {
                    return Err(ShelleySourceBackedError::InvalidLocator);
                }
                Ok(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes: hydrated.text.into_bytes(),
                })
            })
            .collect::<ShelleySourceBackedResult<Vec<_>>>()?;
        BatchHydrationResult::new(records).map_err(Into::into)
    }

    fn hydrate_locators(
        &self,
        locators: &[&SourceRecordLocator],
    ) -> ShelleySourceBackedResult<Vec<ShelleyHydratedMessage>> {
        if locators.is_empty() {
            return Ok(Vec::new());
        }
        let coordinates = locators
            .iter()
            .map(|locator| self.decode_hydration_coordinate(locator))
            .collect::<ShelleySourceBackedResult<Vec<_>>>()?;

        let (source_root, sqlite_snapshot) =
            open_root_authorized_snapshot(&self.data_root, &self.database_path)?;
        #[cfg(test)]
        record_shelley_hydration_snapshot();
        let evidence = sqlite_snapshot.evidence().clone();
        let hydration = (|| {
            let conn = sqlite_snapshot.connection()?;
            let conversation_columns = shelley_conversation_columns(conn)?;
            let message_columns = shelley_message_columns(conn)?;
            let has_message_sequence_id = message_columns.contains("sequence_id");
            shelley_require_message_index(conn, has_message_sequence_id)?;
            let conversation_select =
                shelley_conversation_select_expressions(&conversation_columns, "c");
            let message_select = shelley_message_select_expressions(&message_columns, "m");

            let mut hydrated = Vec::with_capacity(coordinates.len());
            for coordinates in coordinates.chunks(SHELLEY_QUERY_BATCH_ROWS) {
                let rowids = coordinates
                    .iter()
                    .map(|coordinate| coordinate.message_rowid)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let units = message_units_for_rowids(
                    conn,
                    &message_select,
                    &conversation_select,
                    has_message_sequence_id,
                    &rowids,
                )?;
                for coordinate in coordinates {
                    let Some((unit, _)) = units.get(&coordinate.message_rowid) else {
                        return Err(ShelleySourceBackedError::MissingRecord);
                    };
                    let ShelleyUnit::Accepted { rowid, value, .. } = unit else {
                        return Err(ShelleySourceBackedError::MissingRecord);
                    };
                    if *rowid != coordinate.message_rowid
                        || value.conversation.rowid != coordinate.conversation_rowid
                        || value.parent_bearing != coordinate.parent_bearing
                    {
                        return Err(ShelleySourceBackedError::MissingRecord);
                    }
                    let values = shelley_verified_record_values(
                        &value.message,
                        &value.conversation,
                        value.parent_bearing,
                    );
                    let digest = shelley_logical_record_digest(&values);
                    if digest != coordinate.record_digest {
                        return Err(ShelleySourceBackedError::StaleRecordEvidence);
                    }
                    hydrated.push(ShelleyHydratedMessage {
                        text: shelley_message_complete_text(&value.message).unwrap_or_else(|| {
                            format!("Shelley {} message", value.message.entry_type)
                        }),
                        native_record_digest: digest,
                        event_id: shelley_event_identity(&self.source, &value.message)?,
                    });
                }
            }
            Ok(hydrated)
        })();
        let closing_evidence = sqlite_snapshot.finish()?;
        source_root.revalidate()?;
        if closing_evidence != evidence {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        hydration
    }

    fn decode_hydration_coordinate(
        &self,
        locator: &SourceRecordLocator,
    ) -> ShelleySourceBackedResult<ShelleyHydrationCoordinate> {
        locator.validate_contract()?;
        if !self.source.exact_descriptor_eq(locator.source())
            || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        {
            return Err(ShelleySourceBackedError::InvalidLocator);
        }
        let (parent_bearing, message_rowid, conversation_rowid) = decode_compound_locator(locator)?;
        Ok(ShelleyHydrationCoordinate {
            parent_bearing,
            message_rowid,
            conversation_rowid,
            record_digest: *locator.record_digest(),
        })
    }
}
