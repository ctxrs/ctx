use super::*;

pub(super) const MAX_CORE_EVENT_DELTA_BATCH_EXCHANGES: usize = MAX_CORE_EVENT_DELTA_PAGES * 2 - 1;

const EMPTY_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES: usize = b"{\"pages\":[]}".len();
const HOST_ENVELOPE_SEQUENCE_PREFIX_BYTES: usize = b"{\"sequence\":".len();
const HOST_ENVELOPE_REQUEST_ID_PREFIX_BYTES: usize = b",\"request_id\":".len();
const HOST_ENVELOPE_UUID_WIRE_BYTES: usize = 38;
const HOST_ENVELOPE_EVENT_DELTA_PAGES_PREFIX_BYTES: usize =
    b",\"message\":{\"kind\":\"apply_core_event_delta_pages\",\"body\":".len();
const HOST_ENVELOPE_SUFFIX_BYTES: usize = b"}}".len();
const ADDED_EVENT_DELTA_PREFIX_BYTES: usize = b"{\"kind\":\"added\",\"value\":".len();
const ADDED_EVENT_DELTA_SUFFIX_BYTES: usize = b"}".len();
const REPLACED_EVENT_DELTA_PREFIX_BYTES: usize =
    b"{\"kind\":\"replaced\",\"value\":{\"prior_core_record_sha256\":".len();
const REPLACED_EVENT_DELTA_RECORD_PREFIX_BYTES: usize = b",\"record\":".len();
const REPLACED_EVENT_DELTA_SUFFIX_BYTES: usize = b"}}".len();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventDeltaExchangeMode {
    Normal,
    OnePagePerExchange,
}

struct EventDeltaBatchOperationBudget<N, C> {
    deadline: Instant,
    attempts: usize,
    now: N,
    is_cancelled: C,
}

impl<N, C> EventDeltaBatchOperationBudget<N, C>
where
    N: FnMut() -> Instant,
    C: FnMut() -> bool,
{
    fn remaining_for_exchange(&mut self) -> Result<Duration> {
        if (self.is_cancelled)() {
            bail!("helper_cancelled: Core event delta batch operation cancelled");
        }
        if self.attempts >= MAX_CORE_EVENT_DELTA_BATCH_EXCHANGES {
            bail!(
                "invalid_response: Core event delta batch exceeded its structural exchange bound"
            );
        }
        let now = (self.now)();
        let remaining = self.deadline.checked_duration_since(now).filter(|duration| !duration.is_zero()).ok_or_else(|| {
            anyhow!("helper_timeout: Core event delta batch operation exceeded its aggregate deadline")
        })?;
        self.attempts = self
            .attempts
            .checked_add(1)
            .ok_or_else(|| anyhow!("internal: Core event delta batch exchange count overflowed"))?;
        Ok(remaining)
    }
}

pub(super) fn apply_batched_event_delta_pages_with<F>(
    pages: Vec<CoreEventDeltaPage>,
    exchange: &mut F,
) -> Result<()>
where
    F: FnMut(&HostMessage, Duration) -> Result<HelperMessage>,
{
    let started = Instant::now();
    let deadline = started
        .checked_add(BATCH_TIMEOUT)
        .ok_or_else(|| anyhow!("internal: Core event delta batch deadline overflowed"))?;
    apply_batched_event_delta_pages_with_budget(pages, exchange, deadline, Instant::now, || false)
}

pub(super) fn apply_batched_event_delta_pages_with_budget<F, N, C>(
    pages: Vec<CoreEventDeltaPage>,
    exchange: &mut F,
    deadline: Instant,
    now: N,
    is_cancelled: C,
) -> Result<()>
where
    F: FnMut(&HostMessage, Duration) -> Result<HelperMessage>,
    N: FnMut() -> Instant,
    C: FnMut() -> bool,
{
    let mut budget = EventDeltaBatchOperationBudget {
        deadline,
        attempts: 0,
        now,
        is_cancelled,
    };
    apply_batched_event_delta_pages_recursive(pages, exchange, &mut budget)
}

