mod contract;
mod event_range;
mod execution;
mod filtering;
mod reader;
mod records;

pub use contract::*;
pub use ctx_history_index_format::{IndexError, Result};
pub use event_range::*;
use filtering::*;
pub use reader::VerifiedIndex;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use reader::{
    reset_verified_index_publication_construction_count, reset_verified_index_reopen_count,
    verified_index_publication_construction_count, verified_index_reopen_count,
};
pub(crate) use records::stored_event_record;
use records::{
    core_event_fast_preflight, stored_core_event_record, stored_core_event_record_with_size,
    stored_core_event_record_with_source_json, EventAddressCandidate, SessionEventAddressCandidate,
};

#[cfg(any(test, feature = "test-support"))]
use std::cell::{Cell, RefCell};
use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashSet},
    ops::Bound,
};

use ctx_history_core::{
    AgentScope as CoreAgentScope, CoreRecord, LiteralFactKind, ProviderNativeCopyProof,
    ProviderNativeEventCopy, ProviderNativeSessionRelationship, SourceKey, StableEntityId,
    StableEntityKind, TypedKey, MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES,
};
use serde::{Deserialize, Serialize};
use tantivy::{
    collector::{Collector, Count, DocSetCollector, SegmentCollector, TopDocs},
    postings::SegmentPostings,
    query::{
        AllQuery, BooleanQuery, ConstScoreQuery, EmptyQuery, Occur, Query, RangeQuery, RegexQuery,
        TermQuery, TermSetQuery,
    },
    schema::{Field, IndexRecordOption},
    termdict::{TermMerger, TermStreamer},
    tokenizer::TokenStream,
    DocAddress, DocId, DocSet, InvertedIndexReader, Score, SegmentOrdinal, SegmentReader,
    TantivyDocument, Term, TERMINATED,
};
use uuid::Uuid;

use ctx_history_index_format::{
    core_content_bytes, CompactIdentity, SemanticEventOrderKey, SessionEventOrderKey,
    SourceEventOrderKey,
};
use ctx_history_index_format::{
    fields_from_schema, hex, source_token, Fields, MAX_DOCUMENT_METADATA_BYTES,
};

const ID_PREFIX_MATCH_LIMIT: usize = 2;
const EVENT_ID_HIGH_FIELD: &str = "event_id_high";
const EVENT_ID_LOW_FIELD: &str = "event_id_low";
const SESSION_ID_HIGH_FIELD: &str = "session_id_high";
const SESSION_ID_LOW_FIELD: &str = "session_id_low";
const EVENT_SEQUENCE_FIELD: &str = "event_sequence";
const OCCURRED_AT_UNIX_MS_FIELD: &str = "occurred_at_unix_ms";
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

#[cfg(any(test, feature = "test-support"))]
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
    static LEXICAL_CANDIDATE_MATERIALIZATION_FAILURE_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    static SESSION_GROUPING_AUTHORITY_QUERIES: Cell<usize> = const { Cell::new(0) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_session_grouping_authority_queries() {
    SESSION_GROUPING_AUTHORITY_QUERIES.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn session_grouping_authority_queries() -> usize {
    SESSION_GROUPING_AUTHORITY_QUERIES.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_stored_event_record_materializations() {
    STORED_EVENT_RECORD_MATERIALIZATIONS.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn stored_event_record_materializations() -> usize {
    STORED_EVENT_RECORD_MATERIALIZATIONS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_stored_core_event_record_materializations() {
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn stored_core_event_record_materializations() -> usize {
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_core_record_decodes() {
    CORE_RECORD_DECODES.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn core_record_decodes() -> usize {
    CORE_RECORD_DECODES.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_core_event_id_selection_queries() {
    CORE_EVENT_ID_SELECTION_QUERIES.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn core_event_id_selection_queries() -> usize {
    CORE_EVENT_ID_SELECTION_QUERIES.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_source_event_order_term_visits() {
    SOURCE_EVENT_ORDER_TERM_VISITS.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn source_event_order_term_visits() -> usize {
    SOURCE_EVENT_ORDER_TERM_VISITS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_session_event_order_term_visits() {
    SESSION_EVENT_ORDER_TERM_VISITS.set(0);
    SESSION_EVENT_ORDER_VISITED_SEQUENCES.with(|sequences| sequences.borrow_mut().clear());
}

#[cfg(any(test, feature = "test-support"))]
pub fn session_event_order_term_visits() -> usize {
    SESSION_EVENT_ORDER_TERM_VISITS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn session_event_order_visited_sequences() -> Vec<u64> {
    SESSION_EVENT_ORDER_VISITED_SEQUENCES.with(|sequences| sequences.borrow().clone())
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_semantic_event_order_term_visits() {
    SEMANTIC_EVENT_ORDER_TERM_VISITS.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn semantic_event_order_term_visits() -> usize {
    SEMANTIC_EVENT_ORDER_TERM_VISITS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_event_range_order_term_visits() {
    EVENT_RANGE_ORDER_TERM_VISITS.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn event_range_order_term_visits() -> usize {
    EVENT_RANGE_ORDER_TERM_VISITS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_event_range_cursor_record_reserializations() {
    EVENT_RANGE_CURSOR_RECORD_RESERIALIZATIONS.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn event_range_cursor_record_reserializations() -> usize {
    EVENT_RANGE_CURSOR_RECORD_RESERIALIZATIONS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_lexical_query_work() {
    LEXICAL_QUERY_CONSTRUCTIONS.set(0);
    LEXICAL_QUERY_EXECUTIONS.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn lexical_query_constructions() -> usize {
    LEXICAL_QUERY_CONSTRUCTIONS.get()
}

#[cfg(any(test, feature = "test-support"))]
pub fn lexical_query_executions() -> usize {
    LEXICAL_QUERY_EXECUTIONS.get()
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn fail_lexical_candidate_materialization_after(records: usize) {
    LEXICAL_CANDIDATE_MATERIALIZATION_FAILURE_AFTER.set(Some(records));
}

#[cfg(any(test, feature = "test-support"))]
struct LexicalCandidateMaterializationFailureReset;

#[cfg(any(test, feature = "test-support"))]
impl Drop for LexicalCandidateMaterializationFailureReset {
    fn drop(&mut self) {
        LEXICAL_CANDIDATE_MATERIALIZATION_FAILURE_AFTER.set(None);
    }
}

#[cfg(any(test, feature = "test-support"))]
fn lexical_candidate_materialization_failure_reset() -> LexicalCandidateMaterializationFailureReset
{
    LexicalCandidateMaterializationFailureReset
}

#[cfg(any(test, feature = "test-support"))]
fn lexical_candidate_materialization_should_fail() -> bool {
    LEXICAL_CANDIDATE_MATERIALIZATION_FAILURE_AFTER.with(|remaining| match remaining.get() {
        Some(0) => {
            remaining.set(None);
            true
        }
        Some(count) => {
            remaining.set(Some(count - 1));
            false
        }
        None => false,
    })
}

#[cfg(any(test, feature = "test-support"))]
fn record_lexical_query_execution() {
    LEXICAL_QUERY_EXECUTIONS.set(LEXICAL_QUERY_EXECUTIONS.get().saturating_add(1));
}
