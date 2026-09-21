#[cfg(test)]
use super::snapshot_impl::validate_ordered_source_states;
use super::*;

#[cfg(test)]
pub(super) fn core_source_states(manifest: &GenerationManifest) -> Result<Vec<CoreSourceState>> {
    let mut states = manifest
        .sources
        .iter()
        .zip(&manifest.core_record_aggregates)
        .map(|(source, aggregate)| {
            Ok(CoreSourceState {
                source: source.observation().source().clone(),
                core_record_accumulator: aggregate.core_record_accumulator().to_owned(),
                event_count: source.counts().indexed_documents,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    states.sort_by_key(|state| state.source.identity().digest());
    validate_ordered_source_states(&states)?;
    Ok(states)
}

pub(super) fn core_generation_head(
    index: &dyn CoreFeedSnapshot,
    sources: &[CoreSourceState],
) -> Result<CoreGenerationHead> {
    let schema = index.schema();
    core_generation_head_from_schema(&schema, index.generation_id(), sources)
}

pub(super) fn core_generation_head_from_schema(
    schema: &CoreFeedSchema,
    generation_id: &str,
    sources: &[CoreSourceState],
) -> Result<CoreGenerationHead> {
    let mut head = CoreGenerationHead::new(
        generation_id,
        schema.generation_manifest_version,
        schema.identity_version,
        schema.core_record_contract_fingerprint.clone(),
        schema.lexical_schema_version,
        schema.lexical_analyzer_version,
        schema.policy_schema_hash.clone(),
        sources,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    // The public snapshot contract, not this private crate's transitive Core
    // constant, is authoritative for the exact generation being consumed.
    head.core_record_version = schema.core_record_version;
    head.validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    Ok(head)
}

pub(super) fn core_snapshot_deltas(sources: &[CoreSourceState]) -> Vec<CoreSourceDelta> {
    sources
        .iter()
        .cloned()
        .map(CoreSourceDelta::Present)
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CoreSourceDeltaPageBuildError {
    #[error("invalid_request: one Core source delta exceeds its wire bound")]
    OversizedSingleton,
    #[error("invalid_request: Core delta page index overflowed")]
    PageIndexOverflow,
    #[error("invalid_request: Core source delta page byte accounting overflowed")]
    ByteCountOverflow,
    #[error("invalid_request: Core source delta page encoding failed")]
    Encoding(#[source] serde_json::Error),
    #[error("invalid_request: {0}")]
    InvalidPage(String),
}

pub(super) fn build_delta_pages(
    materialization_id: &str,
    generation_id: &str,
    deltas: Vec<CoreSourceDelta>,
) -> Result<Vec<CoreSourceDeltaPage>, CoreSourceDeltaPageBuildError> {
    build_delta_pages_with_wire_bound(
        materialization_id,
        generation_id,
        deltas,
        MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES,
    )
}

pub(super) fn build_delta_pages_with_wire_bound(
    materialization_id: &str,
    generation_id: &str,
    deltas: Vec<CoreSourceDelta>,
    maximum_wire_bytes: usize,
) -> Result<Vec<CoreSourceDeltaPage>, CoreSourceDeltaPageBuildError> {
    if deltas.is_empty() {
        return CoreSourceDeltaPage::new(materialization_id, generation_id, 0, true, Vec::new())
            .map(|page| vec![page])
            .map_err(|error| CoreSourceDeltaPageBuildError::InvalidPage(error.message));
    }

    let mut pages = Vec::new();
    let mut page_index = 0_u32;
    let mut current = Vec::with_capacity(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS);
    let mut encoded_delta_items_bytes = 0_usize;
    let mut empty_nonterminal_wire_bytes =
        empty_source_delta_page_wire_bytes(materialization_id, generation_id, page_index, false)?;
    let mut remaining = deltas.into_iter().peekable();

    // A populated page is exactly its empty envelope plus each independently
    // encoded delta and the intervening commas. Charge every delta once, but
    // rebuild the tiny envelope whenever page_index or terminal changes.
    while let Some(delta) = remaining.next() {
        let terminal = remaining.peek().is_none();
        let encoded_delta_bytes = encoded_reconciliation_json_len(&delta)?;
        let candidate_delta_items_bytes = encoded_delta_items_bytes
            .checked_add(usize::from(!current.is_empty()))
            .and_then(|bytes| bytes.checked_add(encoded_delta_bytes))
            .ok_or(CoreSourceDeltaPageBuildError::ByteCountOverflow)?;
        let empty_wire_bytes = if terminal {
            empty_source_delta_page_wire_bytes(materialization_id, generation_id, page_index, true)?
        } else {
            empty_nonterminal_wire_bytes
        };
        let candidate_wire_bytes = empty_wire_bytes
            .checked_add(candidate_delta_items_bytes)
            .ok_or(CoreSourceDeltaPageBuildError::ByteCountOverflow)?;

        if current.len() == MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
            || candidate_wire_bytes > maximum_wire_bytes
        {
            if current.is_empty() {
                return Err(CoreSourceDeltaPageBuildError::OversizedSingleton);
            }
            pages.push(validated_source_delta_page(
                materialization_id,
                generation_id,
                page_index,
                false,
                std::mem::replace(
                    &mut current,
                    Vec::with_capacity(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS),
                ),
            )?);
            page_index = page_index
                .checked_add(1)
                .ok_or(CoreSourceDeltaPageBuildError::PageIndexOverflow)?;
            empty_nonterminal_wire_bytes = empty_source_delta_page_wire_bytes(
                materialization_id,
                generation_id,
                page_index,
                false,
            )?;
            let singleton_empty_wire_bytes = if terminal {
                empty_source_delta_page_wire_bytes(
                    materialization_id,
                    generation_id,
                    page_index,
                    true,
                )?
            } else {
                empty_nonterminal_wire_bytes
            };
            if singleton_empty_wire_bytes
                .checked_add(encoded_delta_bytes)
                .is_none_or(|bytes| bytes > maximum_wire_bytes)
            {
                return Err(CoreSourceDeltaPageBuildError::OversizedSingleton);
            }
            encoded_delta_items_bytes = encoded_delta_bytes;
            current.push(delta);
        } else {
            encoded_delta_items_bytes = candidate_delta_items_bytes;
            current.push(delta);
        }
    }

    pages.push(validated_source_delta_page(
        materialization_id,
        generation_id,
        page_index,
        true,
        current,
    )?);
    Ok(pages)
}

pub(super) fn validated_source_delta_page(
    materialization_id: &str,
    generation_id: &str,
    page_index: u32,
    terminal: bool,
    deltas: Vec<CoreSourceDelta>,
) -> Result<CoreSourceDeltaPage, CoreSourceDeltaPageBuildError> {
    CoreSourceDeltaPage::new(
        materialization_id,
        generation_id,
        page_index,
        terminal,
        deltas,
    )
    .map_err(|error| CoreSourceDeltaPageBuildError::InvalidPage(error.message))
}

pub(super) fn empty_source_delta_page_wire_bytes(
    materialization_id: &str,
    generation_id: &str,
    page_index: u32,
    terminal: bool,
) -> Result<usize, CoreSourceDeltaPageBuildError> {
    encoded_reconciliation_json_len(&CoreSourceDeltaPage {
        materialization_id: materialization_id.to_owned(),
        core_generation_id: generation_id.to_owned(),
        page_index,
        terminal,
        deltas: Vec::new(),
    })
}

fn encoded_reconciliation_json_len<T: Serialize + ?Sized>(
    value: &T,
) -> Result<usize, CoreSourceDeltaPageBuildError> {
    #[derive(Default)]
    struct Counter(usize);

    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("encoded length overflowed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value).map_err(CoreSourceDeltaPageBuildError::Encoding)?;
    Ok(counter.0)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EventReconciliationReport {
    pub(super) pages: u64,
    pub(super) mutations: u64,
}

#[expect(
    clippy::too_many_arguments,
    reason = "Reconciliation keeps the requested identities, cancellation authority, source stream, and cross-source batch explicit."
)]
pub(super) fn reconcile_source_events(
    generation_id: &str,
    materialization_id: &str,
    session: &mut crate::materializer::CoreMaterializationSession<'_>,
    data_root: &Path,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    reconciliation: CoreSourceReconciliation,
    current_pages: &mut CurrentPageStream<'_>,
    pending_batch: &mut EventDeltaPageBatchBuilder,
) -> Result<EventReconciliationReport> {
    let mut state_after = None;
    let mut state_page_index = 0_u32;
    let mut state_terminal = false;
    let mut states = VecDeque::<CoreEventState>::new();
    let mut current_terminal = current_pages.initially_terminal();
    let mut current = VecDeque::<PreparedCurrentRecord>::new();
    let mut current_credit = None;
    let mut event_page_index = 0_u32;
    let mut pending = EventDeltaPageBuilder::new();
    let mut pages = 0_u64;
    let mut mutations = 0_u64;

    loop {
        if states.is_empty() && !state_terminal {
            ensure_materialization_active(data_root, cancelled)?;
            let (page_states, terminal) = session.event_states(&reconciliation, state_after)?;
            if page_states.len() > MAX_EVENT_INDEX_PAGE_ITEMS
                || (!terminal && page_states.is_empty())
            {
                bail!("corrupt_graph: Core event state page violates direct bounds");
            }
            if page_states
                .windows(2)
                .any(|states| states[0].event_id.digest() >= states[1].event_id.digest())
                || page_states.first().is_some_and(|state| {
                    state_after.is_some_and(|after| state.event_id.digest() <= after.digest())
                })
            {
                bail!("corrupt_graph: Core event state page is not ordered after its cursor");
            }
            state_terminal = terminal;
            if let Some(last) = page_states.last() {
                state_after = Some(last.event_id);
            }
            states.extend(page_states);
            state_page_index = state_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event state page overflowed"))?;
        }
        if current.is_empty() {
            current_credit = None;
            if !current_terminal {
                let PreparedCurrentPage {
                    records,
                    terminal,
                    _encoded_credit,
                } = current_pages.next_page()?;
                current = records.into();
                current_terminal = terminal;
                current_credit = Some(_encoded_credit);
            }
        }

        let delta = match (states.front(), current.front()) {
            (None, None) if state_terminal && current_terminal => break,
            (None, None) => continue,
            (Some(state), None) => {
                let state = state.clone();
                states.pop_front();
                Some(PreparedEventDelta::tombstoned(CoreEventTombstone {
                    event_id: state.event_id,
                    prior_core_record_sha256: state.core_record_sha256,
                }))
            }
            (None, Some(_)) => {
                Some(PreparedEventDelta::added(current.pop_front().ok_or_else(
                    || anyhow!("internal: missing current Core event"),
                )?))
            }
            (Some(state), Some(record)) => {
                match state
                    .event_id
                    .digest()
                    .cmp(&record.record.event_id.digest())
                {
                    std::cmp::Ordering::Less => {
                        let state = states
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing prior Core event state"))?;
                        Some(PreparedEventDelta::tombstoned(CoreEventTombstone {
                            event_id: state.event_id,
                            prior_core_record_sha256: state.core_record_sha256,
                        }))
                    }
                    std::cmp::Ordering::Greater => {
                        Some(PreparedEventDelta::added(current.pop_front().ok_or_else(
                            || anyhow!("internal: missing current Core event"),
                        )?))
                    }
                    std::cmp::Ordering::Equal => {
                        let state = states
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing prior Core event state"))?;
                        let prepared = current
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing current Core event"))?;
                        if state.requires_replacement
                            || prepared.core_record_sha256 != state.core_record_sha256
                        {
                            Some(PreparedEventDelta::replaced(
                                state.core_record_sha256,
                                prepared,
                            ))
                        } else {
                            None
                        }
                    }
                }
            }
        };
        let Some(delta) = delta else {
            continue;
        };
        mutations = mutations
            .checked_add(1)
            .ok_or_else(|| anyhow!("invalid_request: Core event mutation count overflowed"))?;
        if pending.is_full() {
            let deltas = pending.into_deltas();
            send_event_delta_page(
                session,
                data_root,
                cancelled,
                pending_batch,
                materialization_id,
                generation_id,
                &reconciliation,
                event_page_index,
                false,
                deltas,
            )?;
            pages = pages.saturating_add(1);
            event_page_index = event_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event delta page overflowed"))?;
            pending = EventDeltaPageBuilder::new();
        }
        if let Some(overflow) = pending.try_push(delta)? {
            if pending.is_empty() {
                return Err(anyhow!(
                    "invalid_request: one Core event delta exceeds its page bound"
                ));
            }
            let deltas = pending.into_deltas();
            send_event_delta_page(
                session,
                data_root,
                cancelled,
                pending_batch,
                materialization_id,
                generation_id,
                &reconciliation,
                event_page_index,
                false,
                deltas,
            )?;
            pages = pages.saturating_add(1);
            event_page_index = event_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event delta page overflowed"))?;
            pending = EventDeltaPageBuilder::new();
            // Carry a byte-split overflow directly into the next page so a
            // final singleton is judged with its actual terminal encoding.
            pending.push_split_overflow(overflow)?;
        }
    }

    let deltas = pending.into_deltas();
    send_event_delta_page(
        session,
        data_root,
        cancelled,
        pending_batch,
        materialization_id,
        generation_id,
        &reconciliation,
        event_page_index,
        true,
        deltas,
    )?;
    pages = pages.saturating_add(1);
    drop(current_credit);
    Ok(EventReconciliationReport { pages, mutations })
}