fn apply_batched_event_delta_pages_recursive<F, N, C>(
    pages: Vec<CoreEventDeltaPage>,
    exchange: &mut F,
    budget: &mut EventDeltaBatchOperationBudget<N, C>,
) -> Result<()>
where
    F: FnMut(&HostMessage, Duration) -> Result<HelperMessage>,
    N: FnMut() -> Instant,
    C: FnMut() -> bool,
{
    let request = ApplyCoreEventDeltaPagesRequest { pages };
    let acknowledgement_identity = request
        .acknowledgement_identity()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let message = HostMessage::ApplyCoreEventDeltaPages(request);
    let remaining = budget.remaining_for_exchange()?;
    match exchange(&message, remaining)? {
        HelperMessage::CoreEventDeltaPagesApplied(response) => response
            .validate_for_identity(&acknowledgement_identity)
            .map_err(|error| anyhow!("invalid_response: {}", error.message)),
        HelperMessage::Error(error) if error.class == ErrorClass::Bounds => {
            let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
                unreachable!("constructed plural Core event delta request")
            };
            if request.pages.len() == 1 {
                return Err(protocol_error(error));
            }
            let mut left = request.pages;
            let right = left.split_off(left.len() / 2);
            apply_batched_event_delta_pages_recursive(left, exchange, budget)?;
            apply_batched_event_delta_pages_recursive(right, exchange, budget)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-Core-event-delta-batch response"),
    }
}

pub(super) fn apply_prepared_batched_event_delta_pages_with<F>(
    request: PreparedEventDeltaPagesRequest,
    exchange: &mut F,
) -> Result<()>
where
    F: FnMut(&PreparedEventDeltaPagesRequest, Duration) -> Result<HelperMessage>,
{
    let started = Instant::now();
    let deadline = started
        .checked_add(BATCH_TIMEOUT)
        .ok_or_else(|| anyhow!("internal: Core event delta batch deadline overflowed"))?;
    let mut budget = EventDeltaBatchOperationBudget {
        deadline,
        attempts: 0,
        now: Instant::now,
        is_cancelled: || false,
    };
    apply_prepared_batched_event_delta_pages_recursive(request, exchange, &mut budget)
}

