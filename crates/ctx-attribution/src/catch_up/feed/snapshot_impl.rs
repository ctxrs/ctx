use super::*;
use crate::protocol::core_record_digests_from_encoded;

impl CoreFeedSnapshot for CoreSnapshot {
    fn generation_id(&self) -> &str {
        CoreSnapshot::generation_id(self)
    }

    fn schema(&self) -> CoreFeedSchema {
        let contract = self.contract();
        CoreFeedSchema {
            generation_manifest_version: contract.schema.manifest_version,
            identity_version: contract.schema.identity_version,
            core_record_version: contract.schema.core_record_version,
            core_record_contract_fingerprint: contract.core_record_fingerprint.clone(),
            lexical_schema_version: contract.schema.lexical_schema_version,
            lexical_analyzer_version: contract.schema.lexical_analyzer_version,
            policy_schema_hash: contract.schema.policy_schema_hash.clone(),
        }
    }

    fn source_states(&self) -> Result<Vec<CoreSourceState>> {
        if self.source_count() > MAX_CORE_SOURCE_STATES {
            bail!(
                "bounds: Core snapshot has {} sources, exceeding the attribution bound {MAX_CORE_SOURCE_STATES}",
                self.source_count()
            );
        }
        let mut cursor = None;
        let mut states = Vec::with_capacity(self.source_count());
        loop {
            let page = self
                .source_manifest_page(cursor.as_ref(), MAX_SOURCE_MANIFEST_PAGE_ITEMS)
                .map_err(anyhow::Error::new)?;
            if page.generation_id != self.generation_id() {
                bail!("core_generation_mismatch: source page escaped its pinned generation");
            }
            if page.items.len() > MAX_SOURCE_MANIFEST_PAGE_ITEMS
                || (!page.terminal && page.items.is_empty())
            {
                bail!("bounds: Core source manifest page violated its item/progress bound");
            }
            for state in page.items {
                let source_identity = hex::encode(state.source.identity().digest());
                if state.aggregate.source_identity_digest() != source_identity {
                    bail!(
                        "corrupt_core: source aggregate identity does not match its source descriptor"
                    );
                }
                states.push(CoreSourceState {
                    source: state.source,
                    core_record_accumulator: state.aggregate.core_record_accumulator().to_owned(),
                    event_count: state.aggregate.indexed_documents(),
                });
                if states.len() > MAX_CORE_SOURCE_STATES {
                    bail!("bounds: Core source manifest exceeded the attribution source bound");
                }
            }
            match (page.terminal, page.next_cursor) {
                (true, None) => break,
                (false, Some(next)) => {
                    if next.generation_id() != self.generation_id()
                        || cursor
                            .as_ref()
                            .is_some_and(|prior| next.offset() <= prior.offset())
                    {
                        bail!("corrupt_core: Core source manifest cursor did not advance");
                    }
                    cursor = Some(next);
                }
                _ => bail!("corrupt_core: Core source manifest terminal cursor is inconsistent"),
            }
        }
        if states.len() != self.source_count() {
            bail!("corrupt_core: Core source manifest count changed while pinned");
        }
        validate_ordered_source_states(&states)?;
        let indexed_documents = states.iter().try_fold(0_u64, |total, state| {
            total
                .checked_add(state.event_count)
                .ok_or_else(|| anyhow!("bounds: Core source event count overflowed"))
        })?;
        if indexed_documents != self.indexed_documents() {
            bail!("corrupt_core: Core source totals do not match snapshot metadata");
        }
        Ok(states)
    }

    fn record_page(
        &self,
        source: &ctx_history_core::SourceKey,
        cursor: Option<&CoreFeedRecordCursor>,
        limit: usize,
        budget: SnapshotPageBudget,
    ) -> Result<CoreFeedRecordPage> {
        let cursor = match cursor {
            Some(CoreFeedRecordCursor::Snapshot(cursor)) => Some(cursor),
            #[cfg(test)]
            Some(CoreFeedRecordCursor::Query(_)) => {
                bail!("internal: query cursor was supplied to a Core snapshot reader")
            }
            None => None,
        };
        let page = CoreSnapshot::record_page(self, source, cursor, limit, budget)
            .map_err(anyhow::Error::new)?;
        Ok(CoreFeedRecordPage {
            generation_id: page.generation_id,
            source: page.source,
            items: page
                .items
                .into_iter()
                .map(|item| {
                    let core_record_sha256 =
                        core_record_digests_from_encoded(&item.core_record, &item.stored_json)
                            .map_err(|error| anyhow!("invalid_request: {}", error.message))?
                            .core_record_sha256;
                    Ok(CoreFeedRecordPageItem {
                        core_record: item.core_record,
                        core_record_sha256,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            encoded_core_bytes: page.encoded_core_bytes,
            content_bytes: page.content_bytes,
            next_cursor: page.next_cursor.map(CoreFeedRecordCursor::Snapshot),
            terminal: page.terminal,
        })
    }
}

pub(super) fn validate_ordered_source_states(states: &[CoreSourceState]) -> Result<()> {
    for pair in states.windows(2) {
        if pair[0].source.identity().digest() >= pair[1].source.identity().digest() {
            bail!("corrupt_core: Core sources are not strictly ordered by stable identity");
        }
    }
    Ok(())
}

#[cfg(test)]
impl CoreFeedSnapshot for VerifiedIndex {
    fn generation_id(&self) -> &str {
        VerifiedIndex::generation_id(self)
    }

    fn schema(&self) -> CoreFeedSchema {
        let manifest = self.manifest();
        CoreFeedSchema {
            generation_manifest_version: manifest.manifest_version,
            identity_version: manifest.identity_version,
            core_record_version: manifest.core_record_version,
            core_record_contract_fingerprint: manifest.core_record_contract_fingerprint.clone(),
            lexical_schema_version: manifest.lexical_schema_version,
            lexical_analyzer_version: manifest.lexical_analyzer_version,
            policy_schema_hash: manifest.policy_schema_hash.clone(),
        }
    }

    fn source_states(&self) -> Result<Vec<CoreSourceState>> {
        core_source_states(self.manifest())
    }

    fn record_page(
        &self,
        source: &ctx_history_core::SourceKey,
        cursor: Option<&CoreFeedRecordCursor>,
        limit: usize,
        budget: SnapshotPageBudget,
    ) -> Result<CoreFeedRecordPage> {
        let cursor = match cursor {
            Some(CoreFeedRecordCursor::Query(cursor)) => Some(cursor),
            Some(CoreFeedRecordCursor::Snapshot(_)) => {
                bail!("internal: snapshot cursor was supplied to a query fixture")
            }
            None => None,
        };
        let page = self.stored_core_source_event_page_with_budget(source, cursor, limit, budget)?;
        Ok(CoreFeedRecordPage {
            generation_id: page.generation_id,
            source: page.source,
            items: page
                .items
                .into_iter()
                .map(|item| {
                    let encoded = item.stored_json.encoded_core_record()?;
                    let core_record_sha256 =
                        core_record_digests_from_encoded(&item.core_record, encoded)
                            .map_err(|error| anyhow!("invalid_request: {}", error.message))?
                            .core_record_sha256;
                    Ok(CoreFeedRecordPageItem {
                        core_record: item.core_record,
                        core_record_sha256,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            encoded_core_bytes: page.encoded_core_bytes,
            content_bytes: page.content_bytes,
            next_cursor: page.next_cursor.map(CoreFeedRecordCursor::Query),
            terminal: page.terminal,
        })
    }
}
