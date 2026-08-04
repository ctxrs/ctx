mod contract;
mod event_range;
mod execution;
mod filtering;
mod records;
mod verification;

pub use contract::*;
pub use event_range::*;
use filtering::*;
pub(crate) use records::stored_event_record;
use records::{
    core_event_fast_preflight, stored_core_event_record, stored_core_event_record_with_size,
    stored_core_event_record_with_source_json, unique_required_bytes, EventAddressCandidate,
    SessionEventAddressCandidate,
};

pub(crate) use verification::{
    stored_verification_identities, stored_verification_record, validate_verification_projection,
    CompactIdentity, IdentityFieldRole, VerificationRecord,
};

#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashSet},
    ops::Bound,
};

use ctx_history_core::{
    CoreRecord, SourceKey, StableEntityId, StableEntityKind, TypedKey, MAX_CORE_CONTENT_BYTES,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
use serde::{Deserialize, Serialize};
use tantivy::{
    collector::{Collector, Count, DocSetCollector, SegmentCollector, TopDocs},
    postings::SegmentPostings,
    query::{
        AllQuery, BooleanQuery, ConstScoreQuery, EmptyQuery, EnableScoring, Explanation, Occur,
        Query, RangeQuery, RegexQuery, Scorer, TermQuery, TermSetQuery, Weight,
    },
    schema::{Field, IndexRecordOption, Value as TantivyValue},
    termdict::{TermMerger, TermStreamer},
    tokenizer::TokenStream,
    DocAddress, DocId, DocSet, InvertedIndexReader, Score, SegmentOrdinal, SegmentReader,
    TantivyDocument, Term, TERMINATED,
};
use uuid::Uuid;

use super::{
    fields_from_schema, hex, source_token, Fields, IndexError, Result, VerifiedIndex,
    MAX_DOCUMENT_METADATA_BYTES,
};
use crate::index_document::{
    core_content_bytes, SemanticEventOrderKey, SessionEventOrderKey, SourceEventOrderKey,
};

const ID_PREFIX_MATCH_LIMIT: usize = 2;
use crate::analyzer::BODY_ANALYZER;
const EVENT_ID_HIGH_FIELD: &str = "event_id_high";
const EVENT_ID_LOW_FIELD: &str = "event_id_low";
const SESSION_ID_HIGH_FIELD: &str = "session_id_high";
const SESSION_ID_LOW_FIELD: &str = "session_id_low";
const EVENT_SEQUENCE_FIELD: &str = "event_sequence";
const OCCURRED_AT_UNIX_MS_FIELD: &str = "occurred_at_unix_ms";
const EVENT_IDENTITY_DIGEST_FIELD: &str = "event_identity_digest";
const SOURCE_KEY_FIELD: &str = "source_key";
const CORE_CONTENT_BYTES_FIELD: &str = "core_content_bytes";
const CORE_RECORD_ENCODED_BYTES_FIELD: &str = "core_record_encoded_bytes";
const SOURCE_EVENT_ORDER_FIELD: &str = "source_event_order";
const SESSION_EVENT_ORDER_FIELD: &str = "session_event_order";
const SEMANTIC_EVENT_ORDER_FIELD: &str = "semantic_event_order";

pub(crate) struct SemanticEligibilityPostings {
    total: u64,
    segments: Vec<Vec<bool>>,
}

impl SemanticEligibilityPostings {
    fn includes(&self, address: DocAddress) -> Result<bool> {
        self.segments
            .get(address.segment_ord as usize)
            .and_then(|segment| segment.get(address.doc_id as usize))
            .copied()
            .ok_or(IndexError::InvalidStoredDocumentField(
                SEMANTIC_EVENT_ORDER_FIELD,
            ))
    }
}

#[cfg(test)]
thread_local! {
    static STORED_EVENT_RECORD_MATERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static STORED_CORE_EVENT_RECORD_MATERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static CORE_RECORD_DECODES: Cell<usize> = const { Cell::new(0) };
    static CORE_EVENT_ID_SELECTION_QUERIES: Cell<usize> = const { Cell::new(0) };
    static SOURCE_EVENT_ORDER_TERM_VISITS: Cell<usize> = const { Cell::new(0) };
    static SESSION_EVENT_ORDER_TERM_VISITS: Cell<usize> = const { Cell::new(0) };
    static SEMANTIC_EVENT_ORDER_TERM_VISITS: Cell<usize> = const { Cell::new(0) };
    static EVENT_RANGE_ORDER_TERM_VISITS: Cell<usize> = const { Cell::new(0) };
    static EVENT_RANGE_CURSOR_RECORD_RESERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static SESSION_EVENT_ORDER_VISITED_SEQUENCES: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    static LEXICAL_QUERY_CONSTRUCTIONS: Cell<usize> = const { Cell::new(0) };
    static LEXICAL_QUERY_EXECUTIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_stored_event_record_materializations() {
    STORED_EVENT_RECORD_MATERIALIZATIONS.set(0);
}

#[cfg(test)]
pub(crate) fn stored_event_record_materializations() -> usize {
    STORED_EVENT_RECORD_MATERIALIZATIONS.get()
}

#[cfg(test)]
pub(crate) fn reset_stored_core_event_record_materializations() {
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.set(0);
}

#[cfg(test)]
pub(crate) fn stored_core_event_record_materializations() -> usize {
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.get()
}

#[cfg(test)]
pub(crate) fn reset_core_record_decodes() {
    CORE_RECORD_DECODES.set(0);
}

#[cfg(test)]
pub(crate) fn core_record_decodes() -> usize {
    CORE_RECORD_DECODES.get()
}

#[cfg(test)]
pub(crate) fn reset_core_event_id_selection_queries() {
    CORE_EVENT_ID_SELECTION_QUERIES.set(0);
}

#[cfg(test)]
pub(crate) fn core_event_id_selection_queries() -> usize {
    CORE_EVENT_ID_SELECTION_QUERIES.get()
}

#[cfg(test)]
pub(crate) fn reset_source_event_order_term_visits() {
    SOURCE_EVENT_ORDER_TERM_VISITS.set(0);
}

#[cfg(test)]
pub(crate) fn source_event_order_term_visits() -> usize {
    SOURCE_EVENT_ORDER_TERM_VISITS.get()
}

#[cfg(test)]
pub(crate) fn reset_session_event_order_term_visits() {
    SESSION_EVENT_ORDER_TERM_VISITS.set(0);
    SESSION_EVENT_ORDER_VISITED_SEQUENCES.with(|sequences| sequences.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn session_event_order_term_visits() -> usize {
    SESSION_EVENT_ORDER_TERM_VISITS.get()
}

#[cfg(test)]
pub(crate) fn session_event_order_visited_sequences() -> Vec<u64> {
    SESSION_EVENT_ORDER_VISITED_SEQUENCES.with(|sequences| sequences.borrow().clone())
}

#[cfg(test)]
pub(crate) fn reset_semantic_event_order_term_visits() {
    SEMANTIC_EVENT_ORDER_TERM_VISITS.set(0);
}

#[cfg(test)]
pub(crate) fn semantic_event_order_term_visits() -> usize {
    SEMANTIC_EVENT_ORDER_TERM_VISITS.get()
}

#[cfg(test)]
pub(crate) fn reset_event_range_order_term_visits() {
    EVENT_RANGE_ORDER_TERM_VISITS.set(0);
}

#[cfg(test)]
pub(crate) fn event_range_order_term_visits() -> usize {
    EVENT_RANGE_ORDER_TERM_VISITS.get()
}

#[cfg(test)]
pub(crate) fn reset_event_range_cursor_record_reserializations() {
    EVENT_RANGE_CURSOR_RECORD_RESERIALIZATIONS.set(0);
}

#[cfg(test)]
pub(crate) fn event_range_cursor_record_reserializations() -> usize {
    EVENT_RANGE_CURSOR_RECORD_RESERIALIZATIONS.get()
}

#[cfg(test)]
pub(crate) fn reset_lexical_query_work() {
    LEXICAL_QUERY_CONSTRUCTIONS.set(0);
    LEXICAL_QUERY_EXECUTIONS.set(0);
}

#[cfg(test)]
pub(crate) fn lexical_query_constructions() -> usize {
    LEXICAL_QUERY_CONSTRUCTIONS.get()
}

#[cfg(test)]
pub(crate) fn lexical_query_executions() -> usize {
    LEXICAL_QUERY_EXECUTIONS.get()
}

#[cfg(test)]
fn record_lexical_query_construction() {
    LEXICAL_QUERY_CONSTRUCTIONS.set(LEXICAL_QUERY_CONSTRUCTIONS.get().saturating_add(1));
}

#[cfg(test)]
fn record_lexical_query_execution() {
    LEXICAL_QUERY_EXECUTIONS.set(LEXICAL_QUERY_EXECUTIONS.get().saturating_add(1));
}