fn apply_prepared_batched_event_delta_pages_recursive<F, N, C>(
    mut request: PreparedEventDeltaPagesRequest,
    exchange: &mut F,
    budget: &mut EventDeltaBatchOperationBudget<N, C>,
) -> Result<()>
where
    F: FnMut(&PreparedEventDeltaPagesRequest, Duration) -> Result<HelperMessage>,
    N: FnMut() -> Instant,
    C: FnMut() -> bool,
{
    let identity = request.acknowledgement_identity()?;
    let remaining = budget.remaining_for_exchange()?;
    match exchange(&request, remaining)? {
        HelperMessage::CoreEventDeltaPagesApplied(response) => response
            .validate_for_identity(&identity)
            .map_err(|error| anyhow!("invalid_response: {}", error.message)),
        HelperMessage::Error(error) if error.class == ErrorClass::Bounds => {
            if request.page_count() == 1 {
                return Err(protocol_error(error));
            }
            let right = request.split_off(request.page_count() / 2)?;
            apply_prepared_batched_event_delta_pages_recursive(request, exchange, budget)?;
            apply_prepared_batched_event_delta_pages_recursive(right, exchange, budget)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-Core-event-delta-batch response"),
    }
}

#[derive(Debug)]
pub(super) struct PreparedEventDelta {
    delta: CoreEventDelta,
    record_json: Option<PreparedCoreRecordJson>,
    content_bytes: usize,
    wire_bytes: Option<usize>,
}

impl PreparedEventDelta {
    pub(super) fn added(record: PreparedCurrentRecord) -> Self {
        let content_bytes = record.stored_json.content_bytes();
        Self {
            delta: CoreEventDelta::Added(record.record),
            record_json: Some(record.stored_json),
            content_bytes,
            wire_bytes: None,
        }
    }

    pub(super) fn replaced(
        prior_core_record_sha256: String,
        record: PreparedCurrentRecord,
    ) -> Self {
        let content_bytes = record.stored_json.content_bytes();
        Self {
            delta: CoreEventDelta::Replaced(CoreEventReplacement {
                prior_core_record_sha256,
                record: record.record,
            }),
            record_json: Some(record.stored_json),
            content_bytes,
            wire_bytes: None,
        }
    }

    pub(super) fn tombstoned(tombstone: CoreEventTombstone) -> Self {
        Self {
            delta: CoreEventDelta::Tombstoned(tombstone),
            record_json: None,
            content_bytes: 0,
            wire_bytes: None,
        }
    }

    #[cfg(test)]
    pub(super) fn from_typed(delta: CoreEventDelta) -> Result<Self> {
        let record = match &delta {
            CoreEventDelta::Added(record) => Some(record),
            CoreEventDelta::Replaced(replacement) => Some(&replacement.record),
            CoreEventDelta::Tombstoned(_) => None,
        };
        let content_bytes = event_delta_content_bytes(&delta)?;
        let record_json = record.map(serde_json::to_vec).transpose()?.map(|encoded| {
            PreparedCoreRecordJson::Shared {
                encoded: encoded.into(),
                content_bytes,
            }
        });
        let mut prepared = Self {
            delta,
            record_json,
            content_bytes,
            wire_bytes: None,
        };
        prepared.ensure_wire_bytes()?;
        Ok(prepared)
    }

    #[cfg(test)]
    pub(super) fn into_typed(self) -> CoreEventDelta {
        self.delta
    }

    #[cfg(test)]
    pub(super) const fn content_bytes(&self) -> usize {
        self.content_bytes
    }

    fn ensure_wire_bytes(&mut self) -> Result<usize> {
        if let Some(wire_bytes) = self.wire_bytes {
            return Ok(wire_bytes);
        }
        let wire_bytes = prepared_event_delta_encoded_len(self)?;
        self.wire_bytes = Some(wire_bytes);
        Ok(wire_bytes)
    }

    fn carried_wire_bytes(&self) -> Result<usize> {
        self.wire_bytes.ok_or_else(|| {
            anyhow!("internal: Core event delta reached page assembly without a carried length")
        })
    }
}

pub(super) struct EventDeltaPageBuilder {
    pub(super) deltas: Vec<PreparedEventDelta>,
    pub(super) content_bytes: usize,
    // Exact encoding length for this page with `terminal: false`.
    pub(super) wire_bytes: usize,
}

impl EventDeltaPageBuilder {
    pub(super) fn new(
        materialization_id: &str,
        generation_id: &str,
        reconciliation: &CoreSourceReconciliation,
        page_index: u32,
    ) -> Result<Self> {
        let empty_page = unvalidated_event_delta_page(
            materialization_id,
            generation_id,
            reconciliation,
            page_index,
            false,
            Vec::new(),
        );
        Ok(Self {
            deltas: Vec::new(),
            content_bytes: 0,
            wire_bytes: encoded_json_len(&empty_page)?,
        })
    }

    pub(super) fn try_push(
        &mut self,
        mut delta: PreparedEventDelta,
    ) -> Result<Option<PreparedEventDelta>> {
        if self.deltas.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
            return Ok(Some(delta));
        }

        let delta_wire_bytes = delta.ensure_wire_bytes()?;
        let Some((content_bytes, wire_bytes)) = self.prospective_bytes(&delta, delta_wire_bytes)
        else {
            return Ok(Some(delta));
        };
        if content_bytes > MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES
            || wire_bytes > MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES
        {
            return Ok(Some(delta));
        }

        self.deltas.push(delta);
        self.content_bytes = content_bytes;
        self.wire_bytes = wire_bytes;
        Ok(None)
    }

    pub(super) fn push_split_overflow(&mut self, mut delta: PreparedEventDelta) -> Result<()> {
        if !self.deltas.is_empty() {
            bail!("internal: Core event overflow page was not empty");
        }
        let delta_wire_bytes = delta.ensure_wire_bytes()?;
        let (content_bytes, wire_bytes) = self
            .prospective_bytes(&delta, delta_wire_bytes)
            .ok_or_else(|| anyhow!("invalid_request: Core event delta page bytes overflowed"))?;
        self.deltas.push(delta);
        self.content_bytes = content_bytes;
        self.wire_bytes = wire_bytes;
        Ok(())
    }

    fn prospective_bytes(
        &self,
        delta: &PreparedEventDelta,
        delta_wire_bytes: usize,
    ) -> Option<(usize, usize)> {
        let delta_content_bytes = delta.content_bytes;
        let Some(content_bytes) = self.content_bytes.checked_add(delta_content_bytes) else {
            return None;
        };
        // The empty envelope already includes `[]`. A first delta replaces the
        // empty vector contents; every later delta adds one comma as well.
        let separator_bytes = usize::from(!self.deltas.is_empty());
        let Some(wire_bytes) = self
            .wire_bytes
            .checked_add(separator_bytes)
            .and_then(|bytes| bytes.checked_add(delta_wire_bytes))
        else {
            return None;
        };
        Some((content_bytes, wire_bytes))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.deltas.is_empty()
    }

    pub(super) fn is_full(&self) -> bool {
        self.deltas.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS
    }

    pub(super) fn into_deltas_with_wire_bytes(
        self,
        terminal: bool,
    ) -> Result<(Vec<PreparedEventDelta>, usize)> {
        let wire_bytes = if terminal {
            self.wire_bytes.checked_sub("false".len() - "true".len())
        } else {
            Some(self.wire_bytes)
        }
        .ok_or_else(|| anyhow!("invalid_request: Core event delta page bytes underflowed"))?;
        Ok((self.deltas, wire_bytes))
    }
}

