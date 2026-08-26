use super::*;
use crate::records::stored_event_record_with_size;

impl EventCandidateQueryReceipt {
    fn record_query_execution(&mut self) -> Result<()> {
        self.query_executions = self
            .query_executions
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn record_collector_hits(&mut self, hits: usize) -> Result<()> {
        let hits = u64::try_from(hits).map_err(|_| IndexError::CountOverflow)?;
        self.collector_hits = self
            .collector_hits
            .checked_add(hits)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    fn record_decoded(&mut self, encoded_core_bytes: usize) -> Result<()> {
        let encoded_core_bytes =
            u64::try_from(encoded_core_bytes).map_err(|_| IndexError::CountOverflow)?;
        let records_decoded = self
            .records_decoded
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        let encoded_core_bytes_decoded = self
            .encoded_core_bytes_decoded
            .checked_add(encoded_core_bytes)
            .ok_or(IndexError::CountOverflow)?;
        self.records_decoded = records_decoded;
        self.encoded_core_bytes_decoded = encoded_core_bytes_decoded;
        Ok(())
    }
}

use ctx_history_index_format::BODY_ANALYZER;
use std::sync::Arc;
use tantivy::{
    fieldnorm::FieldNormReader,
    postings::{Postings as _, TermInfo},
    query::Bm25Weight,
};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static MANUAL_INVERTED_INDEX_ACQUISITIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MANUAL_POSTING_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MANUAL_LIVE_POSTINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MANUAL_MAXIMUM_LIVE_POSTINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MANUAL_EVENT_RANGE_ORDER_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const EVENT_RANGE_ORDER_FAST_FIELD: &str = "event_range_order";

mod executor;

#[derive(Debug, Clone, Copy)]
enum ManualLexicalMode<'a> {
    Body(&'a [&'a str]),
    List,
}

/// Stable segment identity plus the small body metadata required for exact
/// corpus-wide BM25 statistics. No cursor or reader is retained here.
struct PreparedBodySegment {
    context: LexicalSegmentContext,
    body_term_infos: Vec<Option<TermInfo>>,
}

struct PreparedSegment {
    context: LexicalSegmentContext,
    body_postings: Vec<Option<SegmentPostings>>,
    fieldnorms: Option<FieldNormReader>,
    filters: SegmentFilters,
    classes: SegmentClasses,
    candidate_fields: ManualCandidateFastFields,
}

