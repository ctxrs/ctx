use super::*;

#[derive(Debug)]
pub(super) struct PreparedEventDelta {
    delta: CoreEventDelta,
    content_bytes: usize,
}

impl PreparedEventDelta {
    pub(super) fn added(record: PreparedCurrentRecord) -> Self {
        let content_bytes = record.content_bytes;
        Self {
            delta: CoreEventDelta::Added(record.record),
            content_bytes,
        }
    }

    pub(super) fn replaced(
        prior_core_record_sha256: String,
        record: PreparedCurrentRecord,
    ) -> Self {
        let content_bytes = record.content_bytes;
        Self {
            delta: CoreEventDelta::Replaced(CoreEventReplacement {
                prior_core_record_sha256,
                record: record.record,
            }),
            content_bytes,
        }
    }

    pub(super) fn tombstoned(tombstone: CoreEventTombstone) -> Self {
        Self {
            delta: CoreEventDelta::Tombstoned(tombstone),
            content_bytes: 0,
        }
    }
}

pub(super) struct EventDeltaPageBuilder {
    pub(super) deltas: Vec<PreparedEventDelta>,
    pub(super) content_bytes: usize,
}

impl EventDeltaPageBuilder {
    pub(super) fn new() -> Self {
        Self {
            deltas: Vec::new(),
            content_bytes: 0,
        }
    }

    pub(super) fn try_push(
        &mut self,
        delta: PreparedEventDelta,
    ) -> Result<Option<PreparedEventDelta>> {
        if self.deltas.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
            return Ok(Some(delta));
        }
        let Some(content_bytes) = self.content_bytes.checked_add(delta.content_bytes) else {
            return Ok(Some(delta));
        };
        if content_bytes > MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES {
            return Ok(Some(delta));
        }
        self.deltas.push(delta);
        self.content_bytes = content_bytes;
        Ok(None)
    }

    pub(super) fn push_split_overflow(&mut self, delta: PreparedEventDelta) -> Result<()> {
        if !self.deltas.is_empty() {
            bail!("internal: Core event overflow page was not empty");
        }
        self.content_bytes = delta.content_bytes;
        if self.content_bytes > MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES {
            bail!("invalid_request: Core event delta page exceeds its content bound");
        }
        self.deltas.push(delta);
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }

    pub(super) fn is_full(&self) -> bool {
        self.deltas.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS
    }

    pub(super) fn into_deltas(self) -> Vec<PreparedEventDelta> {
        self.deltas
    }
}

pub(super) struct EventDeltaPageBatchBuilder {
    pub(super) pages: Vec<CoreEventDeltaPage>,
}

impl EventDeltaPageBatchBuilder {
    pub(super) fn new() -> Self {
        Self { pages: Vec::new() }
    }

    pub(super) fn try_push(
        &mut self,
        page: CoreEventDeltaPage,
    ) -> Result<Option<CoreEventDeltaPage>> {
        if self.pages.len() == MAX_CORE_EVENT_DELTA_PAGES {
            return Ok(Some(page));
        }
        if let Some(prior) = self.pages.last() {
            let prior_source = prior.reconciliation.delta.source().identity().digest();
            let next_source = page.reconciliation.delta.source().identity().digest();
            if prior_source > next_source {
                return Ok(Some(page));
            }
        }
        self.pages.push(page);
        Ok(None)
    }

    pub(super) fn push_empty_overflow(&mut self, page: CoreEventDeltaPage) -> Result<()> {
        if !self.pages.is_empty() || self.try_push(page)?.is_some() {
            bail!("invalid_request: one Core event delta page exceeds its bounded batch");
        }
        Ok(())
    }

    pub(super) fn take_pages(&mut self) -> Vec<CoreEventDeltaPage> {
        std::mem::take(&mut self.pages)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

pub(super) fn event_delta_page(
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<PreparedEventDelta>,
) -> Result<CoreEventDeltaPage> {
    let deltas = deltas.into_iter().map(|delta| delta.delta).collect();
    Ok(CoreEventDeltaPage {
        materialization_id: materialization_id.to_owned(),
        core_generation_id: generation_id.to_owned(),
        reconciliation: reconciliation.clone(),
        page_index,
        terminal,
        deltas,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn send_event_delta_page(
    session: &mut crate::materializer::CoreMaterializationSession<'_>,
    data_root: &Path,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    pending_batch: &mut EventDeltaPageBatchBuilder,
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<PreparedEventDelta>,
) -> Result<()> {
    let page = event_delta_page(
        materialization_id,
        generation_id,
        reconciliation,
        page_index,
        terminal,
        deltas,
    )?;
    if let Some(overflow) = pending_batch.try_push(page)? {
        if pending_batch.is_empty() {
            bail!("invalid_request: one Core event delta page exceeds its bounded batch");
        }
        ensure_materialization_active(data_root, cancelled)?;
        session.ingest_event_pages(pending_batch.take_pages())?;
        pending_batch.push_empty_overflow(overflow)?;
    }
    Ok(())
}

pub(super) fn flush_event_delta_pages(
    session: &mut crate::materializer::CoreMaterializationSession<'_>,
    data_root: &Path,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    pending_batch: &mut EventDeltaPageBatchBuilder,
) -> Result<()> {
    if pending_batch.is_empty() {
        return Ok(());
    }
    ensure_materialization_active(data_root, cancelled)?;
    session.ingest_event_pages(pending_batch.take_pages())?;
    Ok(())
}