#[derive(Debug)]
pub(super) struct PreparedEventDeltaPage {
    pub(super) page: CoreEventDeltaPage,
    record_json: Vec<Option<PreparedCoreRecordJson>>,
    pub(super) wire_bytes: usize,
}

#[derive(Debug)]
pub(super) struct PreparedEventDeltaPagesRequest {
    request: ApplyCoreEventDeltaPagesRequest,
    record_json: Vec<Vec<Option<PreparedCoreRecordJson>>>,
    page_wire_bytes: Vec<usize>,
    encoded_request_bytes: usize,
}

#[cfg(test)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CoreRecordEncodingCounters {
    pub(super) canonical_serializations: usize,
    pub(super) canonical_serialized_bytes: usize,
    pub(super) stored_values_reused: usize,
    pub(super) stored_bytes_reused: usize,
}

impl PreparedEventDeltaPagesRequest {
    #[cfg(test)]
    pub(super) fn encoded_request_bytes(&self) -> usize {
        self.encoded_request_bytes
    }

    #[cfg(test)]
    pub(super) fn core_record_encoding_counters(&self) -> Result<CoreRecordEncodingCounters> {
        let mut counters = CoreRecordEncodingCounters::default();
        for record_json in self.record_json.iter().flatten().flatten() {
            counters.stored_values_reused = counters
                .stored_values_reused
                .checked_add(1)
                .ok_or_else(|| anyhow!("test Core record reuse count overflowed"))?;
            counters.stored_bytes_reused = counters
                .stored_bytes_reused
                .checked_add(record_json.bytes()?.len())
                .ok_or_else(|| anyhow!("test Core record reuse bytes overflowed"))?;
        }
        Ok(counters)
    }

    pub(super) fn acknowledgement_identity(
        &self,
    ) -> Result<ctx_pro_host_protocol::CoreEventDeltaPagesAcknowledgementIdentity> {
        self.request
            .acknowledgement_identity_for_prepared_request(self.encoded_request_bytes)
            .map_err(|error| anyhow!("invalid_request: {}", error.message))
    }

    pub(super) fn write_request_json(&self, writer: &mut impl Write) -> Result<()> {
        self.validate_alignment()?;
        write_all(writer, b"{\"pages\":[")?;
        for (index, (page, record_json)) in
            self.request.pages.iter().zip(&self.record_json).enumerate()
        {
            if index > 0 {
                write_all(writer, b",")?;
            }
            write_prepared_event_delta_page_json(writer, page, record_json)?;
        }
        write_all(writer, b"]}")
    }

    fn write_host_envelope_json(
        &self,
        writer: &mut impl Write,
        sequence: u64,
        request_id: uuid::Uuid,
    ) -> Result<()> {
        write_all(writer, b"{\"sequence\":")?;
        write_json_value(writer, &sequence)?;
        write_all(writer, b",\"request_id\":")?;
        write_json_value(writer, &request_id)?;
        write_all(
            writer,
            b",\"message\":{\"kind\":\"apply_core_event_delta_pages\",\"body\":",
        )?;
        self.write_request_json(writer)?;
        write_all(writer, b"}}")
    }

