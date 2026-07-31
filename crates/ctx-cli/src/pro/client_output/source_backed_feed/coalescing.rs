use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};
use ctx_pro_host_protocol::{MaterializeSourcePageRequest, SourceProgress, SourceRecord};

use super::{MaterializationBatchItem, SourceBackedProviderPage, SourceFrontier, SourceKey};

pub(super) struct ProviderPageMaterializationItem {
    next_frontier: Option<SourceFrontier>,
    terminal: bool,
    records: Vec<SourceRecord>,
    record_count: usize,
    content_bytes: usize,
    record_payload_bytes: usize,
    event_ids: BTreeSet<[u8; 32]>,
}

impl ProviderPageMaterializationItem {
    pub(super) fn new(page: SourceBackedProviderPage, expected_source: &SourceKey) -> Result<Self> {
        let SourceBackedProviderPage {
            next_frontier,
            terminal,
            records,
            ..
        } = page;
        let mut content_bytes = 0_usize;
        let mut record_payload_bytes = 0_usize;
        let mut prior_order = None;
        let mut event_ids = BTreeSet::new();
        for record in &records {
            if !record.locator.source().exact_descriptor_eq(expected_source) {
                bail!("invalid_request: source page record belongs to another source descriptor");
            }
            let current = record_order(record);
            if prior_order.is_some_and(|prior| prior >= current) {
                bail!("invalid_request: source page records must be in strict stable event order");
            }
            if !event_ids.insert(record.event_id.digest()) {
                bail!("invalid_request: source page contains a duplicate stable event ID");
            }
            content_bytes = content_bytes
                .checked_add(
                    record
                        .validate_and_count_bytes()
                        .map_err(|error| anyhow!("invalid_request: {}", error.message))?,
                )
                .ok_or_else(|| {
                    anyhow!("invalid_request: source page transient-content bytes overflowed")
                })?;
            let encoded_bytes = serde_json::to_vec(record)
                .map_err(|error| anyhow!("internal: encode source record: {error}"))?
                .len();
            record_payload_bytes = record_payload_bytes
                .checked_add(usize::from(prior_order.is_some()))
                .and_then(|bytes| bytes.checked_add(encoded_bytes))
                .ok_or_else(|| {
                    anyhow!("invalid_request: source page encoded record bytes overflowed")
                })?;
            prior_order = Some(current);
        }
        Ok(Self {
            next_frontier,
            terminal,
            record_count: records.len(),
            records,
            content_bytes,
            record_payload_bytes,
            event_ids,
        })
    }
}

#[derive(serde::Serialize)]
struct MaterializeSourcePageShell<'a> {
    core_generation_id: &'a str,
    expected_prior: &'a SourceProgress,
    next_frontier: &'a Option<SourceFrontier>,
    terminal: bool,
    records: &'a [SourceRecord],
}

fn source_page_shell_encoded_bytes(
    core_generation_id: &str,
    expected_prior: &SourceProgress,
    next_frontier: &Option<SourceFrontier>,
    terminal: bool,
) -> Result<usize> {
    serde_json::to_vec(&MaterializeSourcePageShell {
        core_generation_id,
        expected_prior,
        next_frontier,
        terminal,
        records: &[],
    })
    .map(|encoded| encoded.len())
    .map_err(|error| anyhow!("internal: encode source materialization shell: {error}"))
}

pub(super) struct SourcePageCoalescer {
    request: MaterializeSourcePageRequest,
    record_count: usize,
    content_bytes: usize,
    record_payload_bytes: usize,
    encoded_bytes: usize,
    event_ids: BTreeSet<[u8; 32]>,
}

impl SourcePageCoalescer {
    pub(super) fn new(
        core_generation_id: String,
        expected_prior: SourceProgress,
        item: ProviderPageMaterializationItem,
    ) -> Result<Self> {
        let mut coalescer = Self {
            request: MaterializeSourcePageRequest {
                core_generation_id,
                expected_prior,
                next_frontier: None,
                terminal: false,
                records: Vec::new(),
            },
            record_count: 0,
            content_bytes: 0,
            record_payload_bytes: 0,
            encoded_bytes: 0,
            event_ids: BTreeSet::new(),
        };
        if coalescer.try_append(item)?.is_some() {
            bail!("invalid_request: source materialization page exceeds its bounded request");
        }
        Ok(coalescer)
    }

