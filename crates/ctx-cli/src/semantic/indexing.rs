use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use anyhow::Result;
use ctx_history_store::{EventEmbeddingDocument, Store};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    model_contract::{SEMANTIC_PASSAGE_PREFIX, SEMANTIC_QUERY_PREFIX},
    model_runtime::SharedSemanticRuntime,
    reports::SemanticRetrievalDiagnostics,
    runtime_limits::{
        SEMANTIC_CHUNK_OVERLAP_CHARS, SEMANTIC_CHUNK_TARGET_CHARS,
        SEMANTIC_DEADLINE_CHUNKS_PER_SECOND, SEMANTIC_DEADLINE_MIN_CHUNK_BATCH,
        SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT, SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS,
        SEMANTIC_SOURCE_MAX_CHARS,
    },
    vector_store::{
        SemanticChunkDocument, SemanticHitSearch, SemanticIndexOutcome, SemanticVectorHit,
        SemanticVectorStore,
    },
};

#[allow(clippy::too_many_arguments)]
pub(super) fn backfill_semantic_embeddings(
    store: &Store,
    vector_store: &mut SemanticVectorStore,
    runtime: &SharedSemanticRuntime,
    model_init_ms: &mut Option<u64>,
    cache_dir: &Path,
    max_to_index: usize,
    continue_past_indexed_pages: bool,
    deadline: Option<Instant>,
) -> Result<usize> {
    let mut existing_hashes = HashMap::new();
    let mut before = None;
    let mut indexed = 0_usize;
    let mut recent_probe_done = false;

    let dirty_ids =
        vector_store.queued_dirty_event_ids(max_to_index.min(SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT))?;
    if !dirty_ids.is_empty() && indexed < max_to_index {
        let docs = store.event_embedding_documents_by_ids(&dirty_ids)?;
        extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
        let found_event_ids = docs.iter().map(|doc| doc.event_id).collect::<HashSet<_>>();
        let mut consumed_event_ids = dirty_ids
            .iter()
            .filter(|event_id| !found_event_ids.contains(event_id))
            .copied()
            .collect::<Vec<_>>();
        let outcome = index_semantic_documents(
            vector_store,
            runtime,
            model_init_ms,
            cache_dir,
            &mut existing_hashes,
            docs,
            max_to_index.saturating_sub(indexed),
            deadline,
        )?;
        indexed = indexed.saturating_add(outcome.indexed_chunks);
        consumed_event_ids.extend(outcome.consumed_event_ids);
        if !consumed_event_ids.is_empty() {
            vector_store.dequeue_dirty_events(&consumed_event_ids)?;
        }
        if !continue_past_indexed_pages {
            return Ok(indexed);
        }
    }

    while indexed < max_to_index {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        let embedder_batch_size = runtime.loaded_batch_size()?;
        if embedder_batch_size
            .is_some_and(|batch_size| max_to_index.saturating_sub(indexed) < batch_size)
        {
            break;
        }
        let docs = store.recent_event_embedding_documents(before, 512)?;
        if docs.is_empty() {
            if continue_past_indexed_pages {
                vector_store.set_backfill_cursor(None)?;
            }
            break;
        }
        let page_cursors = docs
            .iter()
            .map(|doc| (doc.event_id, (doc.anchor_occurred_at_ms, doc.seq)))
            .collect::<Vec<_>>();
        extend_existing_hashes_for_docs(vector_store, &mut existing_hashes, &docs)?;
        let outcome = index_semantic_documents(
            vector_store,
            runtime,
            model_init_ms,
            cache_dir,
            &mut existing_hashes,
            docs,
            max_to_index.saturating_sub(indexed),
            deadline,
        )?;
        let added = outcome.indexed_chunks;
        indexed = indexed.saturating_add(added);
        let consumed_cursor =
            semantic_consumed_page_anchor_cursor(&page_cursors, &outcome.consumed_event_ids);
        if !continue_past_indexed_pages {
            break;
        }
        // Store pages are selected by user-anchor time and then reordered by
        // turn activity, so an activity-order prefix is not a safe pagination
        // prefix. Keep the durable frontier unchanged until every anchor in
        // this page has either been embedded or proven unchanged.
        if consumed_cursor.is_none() {
            break;
        }
        if !recent_probe_done {
            recent_probe_done = true;
            let stored_cursor = vector_store.backfill_cursor()?;
            before = stored_cursor.or(consumed_cursor);
            if stored_cursor.is_none() {
                vector_store.set_backfill_cursor(before)?;
            }
        } else {
            before = consumed_cursor;
            vector_store.set_backfill_cursor(before)?;
        }
    }
    Ok(indexed)
}

