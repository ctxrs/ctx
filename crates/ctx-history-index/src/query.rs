mod contract;
mod execution;
mod filtering;
mod records;
mod verification;

pub use contract::*;
use filtering::*;
pub(crate) use records::stored_event_record;
use records::{
    stored_core_event_record, stored_core_event_record_with_size, EventAddressCandidate,
    SessionEventAddressCandidate,
};

pub(super) use verification::{stored_verification_record, validate_verification_projection};

#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
};

use ctx_history_core::{
    CoreRecord, SourceKey, StableEntityId, StableEntityKind, TypedKey, MAX_CORE_CONTENT_BYTES,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::{
    collector::{Collector, Count, DocSetCollector, SegmentCollector, TopDocs},
    query::{
        AllQuery, BooleanQuery, ConstScoreQuery, EmptyQuery, Occur, Query, RangeQuery, RegexQuery,
        TermQuery, TermSetQuery,
    },
    schema::{IndexRecordOption, Value as TantivyValue},
    termdict::TermMerger,
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
    core_content_bytes, SessionEventOrderKey, SourceEventOrderKey, StoredQueryMetadata,
    MAX_QUERY_METADATA_BYTES, QUERY_METADATA_CHUNK_BYTES, QUERY_METADATA_CHUNK_DIGEST_BYTES,
    QUERY_METADATA_CHUNK_HEADER_BYTES, QUERY_METADATA_CHUNK_MAGIC,
    QUERY_METADATA_CHUNK_PAYLOAD_BYTES, QUERY_METADATA_DIGEST_DOMAIN,
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
const QUERY_METADATA_FIELD: &str = "query_metadata";
const CORE_CONTENT_BYTES_FIELD: &str = "core_content_bytes";
const SOURCE_EVENT_ORDER_FIELD: &str = "source_event_order";
const SESSION_EVENT_ORDER_FIELD: &str = "session_event_order";

#[cfg(test)]
thread_local! {
    static STORED_EVENT_RECORD_MATERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static STORED_CORE_EVENT_RECORD_MATERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static SOURCE_EVENT_ORDER_TERM_VISITS: Cell<usize> = const { Cell::new(0) };
    static SESSION_EVENT_ORDER_TERM_VISITS: Cell<usize> = const { Cell::new(0) };
    static SESSION_EVENT_ORDER_VISITED_SEQUENCES: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    static QUERY_METADATA_CHUNK_READS: Cell<usize> = const { Cell::new(0) };
    static QUERY_METADATA_EXACT_ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
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
pub(crate) fn reset_query_metadata_decode_work() {
    QUERY_METADATA_CHUNK_READS.set(0);
    QUERY_METADATA_EXACT_ALLOCATED_BYTES.set(0);
}

#[cfg(test)]
pub(crate) fn query_metadata_chunk_reads() -> usize {
    QUERY_METADATA_CHUNK_READS.get()
}

#[cfg(test)]
pub(crate) fn query_metadata_exact_allocated_bytes() -> usize {
    QUERY_METADATA_EXACT_ALLOCATED_BYTES.get()
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