    pub(super) fn write_frame(
        &self,
        writer: &mut impl Write,
        sequence: u64,
        request_id: uuid::Uuid,
    ) -> Result<()> {
        let payload_bytes =
            prepared_event_delta_pages_frame_payload_bytes(sequence, self.encoded_request_bytes)?;
        if payload_bytes > ctx_pro_host_protocol::MAX_FRAME_PAYLOAD_BYTES {
            bail!(
                "invalid_request: prepared Core event frame has {} payload bytes; maximum is {}",
                payload_bytes,
                ctx_pro_host_protocol::MAX_FRAME_PAYLOAD_BYTES
            );
        }
        let payload_len = u32::try_from(payload_bytes)
            .map_err(|_| anyhow!("invalid_request: Core event frame length overflowed"))?;
        write_all(writer, ctx_pro_host_protocol::FRAME_MAGIC)?;
        write_all(
            writer,
            &ctx_pro_host_protocol::PROTOCOL_VERSION.to_be_bytes(),
        )?;
        write_all(writer, &payload_len.to_be_bytes())?;
        self.write_host_envelope_json(writer, sequence, request_id)?;
        writer
            .flush()
            .map_err(|error| anyhow!("helper_crashed: flush framed request: {error}"))
    }

    pub(super) fn into_typed_pages(self) -> Vec<CoreEventDeltaPage> {
        self.request.pages
    }

    fn page_count(&self) -> usize {
        self.request.pages.len()
    }

    fn split_off(&mut self, at: usize) -> Result<Self> {
        let pages = self.request.pages.split_off(at);
        let record_json = self.record_json.split_off(at);
        let page_wire_bytes = self.page_wire_bytes.split_off(at);
        self.remeasure()?;
        let mut right = Self {
            request: ApplyCoreEventDeltaPagesRequest { pages },
            record_json,
            page_wire_bytes,
            encoded_request_bytes: 0,
        };
        right.remeasure()?;
        Ok(right)
    }

    fn remeasure(&mut self) -> Result<()> {
        self.validate_alignment()?;
        self.encoded_request_bytes = checked_json_items_len(
            EMPTY_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES,
            self.page_wire_bytes.iter().copied().map(Ok),
            "Core event delta page batch length overflowed",
        )?;
        Ok(())
    }

    fn validate_alignment(&self) -> Result<()> {
        if self.request.pages.len() != self.record_json.len()
            || self.request.pages.len() != self.page_wire_bytes.len()
        {
            bail!("internal: prepared Core event page bytes were misaligned");
        }
        Ok(())
    }
}

pub(super) struct EventDeltaPageBatchBuilder {
    pub(super) pages: Vec<PreparedEventDeltaPage>,
    empty_wire_bytes: usize,
    pub(super) wire_bytes: usize,
}

impl EventDeltaPageBatchBuilder {
    pub(super) fn new() -> Result<Self> {
        let empty_wire_bytes = EMPTY_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES;
        Ok(Self {
            pages: Vec::new(),
            empty_wire_bytes,
            wire_bytes: empty_wire_bytes,
        })
    }

    pub(super) fn try_push(
        &mut self,
        encoded_page: PreparedEventDeltaPage,
    ) -> Result<Option<PreparedEventDeltaPage>> {
        if self.pages.len() == MAX_CORE_EVENT_DELTA_PAGES {
            return Ok(Some(encoded_page));
        }
        if let Some(prior) = self.pages.last() {
            let prior_source = prior.page.reconciliation.delta.source().identity().digest();
            let next_source = encoded_page
                .page
                .reconciliation
                .delta
                .source()
                .identity()
                .digest();
            if prior_source > next_source {
                return Ok(Some(encoded_page));
            }
        }
        let separator_bytes = usize::from(!self.pages.is_empty());
        let Some(wire_bytes) = self
            .wire_bytes
            .checked_add(separator_bytes)
            .and_then(|bytes| bytes.checked_add(encoded_page.wire_bytes))
        else {
            return Ok(Some(encoded_page));
        };
        if wire_bytes > MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES {
            return Ok(Some(encoded_page));
        }
        self.pages.push(encoded_page);
        self.wire_bytes = wire_bytes;
        Ok(None)
    }

    pub(super) fn push_empty_overflow(&mut self, page: PreparedEventDeltaPage) -> Result<()> {
        if !self.pages.is_empty() || self.try_push(page)?.is_some() {
            bail!("invalid_request: one Core event delta page exceeds its batch bound");
        }
        Ok(())
    }