pub(super) fn semantic_consumed_page_anchor_cursor(
    page_cursors: &[(Uuid, (i64, u64))],
    consumed_event_ids: &[Uuid],
) -> Option<(i64, u64)> {
    let consumed = consumed_event_ids.iter().copied().collect::<HashSet<_>>();
    if page_cursors
        .iter()
        .any(|(event_id, _)| !consumed.contains(event_id))
    {
        return None;
    }
    page_cursors.iter().map(|(_, cursor)| *cursor).min()
}

pub(super) fn extend_existing_hashes_for_docs(
    vector_store: &SemanticVectorStore,
    existing_hashes: &mut HashMap<Uuid, String>,
    docs: &[EventEmbeddingDocument],
) -> Result<()> {
    let event_ids = docs
        .iter()
        .map(|doc| doc.event_id)
        .filter(|event_id| !existing_hashes.contains_key(event_id))
        .collect::<Vec<_>>();
    if event_ids.is_empty() {
        return Ok(());
    }
    existing_hashes.extend(vector_store.existing_hashes_for_event_ids(&event_ids)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn index_semantic_documents(
    vector_store: &mut SemanticVectorStore,
    runtime: &SharedSemanticRuntime,
    model_init_ms: &mut Option<u64>,
    cache_dir: &Path,
    existing_hashes: &mut HashMap<Uuid, String>,
    docs: Vec<EventEmbeddingDocument>,
    limit: usize,
    deadline: Option<Instant>,
) -> Result<SemanticIndexOutcome> {
    let limit = semantic_deadline_chunk_limit(limit, deadline);
    if limit == 0 {
        return Ok(SemanticIndexOutcome::default());
    }
    let mut pending = Vec::<SemanticChunkDocument>::new();
    let mut unchanged_event_ids = Vec::new();
    let mut considered_event_ids = Vec::new();
    for doc in docs {
        let source_text = semantic_source_text(&doc.text);
        let text_hash = semantic_document_hash(&doc, &source_text);
        if existing_hashes
            .get(&doc.event_id)
            .is_some_and(|existing| existing == &text_hash)
        {
            unchanged_event_ids.push(doc.event_id);
            considered_event_ids.push(doc.event_id);
            continue;
        }
        let chunks = semantic_chunks_for_document(&doc, &source_text, &text_hash);
        if chunks.len() > limit && pending.is_empty() {
            break;
        }
        if pending.len().saturating_add(chunks.len()) > limit && !pending.is_empty() {
            break;
        }
        considered_event_ids.push(doc.event_id);
        pending.extend(chunks);
        if pending.len() >= limit {
            break;
        }
    }
    if pending.is_empty() {
        return Ok(SemanticIndexOutcome {
            indexed_chunks: 0,
            consumed_event_ids: considered_event_ids,
        });
    }
    let texts = pending
        .iter()
        .map(|doc| doc.text.clone())
        .collect::<Vec<_>>();
    if !runtime.is_loaded() {
        if !semantic_deadline_has_model_init_budget(deadline) {
            return Ok(SemanticIndexOutcome {
                indexed_chunks: 0,
                consumed_event_ids: semantic_contiguous_consumed_event_ids(
                    &considered_event_ids,
                    &unchanged_event_ids,
                ),
            });
        }
        if let Some(elapsed_ms) = runtime.ensure_loaded_from_cache(cache_dir)? {
            *model_init_ms = Some(elapsed_ms);
        }
    }
    let batch_size = runtime.active_batch_size()?;
    let mut embeddings = Vec::with_capacity(texts.len());
    for batch in texts.chunks(batch_size) {
        if semantic_deadline_reached(deadline) {
            break;
        }
        let (batch_embeddings, _) = runtime.embed_documents(cache_dir, batch.to_vec(), deadline)?;
        embeddings.extend(batch_embeddings);
    }
    let complete_prefix = semantic_complete_embedding_prefix(&pending, embeddings.len());
    pending.truncate(complete_prefix);
    embeddings.truncate(complete_prefix);
    let mut completed_event_ids = pending.iter().map(|doc| doc.event_id).collect::<Vec<_>>();
    completed_event_ids.dedup();
    let items = pending
        .into_iter()
        .zip(embeddings)
        .map(|(doc, embedding)| {
            existing_hashes.insert(doc.event_id, doc.source_text_hash.clone());
            (doc, embedding)
        })
        .collect::<Vec<_>>();
    vector_store.upsert_chunk_embeddings(&items)?;
    unchanged_event_ids.extend(completed_event_ids);
    let consumed_event_ids =
        semantic_contiguous_consumed_event_ids(&considered_event_ids, &unchanged_event_ids);
    Ok(SemanticIndexOutcome {
        indexed_chunks: items.len(),
        consumed_event_ids,
    })
}

pub(super) fn semantic_contiguous_consumed_event_ids(
    considered_event_ids: &[Uuid],
    completed_event_ids: &[Uuid],
) -> Vec<Uuid> {
    let completed = completed_event_ids.iter().copied().collect::<HashSet<_>>();
    considered_event_ids
        .iter()
        .copied()
        .take_while(|event_id| completed.contains(event_id))
        .collect()
}

pub(super) fn semantic_deadline_reached(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

pub(super) fn semantic_complete_embedding_prefix(
    pending: &[SemanticChunkDocument],
    embedded_len: usize,
) -> usize {
    let embedded_len = embedded_len.min(pending.len());
    if embedded_len == 0 || embedded_len == pending.len() {
        return embedded_len;
    }
    let last_embedded_event = pending[embedded_len - 1].event_id;
    if pending[embedded_len].event_id != last_embedded_event {
        return embedded_len;
    }
    pending[..embedded_len]
        .iter()
        .rposition(|doc| doc.event_id != last_embedded_event)
        .map(|index| index + 1)
        .unwrap_or(0)
}

pub(super) fn semantic_deadline_chunk_limit(limit: usize, deadline: Option<Instant>) -> usize {
    let Some(deadline) = deadline else {
        return limit;
    };
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return 0;
    };
    let seconds = remaining.as_secs() as usize;
    if seconds == 0 {
        return 0;
    }
    let deadline_limit = seconds
        .saturating_mul(SEMANTIC_DEADLINE_CHUNKS_PER_SECOND)
        .max(SEMANTIC_DEADLINE_MIN_CHUNK_BATCH);
    limit.min(deadline_limit)
}

pub(super) fn semantic_deadline_has_model_init_budget(deadline: Option<Instant>) -> bool {
    let Some(deadline) = deadline else {
        return true;
    };
    deadline
        .checked_duration_since(Instant::now())
        .is_some_and(|remaining| {
            remaining >= StdDuration::from_secs(SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS)
        })
}

pub(super) fn semantic_source_text(text: &str) -> String {
    text.chars().take(SEMANTIC_SOURCE_MAX_CHARS).collect()
}

pub(super) fn semantic_chunks_for_document(
    doc: &EventEmbeddingDocument,
    source_text: &str,
    source_text_hash: &str,
) -> Vec<SemanticChunkDocument> {
    let chunks = semantic_text_chunks(source_text);
    chunks
        .into_iter()
        .enumerate()
        .map(
            |(chunk_index, (start_char, end_char, text))| SemanticChunkDocument {
                event_id: doc.event_id,
                seq: doc.seq,
                chunk_index,
                source_text_hash: source_text_hash.to_owned(),
                text: semantic_embedded_chunk_text(doc, &text),
                start_char,
                end_char,
            },
        )
        .collect()
}

pub(super) fn semantic_document_hash(doc: &EventEmbeddingDocument, source_text: &str) -> String {
    semantic_text_hash(&semantic_embedded_document_text(doc, source_text))
}

pub(super) fn semantic_embedded_document_text(doc: &EventEmbeddingDocument, body: &str) -> String {
    semantic_embedded_chunk_text(doc, body)
}

pub(super) fn semantic_embedded_chunk_text(doc: &EventEmbeddingDocument, body: &str) -> String {
    let header = semantic_document_header(doc);
    let text = if header.is_empty() {
        body.to_owned()
    } else {
        format!("{header}\n\n{body}")
    };
    semantic_e5_passage_text(&text)
}

pub(super) fn semantic_e5_prefixed_text(prefix: &str, text: &str) -> String {
    let text = text.trim_start();
    if text.starts_with(prefix) {
        text.to_owned()
    } else {
        format!("{prefix}{text}")
    }
}

pub(super) fn semantic_e5_passage_text(text: &str) -> String {
    semantic_e5_prefixed_text(SEMANTIC_PASSAGE_PREFIX, text)
}

pub(super) fn semantic_e5_query_text_value(text: &str) -> String {
    semantic_e5_prefixed_text(SEMANTIC_QUERY_PREFIX, text)
}

pub(super) fn semantic_document_header(doc: &EventEmbeddingDocument) -> String {
    let mut lines = vec![
        "semantic_document: v2".to_owned(),
        format!("event_type: {}", doc.event_type.as_str()),
    ];
    if let Some(role) = doc.role {
        lines.push(format!("role: {}", role.as_str()));
    }
    if !doc.rank_bucket.trim().is_empty() {
        lines.push(format!(
            "rank_bucket: {}",
            semantic_header_value(&doc.rank_bucket, 80)
        ));
    }
    if let Some(provider) = doc.provider {
        lines.push(format!("provider: {}", provider.as_str()));
    }
    if let Some(source_format) = doc.source_format.as_deref() {
        lines.push(format!(
            "source_format: {}",
            semantic_header_value(source_format, 120)
        ));
    }
    if let Some(agent_type) = doc.agent_type {
        lines.push(format!("agent_type: {}", agent_type.as_str()));
    }
    if let Some(is_primary) = doc.session_is_primary {
        lines.push(format!(
            "session_scope: {}",
            if is_primary { "primary" } else { "subagent" }
        ));
    }
    if let Some(workspace) = doc.record_workspace.as_deref() {
        lines.push(format!(
            "workspace_hint: {}",
            semantic_header_value(workspace, 160)
        ));
    }
    if let Some(cwd) = doc.cwd.as_deref().and_then(path_basename) {
        lines.push(format!("cwd_hint: {}", semantic_header_value(cwd, 120)));
    }
    if let Some(path) = doc.raw_source_path.as_deref().and_then(path_basename) {
        lines.push(format!(
            "source_file_hint: {}",
            semantic_header_value(path, 120)
        ));
    }
    if let Some(title) = doc.record_title.as_deref() {
        lines.push(format!("title_hint: {}", semantic_header_value(title, 180)));
    }
    if let Some(kind) = doc.record_kind.as_deref() {
        lines.push(format!("record_kind: {}", semantic_header_value(kind, 80)));
    }
    lines.join("\n")
}

pub(super) fn semantic_header_value(value: &str, max_chars: usize) -> String {
    let sanitized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = sanitized.chars().take(max_chars).collect::<String>();
    if sanitized.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

pub(super) fn path_basename(path: &str) -> Option<&str> {
    Path::new(path).file_name().and_then(|value| value.to_str())
}

pub(super) fn semantic_text_chunks(text: &str) -> Vec<(usize, usize, String)> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }
    if chars.len() <= SEMANTIC_CHUNK_TARGET_CHARS {
        return vec![(0, chars.len(), text.to_owned())];
    }

    let mut chunks = Vec::new();
    let mut start = 0_usize;
    while start < chars.len() {
        let mut end = start
            .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
            .min(chars.len());
        if end < chars.len() {
            let boundary_floor = end.saturating_sub(150).max(start + 1);
            for index in (boundary_floor..end).rev() {
                if chars[index].is_whitespace() {
                    end = index + 1;
                    break;
                }
            }
        }
        if end <= start {
            end = start
                .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
                .min(chars.len());
        }
        let chunk = chars[start..end].iter().collect::<String>();
        chunks.push((start, end, chunk));
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(SEMANTIC_CHUNK_OVERLAP_CHARS);
    }
    chunks
}

pub(super) fn semantic_text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn compare_semantic_hits_desc(
    left: &SemanticVectorHit,
    right: &SemanticVectorHit,
) -> std::cmp::Ordering {
    right
        .similarity
        .total_cmp(&left.similarity)
        .then_with(|| left.event_id.cmp(&right.event_id))
}

pub(super) fn semantic_hits_for_query(
    store: &Store,
    vector_store: &SemanticVectorStore,
    query_embedding: &[f32],
    limit: usize,
) -> Result<SemanticHitSearch> {
    let vector_search = vector_store.search(query_embedding, limit)?;
    let mut diagnostics = SemanticRetrievalDiagnostics {
        vector_backend: vector_search.stats.backend,
        vector_scan_ms: Some(vector_search.stats.scan_ms),
        chunks_scanned: Some(vector_search.stats.chunks_scanned),
        vector_bytes_read: Some(vector_search.stats.vector_bytes_read),
        events_scored: Some(vector_search.stats.events_scored),
        ..SemanticRetrievalDiagnostics::default()
    };
    let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
    for hit in vector_search.hits {
        let replace = best_by_event
            .get(&hit.event_id)
            .map(|existing| hit.similarity > existing.similarity)
            .unwrap_or(true);
        if replace {
            best_by_event.insert(hit.event_id, hit);
        }
    }
    let mut vector_hits = best_by_event.into_values().collect::<Vec<_>>();
    vector_hits.sort_by(compare_semantic_hits_desc);
    let candidate_chunk_ranges = vector_hits
        .iter()
        .map(|hit| (hit.event_id, (hit.start_char, hit.end_char)))
        .collect::<HashMap<_, _>>();
    let hydration_started = Instant::now();
    let canonical_snapshot = store.semantic_projection_snapshot_by_id(&candidate_chunk_ranges)?;
    diagnostics.hydration_ms = Some(hydration_started.elapsed().as_millis() as u64);
    let current_hashes = current_semantic_source_hashes(&canonical_snapshot.documents);
    let before_stale_filter = vector_hits.len();
    vector_hits.retain(|hit| {
        current_hashes
            .get(&hit.event_id)
            .is_some_and(|hash| hash == &hit.source_text_hash)
    });
    diagnostics.stale_events_dropped = Some(before_stale_filter.saturating_sub(vector_hits.len()));
    if vector_hits.len() > limit {
        vector_hits.truncate(limit);
    }
    let hydrated_by_id = canonical_snapshot
        .hits
        .into_iter()
        .map(|hit| (hit.event_id, hit))
        .collect::<HashMap<_, _>>();
    let mut hits = Vec::new();
    for vector_hit in vector_hits {
        if let Some(hit) = hydrated_by_id.get(&vector_hit.event_id).cloned() {
            hits.push(ctx_history_search::SemanticEventHit {
                hit,
                similarity: vector_hit.similarity,
            });
        }
    }
    diagnostics.semantic_candidates = Some(hits.len());
    Ok(SemanticHitSearch { hits, diagnostics })
}

pub(super) fn current_semantic_source_hashes(
    documents: &[EventEmbeddingDocument],
) -> HashMap<Uuid, String> {
    documents
        .iter()
        .map(|doc| {
            let source_text = semantic_source_text(&doc.text);
            (doc.event_id, semantic_document_hash(doc, &source_text))
        })
        .collect()
}