    pub(super) fn try_append(
        &mut self,
        item: ProviderPageMaterializationItem,
    ) -> Result<Option<ProviderPageMaterializationItem>> {
        if item
            .event_ids
            .iter()
            .any(|event_id| self.event_ids.contains(event_id))
        {
            bail!("invalid_request: source page contains a duplicate stable event ID");
        }
        let record_count = self
            .record_count
            .checked_add(item.record_count)
            .ok_or_else(|| anyhow!("invalid_request: source page record count overflowed"))?;
        let content_bytes = self
            .content_bytes
            .checked_add(item.content_bytes)
            .ok_or_else(|| {
                anyhow!("invalid_request: source page transient-content bytes overflowed")
            })?;
        let record_payload_bytes = self
            .record_payload_bytes
            .checked_add(usize::from(self.record_count > 0 && item.record_count > 0))
            .and_then(|bytes| bytes.checked_add(item.record_payload_bytes))
            .ok_or_else(|| {
                anyhow!("invalid_request: source page encoded record bytes overflowed")
            })?;
        let encoded_bytes = source_page_shell_encoded_bytes(
            &self.request.core_generation_id,
            &self.request.expected_prior,
            &item.next_frontier,
            item.terminal,
        )?
        .checked_add(record_payload_bytes)
        .ok_or_else(|| anyhow!("invalid_request: source page encoded bytes overflowed"))?;
        if record_count > ctx_pro_host_protocol::MAX_SOURCE_RECORDS_PER_PAGE
            || content_bytes > ctx_pro_host_protocol::MAX_SOURCE_CONTENT_BYTES_PER_PAGE
            || encoded_bytes > ctx_pro_host_protocol::MAX_SOURCE_PAGE_WIRE_BYTES
        {
            return Ok(Some(item));
        }

        let ProviderPageMaterializationItem {
            next_frontier,
            terminal,
            records,
            event_ids,
            ..
        } = item;
        self.request.next_frontier = next_frontier;
        self.request.terminal = terminal;
        self.request.records = merge_sorted_records(
            std::mem::take(&mut self.request.records),
            records,
            record_count,
        );
        self.record_count = record_count;
        self.content_bytes = content_bytes;
        self.record_payload_bytes = record_payload_bytes;
        self.encoded_bytes = encoded_bytes;
        self.event_ids.extend(event_ids);
        Ok(None)
    }

    pub(super) fn next_progress(&self) -> SourceProgress {
        self.request.next_progress()
    }

    pub(super) fn terminal(&self) -> bool {
        self.request.terminal
    }

    pub(super) fn finish(self) -> MaterializationBatchItem {
        #[cfg(test)]
        assert_eq!(
            self.encoded_bytes,
            serde_json::to_vec(&self.request)
                .expect("encode coalesced source materialization request")
                .len(),
            "incremental source-page wire accounting diverged from authoritative JSON"
        );
        MaterializationBatchItem {
            request: self.request,
            record_count: self.record_count,
            content_bytes: self.content_bytes,
            encoded_bytes: self.encoded_bytes,
        }
    }
}

fn merge_sorted_records(
    left: Vec<SourceRecord>,
    right: Vec<SourceRecord>,
    capacity: usize,
) -> Vec<SourceRecord> {
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    let mut merged = Vec::with_capacity(capacity);
    while left.peek().is_some() && right.peek().is_some() {
        let take_left = record_order(left.peek().expect("left record"))
            < record_order(right.peek().expect("right record"));
        merged.push(if take_left {
            left.next().expect("left record")
        } else {
            right.next().expect("right record")
        });
    }
    merged.extend(left);
    merged.extend(right);
    merged
}

fn record_order(record: &SourceRecord) -> (u64, [u8; 32]) {
    (record.metadata.event_sequence, record.event_id.digest())
}