    pub(super) fn take_request(&mut self) -> Result<PreparedEventDeltaPagesRequest> {
        let encoded_request_bytes = self.wire_bytes;
        self.wire_bytes = self.empty_wire_bytes;
        let pages = std::mem::take(&mut self.pages);
        let (pages_and_records, page_wire_bytes): (Vec<_>, Vec<_>) = pages
            .into_iter()
            .map(|page| ((page.page, page.record_json), page.wire_bytes))
            .unzip();
        let (pages, record_json): (Vec<_>, Vec<_>) = pages_and_records.into_iter().unzip();
        let request = PreparedEventDeltaPagesRequest {
            request: ApplyCoreEventDeltaPagesRequest { pages },
            record_json,
            page_wire_bytes,
            encoded_request_bytes,
        };
        request.validate_alignment()?;
        Ok(request)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

fn checked_encoded_sum(
    parts: impl IntoIterator<Item = usize>,
    overflow_message: &'static str,
) -> Result<usize> {
    parts.into_iter().try_fold(0_usize, |total, part| {
        total
            .checked_add(part)
            .ok_or_else(|| anyhow!("invalid_request: {overflow_message}"))
    })
}

fn checked_json_items_len(
    empty_wire_bytes: usize,
    item_wire_bytes: impl IntoIterator<Item = Result<usize>>,
    overflow_message: &'static str,
) -> Result<usize> {
    item_wire_bytes.into_iter().enumerate().try_fold(
        empty_wire_bytes,
        |total, (index, item_wire_bytes)| {
            let item_wire_bytes = item_wire_bytes?;
            total
                .checked_add(usize::from(index != 0))
                .and_then(|total| total.checked_add(item_wire_bytes))
                .ok_or_else(|| anyhow!("invalid_request: {overflow_message}"))
        },
    )
}

const fn u64_decimal_bytes(mut value: u64) -> usize {
    let mut bytes = 1;
    while value >= 10 {
        value /= 10;
        bytes += 1;
    }
    bytes
}

pub(super) fn prepared_event_delta_pages_frame_payload_bytes(
    sequence: u64,
    encoded_request_bytes: usize,
) -> Result<usize> {
    checked_encoded_sum(
        [
            HOST_ENVELOPE_SEQUENCE_PREFIX_BYTES,
            u64_decimal_bytes(sequence),
            HOST_ENVELOPE_REQUEST_ID_PREFIX_BYTES,
            HOST_ENVELOPE_UUID_WIRE_BYTES,
            HOST_ENVELOPE_EVENT_DELTA_PAGES_PREFIX_BYTES,
            encoded_request_bytes,
            HOST_ENVELOPE_SUFFIX_BYTES,
        ],
        "Core event frame length overflowed",
    )
}

#[derive(Default)]
struct EncodedLength {
    bytes: usize,
}

impl Write for EncodedLength {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("JSON encoding length overflowed"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encoded_json_len(value: &impl serde::Serialize) -> Result<usize> {
    let mut encoded = EncodedLength::default();
    serde_json::to_writer(&mut encoded, value)
        .map_err(|_| anyhow!("invalid_request: protocol encoding failed"))?;
    Ok(encoded.bytes)
}

fn write_all(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    writer
        .write_all(bytes)
        .map_err(|error| anyhow!("invalid_request: protocol encoding failed: {error}"))
}

fn write_json_value(writer: &mut impl Write, value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(writer, value)
        .map_err(|_| anyhow!("invalid_request: protocol encoding failed"))
}

fn prepared_event_delta_encoded_len(delta: &PreparedEventDelta) -> Result<usize> {
    match (&delta.delta, delta.record_json.as_ref()) {
        (CoreEventDelta::Added(_), Some(record_json)) => checked_encoded_sum(
            [
                ADDED_EVENT_DELTA_PREFIX_BYTES,
                record_json.bytes()?.len(),
                ADDED_EVENT_DELTA_SUFFIX_BYTES,
            ],
            "Core event delta length overflowed",
        ),
        (CoreEventDelta::Replaced(replacement), Some(record_json)) => checked_encoded_sum(
            [
                REPLACED_EVENT_DELTA_PREFIX_BYTES,
                encoded_json_len(&replacement.prior_core_record_sha256)?,
                REPLACED_EVENT_DELTA_RECORD_PREFIX_BYTES,
                record_json.bytes()?.len(),
                REPLACED_EVENT_DELTA_SUFFIX_BYTES,
            ],
            "Core event delta length overflowed",
        ),
        (CoreEventDelta::Tombstoned(_), None) => encoded_json_len(&delta.delta),
        _ => bail!("internal: prepared Core event delta record bytes were misaligned"),
    }
}

fn write_prepared_event_delta_json(
    writer: &mut impl Write,
    delta: &CoreEventDelta,
    record_json: Option<&PreparedCoreRecordJson>,
) -> Result<()> {
    match (delta, record_json) {
        (CoreEventDelta::Added(_), Some(record_json)) => {
            write_all(writer, b"{\"kind\":\"added\",\"value\":")?;
            #[cfg(test)]
            note_prepared_record_body_write();
            write_all(writer, record_json.bytes()?)?;
            write_all(writer, b"}")
        }
        (CoreEventDelta::Replaced(replacement), Some(record_json)) => {
            write_all(
                writer,
                b"{\"kind\":\"replaced\",\"value\":{\"prior_core_record_sha256\":",
            )?;
            write_json_value(writer, &replacement.prior_core_record_sha256)?;
            write_all(writer, b",\"record\":")?;
            #[cfg(test)]
            note_prepared_record_body_write();
            write_all(writer, record_json.bytes()?)?;
            write_all(writer, b"}}")
        }
        (CoreEventDelta::Tombstoned(_), None) => write_json_value(writer, delta),
        _ => bail!("internal: prepared Core event delta record bytes were misaligned"),
    }
}

#[cfg(test)]
std::thread_local! {
    static PREPARED_RECORD_BODY_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn note_prepared_record_body_write() {
    PREPARED_RECORD_BODY_WRITES.set(PREPARED_RECORD_BODY_WRITES.get().saturating_add(1));
}

#[cfg(test)]
pub(super) fn reset_prepared_record_body_writes() {
    PREPARED_RECORD_BODY_WRITES.set(0);
}

#[cfg(test)]
pub(super) fn prepared_record_body_writes() -> usize {
    PREPARED_RECORD_BODY_WRITES.get()
}

fn write_prepared_event_delta_page_json(
    writer: &mut impl Write,
    page: &CoreEventDeltaPage,
    record_json: &[Option<PreparedCoreRecordJson>],
) -> Result<()> {
    if page.deltas.len() != record_json.len() {
        bail!("internal: prepared Core event page record bytes were misaligned");
    }
    write_all(writer, b"{\"materialization_id\":")?;
    write_json_value(writer, &page.materialization_id)?;
    write_all(writer, b",\"core_generation_id\":")?;
    write_json_value(writer, &page.core_generation_id)?;
    write_all(writer, b",\"reconciliation\":")?;
    write_json_value(writer, &page.reconciliation)?;
    write_all(writer, b",\"page_index\":")?;
    write_json_value(writer, &page.page_index)?;
    write_all(writer, b",\"terminal\":")?;
    write_json_value(writer, &page.terminal)?;
    write_all(writer, b",\"deltas\":[")?;
    for (index, (delta, record_json)) in page.deltas.iter().zip(record_json).enumerate() {
        if index > 0 {
            write_all(writer, b",")?;
        }
        write_prepared_event_delta_json(writer, delta, record_json.as_ref())?;
    }
    write_all(writer, b"]}")
}

#[cfg(test)]
fn event_delta_content_bytes(delta: &CoreEventDelta) -> Result<usize> {
    let record = match delta {
        CoreEventDelta::Added(record) => Some(record),
        CoreEventDelta::Replaced(replacement) => Some(&replacement.record),
        CoreEventDelta::Tombstoned(_) => None,
    };
    let Some(record) = record else {
        return Ok(0);
    };
    let body_bytes = record
        .content
        .normalized_body
        .as_ref()
        .map_or(0, String::len);
    let structured_bytes = record
        .content
        .structured_content
        .as_ref()
        .map(encoded_json_len)
        .transpose()?
        .unwrap_or(0);
    let mcp_exchange_bytes = record
        .content
        .mcp_exchange
        .as_ref()
        .map(encoded_json_len)
        .transpose()?
        .unwrap_or(0);
    body_bytes
        .checked_add(structured_bytes)
        .and_then(|bytes| bytes.checked_add(mcp_exchange_bytes))
        .ok_or_else(|| anyhow!("invalid_request: Core event delta content bytes overflowed"))
}

#[cfg(test)]
pub(super) fn event_delta_page(
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<CoreEventDelta>,
) -> Result<CoreEventDeltaPage> {
    let page = unvalidated_event_delta_page(
        materialization_id,
        generation_id,
        reconciliation,
        page_index,
        terminal,
        deltas,
    );
    page.validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    Ok(page)
}

pub(super) fn prepared_event_delta_page(
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<PreparedEventDelta>,
    expected_wire_bytes: usize,
) -> Result<PreparedEventDeltaPage> {
    let empty_page = unvalidated_event_delta_page(
        materialization_id,
        generation_id,
        reconciliation,
        page_index,
        terminal,
        Vec::new(),
    );
    let carried_wire_bytes = checked_json_items_len(
        encoded_json_len(&empty_page)?,
        deltas.iter().map(PreparedEventDelta::carried_wire_bytes),
        "Core event delta page length overflowed",
    )?;
    if carried_wire_bytes != expected_wire_bytes {
        bail!("internal: carried Core event delta page length was not exact");
    }
    if carried_wire_bytes > MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES {
        bail!("invalid_request: Core event delta page exceeds its wire bound");
    }
    let (deltas, record_json): (Vec<_>, Vec<_>) = deltas
        .into_iter()
        .map(|delta| (delta.delta, delta.record_json))
        .unzip();
    let page = unvalidated_event_delta_page(
        materialization_id,
        generation_id,
        reconciliation,
        page_index,
        terminal,
        deltas,
    );
    Ok(PreparedEventDeltaPage {
        page,
        record_json,
        wire_bytes: carried_wire_bytes,
    })
}

#[cfg(test)]
pub(super) fn prepared_event_delta_page_from_typed(
    page: CoreEventDeltaPage,
) -> Result<PreparedEventDeltaPage> {
    let expected_wire_bytes = encoded_json_len(&page)?;
    let CoreEventDeltaPage {
        materialization_id,
        core_generation_id,
        reconciliation,
        page_index,
        terminal,
        deltas,
    } = page;
    let deltas = deltas
        .into_iter()
        .map(PreparedEventDelta::from_typed)
        .collect::<Result<Vec<_>>>()?;
    prepared_event_delta_page(
        &materialization_id,
        &core_generation_id,
        &reconciliation,
        page_index,
        terminal,
        deltas,
        expected_wire_bytes,
    )
}

pub(super) fn unvalidated_event_delta_page(
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<CoreEventDelta>,
) -> CoreEventDeltaPage {
    CoreEventDeltaPage {
        materialization_id: materialization_id.to_owned(),
        core_generation_id: generation_id.to_owned(),
        reconciliation: reconciliation.clone(),
        page_index,
        terminal,
        deltas,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn send_event_delta_page<C: CoreMaterializationConsumer>(
    consumer: &mut C,
    pending_batch: &mut EventDeltaPageBatchBuilder,
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<PreparedEventDelta>,
    wire_bytes: usize,
    exchange_mode: EventDeltaExchangeMode,
) -> Result<()> {
    let page = prepared_event_delta_page(
        materialization_id,
        generation_id,
        reconciliation,
        page_index,
        terminal,
        deltas,
        wire_bytes,
    )?;
    if exchange_mode == EventDeltaExchangeMode::OnePagePerExchange {
        if !pending_batch.is_empty() {
            bail!("internal: partial Core replay retained a pending event delta batch");
        }
        let mut one = EventDeltaPageBatchBuilder::new()?;
        one.push_empty_overflow(page)?;
        return consumer.apply_prepared_event_delta_pages(one.take_request()?);
    }
    if let Some(overflow) = pending_batch.try_push(page)? {
        if pending_batch.is_empty() {
            bail!("invalid_request: one Core event delta page exceeds its batch bound");
        }
        consumer.apply_prepared_event_delta_pages(pending_batch.take_request()?)?;
        pending_batch.push_empty_overflow(overflow)?;
    }
    Ok(())
}

pub(super) fn flush_event_delta_pages<C: CoreMaterializationConsumer>(
    consumer: &mut C,
    pending_batch: &mut EventDeltaPageBatchBuilder,
) -> Result<()> {
    if pending_batch.is_empty() {
        return Ok(());
    }
    consumer.apply_prepared_event_delta_pages(pending_batch.take_request()?)
}