impl PreparedSegment {
    fn new(
        context: LexicalSegmentContext,
        body_postings: Vec<Option<SegmentPostings>>,
        fieldnorms: Option<FieldNormReader>,
        filters: SegmentFilters,
        classes: SegmentClasses,
        candidate_fields: ManualCandidateFastFields,
    ) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        record_live_postings_added(
            body_postings.iter().flatten().count()
                + filters.posting_count()
                + classes.posting_count(),
        );
        Self {
            context,
            body_postings,
            fieldnorms,
            filters,
            classes,
            candidate_fields,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn posting_count(&self) -> usize {
        self.body_postings.iter().flatten().count()
            + self.filters.posting_count()
            + self.classes.posting_count()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for PreparedSegment {
    fn drop(&mut self) {
        let posting_count = self.posting_count();
        MANUAL_LIVE_POSTINGS.set(MANUAL_LIVE_POSTINGS.get() - posting_count);
    }
}

struct SegmentFilters {
    required: Vec<Vec<SegmentPostings>>,
    prohibited: Vec<Vec<SegmentPostings>>,
    workspace: Option<SegmentBitmap>,
    file: Option<SegmentBitmap>,
}

impl SegmentFilters {
    #[cfg(any(test, feature = "test-support"))]
    fn posting_count(&self) -> usize {
        self.required.iter().map(Vec::len).sum::<usize>()
            + self.prohibited.iter().map(Vec::len).sum::<usize>()
    }

    fn matches_none(&self) -> bool {
        self.required.iter().any(Vec::is_empty)
            || self.workspace.as_ref().is_some_and(|bitmap| !bitmap.any)
            || self.file.as_ref().is_some_and(|bitmap| !bitmap.any)
    }

    fn accepts(
        &mut self,
        doc: DocId,
        meter: &mut LexicalWorkMeter,
        segment: &LexicalSegmentContext,
    ) -> Result<Option<bool>> {
        for group in &mut self.required {
            let Some(matches) = posting_group_contains(group, doc, meter, segment) else {
                return Ok(None);
            };
            if !matches {
                return Ok(Some(false));
            }
        }
        for group in &mut self.prohibited {
            let Some(matches) = posting_group_contains(group, doc, meter, segment) else {
                return Ok(None);
            };
            if matches {
                return Ok(Some(false));
            }
        }
        for bitmap in [self.workspace.as_ref(), self.file.as_ref()] {
            let Some(bitmap) = bitmap else {
                continue;
            };
            if !meter.charge(
                LexicalWorkCounter::FilterProbes,
                1,
                Some(segment),
                Some(doc),
            ) {
                return Ok(None);
            }
            if !bitmap.contains(doc)? {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }
}

struct SegmentBitmap {
    words: Vec<u64>,
    any: bool,
}

/// Segment-cached fields for cheap filtering and exact boundary decoding.
struct ManualCandidateFastFields {
    occurred_at_unix_ms: tantivy::columnar::Column<i64>,
    event_id_high: tantivy::columnar::Column<u64>,
    event_id_low: tantivy::columnar::Column<u64>,
    event_range_order: tantivy::columnar::BytesColumn,
    event_range_order_buffer: Vec<u8>,
}

impl ManualCandidateFastFields {
    fn open(reader: &SegmentReader) -> Result<Self> {
        Ok(Self {
            occurred_at_unix_ms: reader.fast_fields().i64(OCCURRED_AT_UNIX_MS_FIELD)?,
            event_id_high: reader.fast_fields().u64(EVENT_ID_HIGH_FIELD)?,
            event_id_low: reader.fast_fields().u64(EVENT_ID_LOW_FIELD)?,
            event_range_order: reader
                .fast_fields()
                .bytes(EVENT_RANGE_ORDER_FAST_FIELD)?
                .ok_or(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ))?,
            event_range_order_buffer: Vec::with_capacity(
                ctx_history_index_format::EVENT_RANGE_ORDER_KEY_LEN,
            ),
        })
    }

    fn occurred_at_unix_ms(&self, doc: DocId) -> Result<Option<i64>> {
        let mut values = self.occurred_at_unix_ms.values_for_doc(doc);
        let value = values.next();
        if values.next().is_some() {
            return Err(IndexError::InvalidStoredDocumentField(
                OCCURRED_AT_UNIX_MS_FIELD,
            ));
        }
        Ok(value)
    }

    fn event_id_high(&self, doc: DocId) -> Result<u64> {
        unique_manual_fast_u64(&self.event_id_high, doc, EVENT_ID_HIGH_FIELD)
    }

    fn exact_identity_matches_compact_fields(
        &self,
        doc: DocId,
        digest: [u8; 32],
        cached_high: Option<u64>,
    ) -> Result<bool> {
        let compact = CompactIdentity { digest }.as_uuid().as_u128();
        let high = match cached_high {
            Some(high) => high,
            None => self.event_id_high(doc)?,
        };
        let low = unique_manual_fast_u64(&self.event_id_low, doc, EVENT_ID_LOW_FIELD)?;
        Ok(high == (compact >> 64) as u64 && low == compact as u64)
    }

    fn event_range_order(
        &mut self,
        doc: DocId,
    ) -> Result<ctx_history_index_format::EventRangeOrderKey> {
        let term_ord = {
            let mut term_ords = self.event_range_order.term_ords(doc);
            let term_ord = term_ords
                .next()
                .ok_or(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ))?;
            if term_ords.next().is_some() {
                return Err(IndexError::InvalidStoredDocumentField(
                    EVENT_RANGE_ORDER_FAST_FIELD,
                ));
            }
            term_ord
        };
        self.event_range_order_buffer.clear();
        if !self
            .event_range_order
            .ord_to_bytes(term_ord, &mut self.event_range_order_buffer)?
        {
            return Err(IndexError::InvalidStoredDocumentField(
                EVENT_RANGE_ORDER_FAST_FIELD,
            ));
        }
        #[cfg(any(test, feature = "test-support"))]
        MANUAL_EVENT_RANGE_ORDER_DECODES
            .set(MANUAL_EVENT_RANGE_ORDER_DECODES.get().saturating_add(1));
        ctx_history_index_format::EventRangeOrderKey::decode(&self.event_range_order_buffer)
    }
}

fn unique_manual_fast_u64(
    column: &tantivy::columnar::Column<u64>,
    doc: DocId,
    field_name: &'static str,
) -> Result<u64> {
    let mut values = column.values_for_doc(doc);
    let value = values
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

impl SegmentBitmap {
    fn insert(&mut self, doc: DocId) -> Result<()> {
        let doc = usize::try_from(doc).map_err(|_| IndexError::CountOverflow)?;
        let word = self
            .words
            .get_mut(doc / u64::BITS as usize)
            .ok_or(IndexError::InvalidStoredDocumentField("literal_fact"))?;
        *word |= 1_u64 << (doc % u64::BITS as usize);
        self.any = true;
        Ok(())
    }

    fn contains(&self, doc: DocId) -> Result<bool> {
        let doc = usize::try_from(doc).map_err(|_| IndexError::CountOverflow)?;
        let word = self
            .words
            .get(doc / u64::BITS as usize)
            .ok_or(IndexError::InvalidStoredDocumentField("literal_fact"))?;
        Ok(word & (1_u64 << (doc % u64::BITS as usize)) != 0)
    }
}

enum SegmentClasses {
    Unweighted,
    Weighted(Box<WeightedSegmentClasses>),
}

struct WeightedSegmentClasses {
    message: Option<SegmentPostings>,
    summary: Option<SegmentPostings>,
    outputs: Vec<SegmentPostings>,
    scope: SearchContentScope,
}

impl SegmentClasses {
    #[cfg(any(test, feature = "test-support"))]
    fn posting_count(&self) -> usize {
        match self {
            Self::Unweighted => 0,
            Self::Weighted(weighted) => {
                usize::from(weighted.message.is_some())
                    + usize::from(weighted.summary.is_some())
                    + weighted.outputs.len()
            }
        }
    }

    fn weight(
        &mut self,
        doc: DocId,
        meter: &mut LexicalWorkMeter,
        segment: &LexicalSegmentContext,
    ) -> Option<Score> {
        let Self::Weighted(weighted) = self else {
            return Some(1.0);
        };
        if posting_option_contains(&mut weighted.message, doc, meter, segment)? {
            return Some(1.0);
        }
        if posting_option_contains(&mut weighted.summary, doc, meter, segment)? {
            return Some(0.9);
        }
        if posting_group_contains(&mut weighted.outputs, doc, meter, segment)? {
            return Some(0.6);
        }
        Some(match weighted.scope {
            SearchContentScope::All => 0.8,
            SearchContentScope::Transcript => 0.9,
            SearchContentScope::Calls | SearchContentScope::Outputs => unreachable!(),
        })
    }
}

enum PostingOpen {
    Opened { postings: Box<SegmentPostings> },
    Missing,
    Exhausted,
}

/// Tantivy's `Bm25Weight` is used only as a pure formula value. Both corpus
/// inputs come from the explicitly metered manual term-info prepass, and the
/// executor calls only `score(fieldnorm_id, term_frequency)` afterward. No
/// `Query`, `Weight`, `Scorer`, `TopDocs`, or `Searcher` execution is hidden in
/// this helper.
fn explicit_bm25_weight(
    doc_frequency: u64,
    total_num_docs: u64,
    average_fieldnorm: Score,
) -> Bm25Weight {
    Bm25Weight::for_one_term_without_explain(doc_frequency, total_num_docs, average_fieldnorm)
}

fn manual_inverted_index(reader: &SegmentReader, field: Field) -> Result<Arc<InvertedIndexReader>> {
    #[cfg(any(test, feature = "test-support"))]
    MANUAL_INVERTED_INDEX_ACQUISITIONS
        .set(MANUAL_INVERTED_INDEX_ACQUISITIONS.get().saturating_add(1));
    Ok(reader.inverted_index(field)?)
}

fn manual_read_postings(
    inverted: &InvertedIndexReader,
    term_info: &TermInfo,
    option: IndexRecordOption,
) -> Result<SegmentPostings> {
    #[cfg(any(test, feature = "test-support"))]
    MANUAL_POSTING_READS.set(MANUAL_POSTING_READS.get().saturating_add(1));
    Ok(inverted.read_postings_from_terminfo(term_info, option)?)
}

#[cfg(any(test, feature = "test-support"))]
fn record_live_postings_added(posting_count: usize) {
    let live = MANUAL_LIVE_POSTINGS.get().saturating_add(posting_count);
    MANUAL_LIVE_POSTINGS.set(live);
    MANUAL_MAXIMUM_LIVE_POSTINGS.set(MANUAL_MAXIMUM_LIVE_POSTINGS.get().max(live));
}

fn open_manual_segment(
    reader: &SegmentReader,
    prepared: PreparedBodySegment,
    filter_plan: &LexicalFilterAdapter,
    content_scope: SearchContentScope,
    fields: Fields,
    meter: &mut LexicalWorkMeter,
) -> Result<Option<PreparedSegment>> {
    let PreparedBodySegment {
        context,
        body_term_infos,
    } = prepared;
    let Some(body_postings) = open_body_postings(
        reader,
        fields.body_search,
        &body_term_infos,
        meter,
        &context,
    )?
    else {
        return Ok(None);
    };
    let fieldnorms = if body_term_infos.is_empty() {
        None
    } else {
        Some(reader.get_fieldnorms_reader(fields.body_search)?)
    };
    let Some(filters) = open_segment_filters(reader, filter_plan, fields, meter, &context)? else {
        return Ok(None);
    };
    let Some(classes) =
        open_segment_classes(reader, fields.event_type, content_scope, meter, &context)?
    else {
        return Ok(None);
    };
    let candidate_fields = ManualCandidateFastFields::open(reader)?;
    Ok(Some(PreparedSegment::new(
        context,
        body_postings,
        fieldnorms,
        filters,
        classes,
        candidate_fields,
    )))
}

fn open_body_postings(
    reader: &SegmentReader,
    field: Field,
    term_infos: &[Option<TermInfo>],
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<Vec<Option<SegmentPostings>>>> {
    let mut inverted = None;
    let mut postings = Vec::with_capacity(term_infos.len());
    for term_info in term_infos {
        let Some(term_info) = term_info else {
            postings.push(None);
            continue;
        };
        // Term metadata was gathered in the corpus-statistics prepass. Charge
        // the posting read before reacquiring the segment's inverted reader.
        if !meter.charge(LexicalWorkCounter::PostingOpens, 1, Some(segment), None) {
            return Ok(None);
        }
        let inverted = match &inverted {
            Some(inverted) => inverted,
            None => inverted.insert(manual_inverted_index(reader, field)?),
        };
        postings.push(Some(manual_read_postings(
            inverted,
            term_info,
            IndexRecordOption::WithFreqs,
        )?));
    }
    Ok(Some(postings))
}

fn open_posting(
    reader: &SegmentReader,
    term: &Term,
    option: IndexRecordOption,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<PostingOpen> {
    // This charge precedes both inverted-reader acquisition and the exact
    // term-info lookup. A zero dictionary budget therefore performs neither.
    if !meter.charge(
        LexicalWorkCounter::DictionaryLookups,
        1,
        Some(segment),
        None,
    ) {
        return Ok(PostingOpen::Exhausted);
    }
    let inverted = manual_inverted_index(reader, term.field())?;
    let Some(term_info) = inverted.get_term_info(term)? else {
        return Ok(PostingOpen::Missing);
    };
    if !meter.charge(LexicalWorkCounter::PostingOpens, 1, Some(segment), None) {
        return Ok(PostingOpen::Exhausted);
    }
    Ok(PostingOpen::Opened {
        postings: Box::new(manual_read_postings(&inverted, &term_info, option)?),
    })
}

fn open_segment_filters(
    reader: &SegmentReader,
    plan: &LexicalFilterAdapter,
    fields: Fields,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<SegmentFilters>> {
    let Some(required) = open_posting_groups(reader, &plan.required, meter, segment)? else {
        return Ok(None);
    };
    let Some(prohibited) = open_posting_groups(reader, &plan.prohibited, meter, segment)? else {
        return Ok(None);
    };
    if required.iter().any(Vec::is_empty) {
        return Ok(Some(SegmentFilters {
            required,
            prohibited,
            workspace: None,
            file: None,
        }));
    }
    let workspace = if let Some(needle) = plan.workspace_substring.as_ref() {
        let workspace_fields = [
            fields.fact_workspace,
            fields.fact_session_cwd,
            fields.fact_tool_workdir,
            fields.fact_project,
        ];
        let Some(bitmap) =
            open_substring_bitmap(reader, &workspace_fields, needle, meter, segment)?
        else {
            return Ok(None);
        };
        Some(bitmap)
    } else {
        None
    };
    let file = if workspace.as_ref().is_some_and(|bitmap| !bitmap.any) {
        None
    } else if let Some(needle) = plan.file_substring.as_ref() {
        let Some(bitmap) =
            open_substring_bitmap(reader, &[fields.fact_file], needle, meter, segment)?
        else {
            return Ok(None);
        };
        Some(bitmap)
    } else {
        None
    };
    Ok(Some(SegmentFilters {
        required,
        prohibited,
        workspace,
        file,
    }))
}

fn open_substring_bitmap(
    reader: &SegmentReader,
    fields: &[Field],
    needle: &AsciiFoldSubstring,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<SegmentBitmap>> {
    let word_count = usize::try_from(reader.max_doc())
        .map_err(|_| IndexError::CountOverflow)?
        .div_ceil(u64::BITS as usize);
    let bitmap_bytes = word_count
        .checked_mul(size_of::<u64>())
        .ok_or(IndexError::CountOverflow)?;
    let bitmap_bytes = u64::try_from(bitmap_bytes).map_err(|_| IndexError::CountOverflow)?;
    if !meter.charge(
        LexicalWorkCounter::SubstringBitmapBytes,
        bitmap_bytes,
        Some(segment),
        None,
    ) {
        return Ok(None);
    }
    let mut bitmap = SegmentBitmap {
        words: vec![0; word_count],
        any: false,
    };

    for field in fields {
        // The step charge precedes both inverted-index acquisition and the
        // first dictionary advance. The final unsuccessful advance is also a
        // real, bounded dictionary operation and is intentionally counted.
        if !meter.charge(
            LexicalWorkCounter::SubstringDictionarySteps,
            1,
            Some(segment),
            None,
        ) {
            return Ok(None);
        }
        let inverted = manual_inverted_index(reader, *field)?;
        let mut terms = inverted.terms().stream()?;
        loop {
            if !terms.advance() {
                break;
            }
            let key = terms.key();
            let key_bytes = u64::try_from(key.len()).map_err(|_| IndexError::CountOverflow)?;
            if !meter.charge(
                LexicalWorkCounter::SubstringDictionaryBytes,
                key_bytes,
                Some(segment),
                None,
            ) {
                return Ok(None);
            }
            std::str::from_utf8(key)
                .map_err(|_| IndexError::InvalidStoredDocumentField("literal_fact"))?;
            if needle.matches(key) {
                if !meter.charge(LexicalWorkCounter::TermExpansions, 1, Some(segment), None)
                    || !meter.charge(LexicalWorkCounter::PostingOpens, 1, Some(segment), None)
                {
                    return Ok(None);
                }
                let mut postings =
                    manual_read_postings(&inverted, terms.value(), IndexRecordOption::Basic)?;
                let mut doc = postings.doc();
                while doc != TERMINATED {
                    if !meter.charge(
                        LexicalWorkCounter::SubstringPostingDocs,
                        1,
                        Some(segment),
                        Some(doc),
                    ) {
                        return Ok(None);
                    }
                    bitmap.insert(doc)?;
                    doc = postings.advance();
                }
            }
            if !meter.charge(
                LexicalWorkCounter::SubstringDictionarySteps,
                1,
                Some(segment),
                None,
            ) {
                return Ok(None);
            }
        }
    }
    Ok(Some(bitmap))
}

fn open_posting_groups(
    reader: &SegmentReader,
    groups: &[CanonicalAnyOfTerms],
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<Vec<Vec<SegmentPostings>>>> {
    let mut opened_groups = Vec::with_capacity(groups.len());
    for group in groups {
        let mut postings = Vec::with_capacity(group.terms.len());
        for term in &group.terms {
            match open_posting(reader, term, IndexRecordOption::Basic, meter, segment)? {
                PostingOpen::Opened {
                    postings: opened, ..
                } => postings.push(*opened),
                PostingOpen::Missing => {}
                PostingOpen::Exhausted => return Ok(None),
            }
        }
        opened_groups.push(postings);
    }
    Ok(Some(opened_groups))
}

fn open_segment_classes(
    reader: &SegmentReader,
    event_type_field: Field,
    scope: SearchContentScope,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Result<Option<SegmentClasses>> {
    if matches!(
        scope,
        SearchContentScope::Calls | SearchContentScope::Outputs
    ) {
        return Ok(Some(SegmentClasses::Unweighted));
    }
    let open = |value: &str, meter: &mut LexicalWorkMeter| {
        open_posting(
            reader,
            &Term::from_field_text(event_type_field, value),
            IndexRecordOption::Basic,
            meter,
            segment,
        )
    };
    let message = match open("message", meter)? {
        PostingOpen::Opened { postings, .. } => Some(*postings),
        PostingOpen::Missing => None,
        PostingOpen::Exhausted => return Ok(None),
    };
    let summary = match open("summary", meter)? {
        PostingOpen::Opened { postings, .. } => Some(*postings),
        PostingOpen::Missing => None,
        PostingOpen::Exhausted => return Ok(None),
    };
    let mut outputs = Vec::new();
    if scope == SearchContentScope::All {
        for output_type in OUTPUT_EVENT_TYPES {
            match open(output_type, meter)? {
                PostingOpen::Opened { postings, .. } => outputs.push(*postings),
                PostingOpen::Missing => {}
                PostingOpen::Exhausted => return Ok(None),
            }
        }
    }
    Ok(Some(SegmentClasses::Weighted(Box::new(
        WeightedSegmentClasses {
            message,
            summary,
            outputs,
            scope,
        },
    ))))
}

fn posting_option_contains(
    postings: &mut Option<SegmentPostings>,
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Option<bool> {
    let Some(postings) = postings else {
        return Some(false);
    };
    posting_contains(postings, doc, meter, segment)
}

fn posting_group_contains(
    postings: &mut [SegmentPostings],
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Option<bool> {
    for postings in postings {
        if posting_contains(postings, doc, meter, segment)? {
            return Some(true);
        }
    }
    Some(false)
}

fn posting_contains(
    postings: &mut SegmentPostings,
    doc: DocId,
    meter: &mut LexicalWorkMeter,
    segment: &LexicalSegmentContext,
) -> Option<bool> {
    if !meter.charge(
        LexicalWorkCounter::FilterProbes,
        1,
        Some(segment),
        Some(doc),
    ) {
        return None;
    }
    let posting_doc = postings.doc();
    if posting_doc == doc {
        return Some(true);
    }
    if posting_doc < doc {
        if !meter.charge(LexicalWorkCounter::FilterSeeks, 1, Some(segment), Some(doc)) {
            return None;
        }
        return Some(postings.seek(doc) == doc);
    }
    Some(false)
}

fn next_body_doc(postings: &[Option<SegmentPostings>]) -> Option<DocId> {
    postings
        .iter()
        .filter_map(|postings| postings.as_ref().map(|postings| postings.doc()))
        .filter(|doc| *doc != TERMINATED)
        .min()
}

fn validate_materialized_event(
    order: ctx_history_index_format::EventRangeOrderKey,
    event: &EventRecord,
    fast_event_id: Uuid,
) -> Result<()> {
    if event.event_id.as_uuid() != fast_event_id
        || event.event_id.digest() != order.event_identity_digest()
        || event.event_sequence != order.event_sequence()
        || event.occurred_at_unix_ms != order.occurred_at_unix_ms()
    {
        return Err(IndexError::InvalidStoredDocumentField(
            EVENT_RANGE_ORDER_FAST_FIELD,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateExamination {
    Rejected,
    Accepted,
    Exhausted,
}

fn empty_lexical_batch(candidate_set_exhaustive: bool) -> LexicalSearchBatch {
    LexicalSearchBatch {
        candidates: Vec::new(),
        complete: true,
        candidate_set_exhaustive,
        exhaustion: None,
        counters: LexicalWorkCounters::default(),
    }
}

fn finish_lexical_batch(
    candidates: Vec<LexicalSearchCandidate>,
    meter: LexicalWorkMeter,
    retained_truncated: bool,
) -> LexicalSearchBatch {
    let (counters, exhaustion) = meter.into_parts();
    let complete = exhaustion.is_none();
    LexicalSearchBatch {
        candidates,
        complete,
        candidate_set_exhaustive: complete && !retained_truncated,
        exhaustion,
        counters,
    }
}

fn complete_compatibility_candidates(
    batch: LexicalSearchBatch,
) -> LexicalSearchResult<Vec<EventSearchCandidate>> {
    let LexicalSearchBatch {
        candidates,
        complete,
        exhaustion,
        ..
    } = batch;
    if !complete {
        return Err(LexicalSearchError::WorkExhausted(exhaustion.expect(
            "an incomplete lexical batch always has an exhaustion reason",
        )));
    }
    Ok(candidates.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_bm25_helper_uses_only_explicit_pinned_statistics() {
        reset_lexical_query_work();

        let doc_frequency = 2_u64;
        let total_num_docs = 10_u64;
        let average_fieldnorm = 5.0_f32;
        let fieldnorm_id = FieldNormReader::fieldnorm_to_id(5);
        let term_frequency = 3_u32;
        let score = explicit_bm25_weight(doc_frequency, total_num_docs, average_fieldnorm)
            .score(fieldnorm_id, term_frequency);

        // Pin Tantivy BM25's k1=1.2, b=0.75 formula independently of the
        // helper result. This is formula use, not dynamic query execution.
        let idf = (1.0_f32
            + ((total_num_docs - doc_frequency) as f32 + 0.5) / (doc_frequency as f32 + 0.5))
            .ln();
        let norm = 1.2_f32
            * (1.0 - 0.75
                + 0.75 * FieldNormReader::id_to_fieldnorm(fieldnorm_id) as f32 / average_fieldnorm);
        let expected = idf * (1.0 + 1.2) * term_frequency as f32 / (term_frequency as f32 + norm);
        assert!((score - expected).abs() < f32::EPSILON * 8.0);
        assert_eq!(lexical_query_constructions(), 0);
        assert_eq!(lexical_query_executions(), 0);
    }
}
