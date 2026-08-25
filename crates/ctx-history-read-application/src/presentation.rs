use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fmt,
    ops::Range,
};

use anyhow::{anyhow, Result};
use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index_format::search_projection::{
    project_search_content, visit_body_analyzer_tokens, SearchContentProjection, SearchFragmentKind,
};
use ctx_history_index_query::{
    CoreEventPageBudget, CoreEventRecord, VerifiedIndex, LEXICAL_QUERY_LIMITS,
};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation as _};
use uuid::Uuid;

use crate::{NormalizedSearchQuery, SearchEventMetadata, SearchHit};

pub const MAX_SEARCH_RESULTS: usize = 200;
pub const SEARCH_SNIPPET_MAX_CHARS: usize = 320;
pub const SEARCH_SNIPPET_MAX_BYTES: usize = 16 * 1024;
const SEARCH_CORE_RECORD_BUDGET: CoreEventPageBudget =
    CoreEventPageBudget::new(MAX_ENCODED_CORE_RECORD_BYTES, MAX_CORE_CONTENT_BYTES);
pub const SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES: usize =
    MAX_SEARCH_RESULTS * SEARCH_SNIPPET_MAX_BYTES;

const SEARCH_EXCERPT_ELLIPSIS: &str = "…";
const SEARCH_EXCERPT_MAX_QUERY_TERMS: usize = 32;
const SEARCH_EXCERPT_MAX_LOCAL_OCCURRENCES: usize = SEARCH_SNIPPET_MAX_CHARS;

/// Bounded query result state derived from one complete stored Core record.
#[derive(Debug, PartialEq, Eq)]
pub struct SearchPresentation {
    pub event_id: Uuid,
    pub snippet: String,
    pub snippet_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchPresentationHydrationBudget {
    pub maximum_retained_snippet_bytes: usize,
}

pub const SEARCH_PRESENTATION_HYDRATION_BUDGET: SearchPresentationHydrationBudget =
    SearchPresentationHydrationBudget {
        maximum_retained_snippet_bytes: SEARCH_PRESENTATION_MAX_RETAINED_SNIPPET_BYTES,
    };

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPresentationRetentionBudgetExceeded {
    pub event_id: Uuid,
    pub retained_snippet_bytes: usize,
    pub maximum_retained_snippet_bytes: usize,
}

impl fmt::Display for SearchPresentationRetentionBudgetExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core search event {} cannot fit the bounded search presentation retention budget (retained snippets: {}/{})",
            self.event_id,
            self.retained_snippet_bytes,
            self.maximum_retained_snippet_bytes,
        )
    }
}

impl std::error::Error for SearchPresentationRetentionBudgetExceeded {}

pub(crate) fn presentations_for_search_hits(
    index: &VerifiedIndex,
    hits: &[SearchHit],
    query: &NormalizedSearchQuery,
) -> Result<Vec<SearchPresentation>> {
    presentations_for_search_hits_with_budget(
        index,
        hits,
        query,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
}

pub fn presentations_for_search_hits_with_budget(
    index: &VerifiedIndex,
    hits: &[SearchHit],
    query: &NormalizedSearchQuery,
    budget: SearchPresentationHydrationBudget,
) -> Result<Vec<SearchPresentation>> {
    if hits.len() > MAX_SEARCH_RESULTS {
        return Err(anyhow!(
            "search presentation cannot hydrate more than {MAX_SEARCH_RESULTS} hits"
        ));
    }
    if budget.maximum_retained_snippet_bytes == 0 {
        return Err(anyhow!(
            "search presentation hydration budget must be positive"
        ));
    }

    let mut requested = BTreeSet::new();
    for hit in hits {
        if !requested.insert(hit.event.event_id) {
            return Err(anyhow!(
                "search result duplicated Core event {}",
                hit.event.event_id
            ));
        }
    }
    let event_ids = hits
        .iter()
        .map(|hit| hit.event.event_id)
        .collect::<Vec<_>>();

    // Execute one generation-pinned Tantivy selection. The returned iterator
    // decodes exactly one complete Core record at a time, allowing each body
    // to be projected and discarded before the next record is materialized.
    let mut records = index
        .stream_core_events_by_ids_with_strict_per_record_budget(
            &event_ids,
            hits.len(),
            SEARCH_CORE_RECORD_BUDGET,
        )?
        .ok_or_else(|| {
            anyhow!(
                "pinned Core lookup omitted search event {}",
                event_ids.first().copied().unwrap_or_else(Uuid::nil)
            )
        })?;

    // The query is analyzed once for the whole hydrated page with the same
    // analyzer used for body_search. Every result reuses this fixed term map.
    let query_texts = query.texts();
    let query_terms = analyzed_query_terms(&query_texts);
    let mut presentations = Vec::with_capacity(hits.len());
    let mut retained_snippet_bytes = 0_usize;
    for hit in hits {
        let event_id = hit.event.event_id;
        let record = records
            .next()
            .transpose()?
            .ok_or_else(|| anyhow!("pinned Core lookup omitted search event {event_id}"))?;
        let (presentation, snippet_bytes) =
            search_presentation_projection(record, &hit.event, &query_terms)?;
        let next_retained_snippet_bytes = retained_snippet_bytes
            .checked_add(snippet_bytes)
            .ok_or_else(|| {
                search_presentation_retention_budget_error(event_id, retained_snippet_bytes, budget)
            })?;
        if next_retained_snippet_bytes > budget.maximum_retained_snippet_bytes {
            return Err(search_presentation_retention_budget_error(
                event_id,
                next_retained_snippet_bytes,
                budget,
            ));
        }
        retained_snippet_bytes = next_retained_snippet_bytes;
        presentations.push(presentation);
    }
    if records.next().transpose()?.is_some() {
        return Err(anyhow!(
            "pinned Core lookup returned more search records than requested"
        ));
    }
    Ok(presentations)
}

fn search_presentation_projection(
    record: CoreEventRecord,
    expected_event: &SearchEventMetadata,
    query_terms: &AnalyzedQueryTerms,
) -> Result<(SearchPresentation, usize)> {
    let CoreEventRecord { event, core_record } = record;
    if event.event_id != core_record.event_id
        || event.session_id != core_record.session_id
        || SearchEventMetadata::from(&event) != *expected_event
    {
        return Err(anyhow!(
            "pinned Core lookup returned misaligned metadata for search event {}",
            expected_event.event_id
        ));
    }
    let projection = project_search_content(core_record.content)?.ok_or_else(|| {
        anyhow!(
            "Core search event {} has no searchable body projection",
            event.event_id
        )
    })?;
    let (snippet, snippet_truncated) = search_excerpt(&projection, query_terms);
    let retained_snippet_bytes = snippet.len();
    // Neither the complete searchable projection nor the remainder of Core
    // crosses the search presentation boundary.
    drop(projection);
    drop(event);
    Ok((
        SearchPresentation {
            event_id: expected_event.event_id,
            snippet,
            snippet_truncated,
        },
        retained_snippet_bytes,
    ))
}

fn search_presentation_retention_budget_error(
    event_id: Uuid,
    retained_snippet_bytes: usize,
    budget: SearchPresentationHydrationBudget,
) -> anyhow::Error {
    anyhow::Error::new(SearchPresentationRetentionBudgetExceeded {
        event_id,
        retained_snippet_bytes,
        maximum_retained_snippet_bytes: budget.maximum_retained_snippet_bytes,
    })
}

#[derive(Debug, Default)]
struct AnalyzedQueryTerms {
    ids: HashMap<String, u8>,
}

impl AnalyzedQueryTerms {
    fn id(&self, token: &str) -> Option<usize> {
        self.ids.get(token).copied().map(usize::from)
    }

    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

fn analyzed_query_terms(query_texts: &[&str]) -> AnalyzedQueryTerms {
    let maximum_terms = LEXICAL_QUERY_LIMITS
        .maximum_unique_tokens
        .min(SEARCH_EXCERPT_MAX_QUERY_TERMS);
    let mut terms = AnalyzedQueryTerms::default();
    for query_text in query_texts {
        visit_body_analyzer_tokens(query_text, |token, _| {
            if terms.ids.contains_key(token) {
                return true;
            }
            if terms.ids.len() >= maximum_terms {
                return false;
            }
            let term_id = u8::try_from(terms.ids.len()).unwrap_or(u8::MAX);
            terms.ids.insert(token.to_owned(), term_id);
            true
        });
        if terms.ids.len() >= maximum_terms {
            break;
        }
    }
    terms
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExcerptTextSource {
    Exact,
    DecodedJsonString,
}

#[derive(Debug, Clone)]
struct TextSelection {
    coverage: usize,
    observed_terms: u32,
    byte_range: Range<usize>,
    grapheme_range: Range<usize>,
}

impl Default for TextSelection {
    fn default() -> Self {
        Self {
            coverage: 0,
            observed_terms: 0,
            byte_range: 0..0,
            grapheme_range: 0..0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TermOccurrence {
    term_id: usize,
    byte_start: usize,
    byte_end: usize,
    grapheme_start: usize,
    grapheme_end: usize,
}

#[derive(Debug, Default, Clone, Copy)]
struct ExcerptWorkStats {
    analyzed_tokens: usize,
    decoded_analyzed_tokens: usize,
    query_membership_lookups: usize,
    alignment_bytes_traversed: usize,
    alignment_graphemes_traversed: usize,
    local_context_calls: usize,
    local_context_bytes_traversed: usize,
    local_context_graphemes_traversed: usize,
    decoded_fit_bytes_traversed: usize,
    decoded_fit_graphemes_traversed: usize,
    peak_retained_occurrences: usize,
    peak_retained_graphemes: usize,
}

#[derive(Debug, Clone)]
struct PreparedExcerpt {
    fragment_index: usize,
    source: ExcerptTextSource,
    coverage: usize,
    readability: u8,
    tight_graphemes: usize,
    match_byte_start: usize,
    display_range: Range<usize>,
    leading_ellipsis: bool,
    trailing_ellipsis: bool,
}

#[derive(Debug, Clone, Copy)]
struct ExcerptContext {
    kind: SearchFragmentKind,
    fragment_index: usize,
    fragment_count: usize,
    source: ExcerptTextSource,
    include_ellipses: bool,
}

fn search_excerpt(
    projection: &SearchContentProjection,
    query_terms: &AnalyzedQueryTerms,
) -> (String, bool) {
    let mut work = ExcerptWorkStats::default();
    search_excerpt_with_work(projection, query_terms, &mut work)
}

fn search_excerpt_with_work(
    projection: &SearchContentProjection,
    query_terms: &AnalyzedQueryTerms,
    work: &mut ExcerptWorkStats,
) -> (String, bool) {
    let mut best = None;
    let fragment_count = projection.fragments().len();
    for (fragment_index, fragment) in projection.fragments().iter().enumerate() {
        let preceding_fragments = fragment_index > 0;
        let following_fragments = fragment_index + 1 < fragment_count;
        let exact_text = fragment.index_text(projection);
        let exact = select_text_window(
            exact_text,
            query_terms,
            u32::MAX,
            preceding_fragments,
            following_fragments,
            true,
            work,
        );
        retain_prepared_excerpt(
            &mut best,
            prepare_excerpt(
                exact_text,
                &exact,
                ExcerptContext {
                    kind: fragment.kind(),
                    fragment_index,
                    fragment_count,
                    source: ExcerptTextSource::Exact,
                    include_ellipses: true,
                },
                work,
            ),
        );

        if fragment.has_decoded_json_display()
            && decoded_display_fits_local_bound(fragment.display_text(projection), work)
        {
            let display_text = fragment.display_text(projection);
            let analyzed_tokens_before = work.analyzed_tokens;
            let decoded = select_text_window(
                display_text,
                query_terms,
                exact.observed_terms,
                preceding_fragments,
                following_fragments,
                true,
                work,
            );
            work.decoded_analyzed_tokens = work
                .decoded_analyzed_tokens
                .saturating_add(work.analyzed_tokens.saturating_sub(analyzed_tokens_before));
            retain_prepared_excerpt(
                &mut best,
                prepare_excerpt(
                    display_text,
                    &decoded,
                    ExcerptContext {
                        kind: fragment.kind(),
                        fragment_index,
                        fragment_count,
                        source: ExcerptTextSource::DecodedJsonString,
                        include_ellipses: true,
                    },
                    work,
                ),
            );
        }
    }

    let Some(best) = best else {
        return (String::new(), true);
    };
    let fragment = &projection.fragments()[best.fragment_index];
    let text = match best.source {
        ExcerptTextSource::Exact => fragment.index_text(projection),
        ExcerptTextSource::DecodedJsonString => fragment.display_text(projection),
    };
    render_prepared_excerpt(text, &best, true)
}

fn decoded_display_fits_local_bound(text: &str, work: &mut ExcerptWorkStats) -> bool {
    if text.len() > SEARCH_SNIPPET_MAX_BYTES {
        return false;
    }
    for (index, grapheme) in text.graphemes(true).enumerate() {
        work.decoded_fit_bytes_traversed = work
            .decoded_fit_bytes_traversed
            .saturating_add(grapheme.len());
        work.decoded_fit_graphemes_traversed =
            work.decoded_fit_graphemes_traversed.saturating_add(1);
        if index >= SEARCH_SNIPPET_MAX_CHARS {
            return false;
        }
    }
    true
}

fn select_text_window(
    text: &str,
    query_terms: &AnalyzedQueryTerms,
    allowed_terms: u32,
    preceding_fragments: bool,
    following_fragments: bool,
    include_ellipses: bool,
    work: &mut ExcerptWorkStats,
) -> TextSelection {
    if query_terms.is_empty() || allowed_terms == 0 {
        return TextSelection::default();
    }

    let mut selected = TextSelection::default();
    let mut occurrences = VecDeque::with_capacity(SEARCH_EXCERPT_MAX_LOCAL_OCCURRENCES);
    let mut counts = [0_u16; SEARCH_EXCERPT_MAX_QUERY_TERMS];
    let mut coverage = 0_usize;
    let mut graphemes = text.grapheme_indices(true).peekable();
    let mut next_grapheme_index = 0_usize;
    let mut current_grapheme_index = 0_usize;

    visit_body_analyzer_tokens(text, |token, token_range| {
        work.analyzed_tokens = work.analyzed_tokens.saturating_add(1);
        while graphemes
            .peek()
            .is_some_and(|(start, _)| *start <= token_range.start)
        {
            if let Some((_, grapheme)) = graphemes.next() {
                work.alignment_bytes_traversed = work
                    .alignment_bytes_traversed
                    .saturating_add(grapheme.len());
                work.alignment_graphemes_traversed =
                    work.alignment_graphemes_traversed.saturating_add(1);
            }
            current_grapheme_index = next_grapheme_index;
            next_grapheme_index = next_grapheme_index.saturating_add(1);
        }

        work.query_membership_lookups = work.query_membership_lookups.saturating_add(1);
        let Some(term_id) = query_terms.id(token) else {
            return true;
        };
        let term_bit = 1_u32 << term_id;
        selected.observed_terms |= term_bit;
        if allowed_terms & term_bit == 0 {
            return true;
        }

        let token_graphemes = text[token_range.clone()].graphemes(true).count().max(1);
        let occurrence = TermOccurrence {
            term_id,
            byte_start: token_range.start,
            byte_end: token_range.end,
            grapheme_start: current_grapheme_index,
            grapheme_end: current_grapheme_index.saturating_add(token_graphemes),
        };

        if occurrences.len() == SEARCH_EXCERPT_MAX_LOCAL_OCCURRENCES {
            pop_front_occurrence(&mut occurrences, &mut counts, &mut coverage);
        }
        occurrences.push_back(occurrence);
        if counts[term_id] == 0 {
            coverage = coverage.saturating_add(1);
        }
        counts[term_id] = counts[term_id].saturating_add(1);

        while !occurrences_fit(
            &occurrences,
            text.len(),
            preceding_fragments,
            following_fragments,
            include_ellipses,
        ) {
            pop_front_occurrence(&mut occurrences, &mut counts, &mut coverage);
        }
        while occurrences
            .front()
            .is_some_and(|front| counts[front.term_id] > 1)
        {
            pop_front_occurrence(&mut occurrences, &mut counts, &mut coverage);
        }

        work.peak_retained_occurrences = work.peak_retained_occurrences.max(occurrences.len());
        if let (Some(first), Some(last)) = (occurrences.front(), occurrences.back()) {
            work.peak_retained_graphemes = work
                .peak_retained_graphemes
                .max(last.grapheme_end.saturating_sub(first.grapheme_start));
            let candidate = TextSelection {
                coverage,
                observed_terms: selected.observed_terms,
                byte_range: first.byte_start..last.byte_end,
                grapheme_range: first.grapheme_start..last.grapheme_end,
            };
            if text_selection_is_preferred(&candidate, &selected) {
                let observed_terms = selected.observed_terms;
                selected = candidate;
                selected.observed_terms = observed_terms;
            }
        }
        true
    });
    selected
}

fn pop_front_occurrence(
    occurrences: &mut VecDeque<TermOccurrence>,
    counts: &mut [u16; SEARCH_EXCERPT_MAX_QUERY_TERMS],
    coverage: &mut usize,
) {
    let Some(removed) = occurrences.pop_front() else {
        return;
    };
    counts[removed.term_id] = counts[removed.term_id].saturating_sub(1);
    if counts[removed.term_id] == 0 {
        *coverage = coverage.saturating_sub(1);
    }
}

fn occurrences_fit(
    occurrences: &VecDeque<TermOccurrence>,
    text_len: usize,
    preceding_fragments: bool,
    following_fragments: bool,
    include_ellipses: bool,
) -> bool {
    let (Some(first), Some(last)) = (occurrences.front(), occurrences.back()) else {
        return true;
    };
    let omissions = if include_ellipses {
        usize::from(preceding_fragments || first.byte_start > 0)
            + usize::from(following_fragments || last.byte_end < text_len)
    } else {
        0
    };
    last.grapheme_end
        .saturating_sub(first.grapheme_start)
        .saturating_add(omissions)
        <= SEARCH_SNIPPET_MAX_CHARS
        && last
            .byte_end
            .saturating_sub(first.byte_start)
            .saturating_add(omissions.saturating_mul(SEARCH_EXCERPT_ELLIPSIS.len()))
            <= SEARCH_SNIPPET_MAX_BYTES
}

fn text_selection_is_preferred(candidate: &TextSelection, current: &TextSelection) -> bool {
    candidate.coverage > current.coverage
        || (candidate.coverage == current.coverage
            && candidate.grapheme_range.len() < current.grapheme_range.len())
        || (candidate.coverage == current.coverage
            && candidate.grapheme_range.len() == current.grapheme_range.len()
            && candidate.byte_range.start < current.byte_range.start)
}

fn prepare_excerpt(
    text: &str,
    selection: &TextSelection,
    context: ExcerptContext,
    work: &mut ExcerptWorkStats,
) -> Option<PreparedExcerpt> {
    if text.is_empty() {
        return None;
    }
    let required_bytes = (selection.coverage > 0).then_some(&selection.byte_range);
    let mut window = local_grapheme_window(text, required_bytes, work)?;

    for _ in 0..2 {
        let raw = window_byte_range(&window)?;
        let omissions = if context.include_ellipses {
            usize::from(context.fragment_index > 0 || raw.start > 0)
                + usize::from(
                    context.fragment_index + 1 < context.fragment_count || raw.end < text.len(),
                )
        } else {
            0
        };
        let maximum_graphemes = SEARCH_SNIPPET_MAX_CHARS.checked_sub(omissions)?;
        let maximum_bytes = SEARCH_SNIPPET_MAX_BYTES
            .checked_sub(omissions.saturating_mul(SEARCH_EXCERPT_ELLIPSIS.len()))?;
        trim_local_window(
            &mut window,
            required_bytes,
            maximum_graphemes,
            maximum_bytes,
        )?;
    }

    let raw = window_byte_range(&window)?;
    let display_range = snap_to_safe_boundaries(text, &window, raw, required_bytes);
    if display_range.is_empty()
        || required_bytes.is_some_and(|required| {
            display_range.start > required.start || display_range.end < required.end
        })
    {
        return None;
    }
    let leading_ellipsis = context.fragment_index > 0 || display_range.start > 0;
    let trailing_ellipsis =
        context.fragment_index + 1 < context.fragment_count || display_range.end < text.len();
    Some(PreparedExcerpt {
        fragment_index: context.fragment_index,
        source: context.source,
        coverage: selection.coverage,
        readability: fragment_readability(context.kind, context.source),
        tight_graphemes: selection.grapheme_range.len(),
        match_byte_start: selection.byte_range.start,
        display_range,
        leading_ellipsis,
        trailing_ellipsis,
    })
}

fn local_grapheme_window(
    text: &str,
    required: Option<&Range<usize>>,
    work: &mut ExcerptWorkStats,
) -> Option<VecDeque<Range<usize>>> {
    work.local_context_calls = work.local_context_calls.saturating_add(1);
    let envelope = local_byte_envelope(text, required)?;
    let window = match required {
        Some(required) => matched_local_grapheme_window(text, required, &envelope, work)?,
        None => fallback_local_grapheme_window(text, &envelope, work)?,
    };
    work.peak_retained_graphemes = work.peak_retained_graphemes.max(window.len());
    Some(window)
}

fn local_byte_envelope(text: &str, required: Option<&Range<usize>>) -> Option<Range<usize>> {
    let Some(required) = required else {
        let mut end = text.len().min(SEARCH_SNIPPET_MAX_BYTES);
        while end > 0 && !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        return (end > 0).then_some(0..end);
    };
    if required.is_empty()
        || required.end > text.len()
        || !text.is_char_boundary(required.start)
        || !text.is_char_boundary(required.end)
        || required.len() > SEARCH_SNIPPET_MAX_BYTES
    {
        return None;
    }

    let remaining = SEARCH_SNIPPET_MAX_BYTES.saturating_sub(required.len());
    let desired_left = remaining / 2;
    let desired_right = remaining.saturating_sub(desired_left);
    let available_left = required.start;
    let available_right = text.len().saturating_sub(required.end);
    let mut left = desired_left.min(available_left);
    let mut right = desired_right.min(available_right);
    let mut unused = remaining.saturating_sub(left).saturating_sub(right);
    let extra_left = unused.min(available_left.saturating_sub(left));
    left = left.saturating_add(extra_left);
    unused = unused.saturating_sub(extra_left);
    right = right.saturating_add(unused.min(available_right.saturating_sub(right)));

    let mut start = required.start.saturating_sub(left);
    while start < required.start && !text.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    let mut end = required.end.saturating_add(right).min(text.len());
    while end > required.end && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (start <= required.start && end >= required.end).then_some(start..end)
}

fn matched_local_grapheme_window(
    text: &str,
    required: &Range<usize>,
    envelope: &Range<usize>,
    work: &mut ExcerptWorkStats,
) -> Option<VecDeque<Range<usize>>> {
    // Analyzer tokens may start or end inside a full-text extended grapheme
    // (notably an Indic conjunct under GB9c). Align both selected endpoints
    // within the fixed byte envelope before segmenting local context.
    let required = bounded_full_grapheme_range(text, required, envelope)?;
    let mut forward = text[required.start..envelope.end]
        .grapheme_indices(true)
        .map(|(relative, grapheme)| {
            let start = required.start.saturating_add(relative);
            start..start.saturating_add(grapheme.len())
        });
    let mut required_ranges = VecDeque::with_capacity(SEARCH_SNIPPET_MAX_CHARS);
    while required_ranges
        .back()
        .is_none_or(|grapheme: &Range<usize>| grapheme.end < required.end)
    {
        let grapheme = forward.next()?;
        record_local_grapheme(work, &grapheme);
        if required_ranges.len() == SEARCH_SNIPPET_MAX_CHARS {
            return None;
        }
        required_ranges.push_back(grapheme);
    }
    if required_ranges
        .back()
        .is_some_and(|grapheme| grapheme.end == envelope.end && envelope.end < text.len())
    {
        return None;
    }

    let remaining = SEARCH_SNIPPET_MAX_CHARS.saturating_sub(required_ranges.len());
    let left_target = remaining / 2;
    let mut left = Vec::with_capacity(left_target);
    let mut left_graphemes = text[envelope.start..required.start]
        .grapheme_indices(true)
        .rev();
    for (relative, grapheme) in left_graphemes.by_ref().take(left_target) {
        let start = envelope.start.saturating_add(relative);
        let range = start..start.saturating_add(grapheme.len());
        record_local_grapheme(work, &range);
        left.push(range);
    }

    let right_target = SEARCH_SNIPPET_MAX_CHARS
        .saturating_sub(required_ranges.len())
        .saturating_sub(left.len());
    let mut right = Vec::with_capacity(right_target);
    for grapheme in forward.take(right_target) {
        record_local_grapheme(work, &grapheme);
        right.push(grapheme);
    }
    if envelope.end < text.len()
        && right
            .last()
            .is_some_and(|grapheme| grapheme.end == envelope.end)
    {
        let _ = right.pop();
    }
    let extra_left = SEARCH_SNIPPET_MAX_CHARS
        .saturating_sub(required_ranges.len())
        .saturating_sub(left.len())
        .saturating_sub(right.len());
    for (relative, grapheme) in left_graphemes.take(extra_left) {
        let start = envelope.start.saturating_add(relative);
        let range = start..start.saturating_add(grapheme.len());
        record_local_grapheme(work, &range);
        left.push(range);
    }
    if envelope.start > 0
        && left
            .last()
            .is_some_and(|grapheme| grapheme.start == envelope.start)
    {
        let _ = left.pop();
    }

    let mut window = VecDeque::with_capacity(SEARCH_SNIPPET_MAX_CHARS);
    window.extend(left.into_iter().rev());
    window.extend(required_ranges);
    window.extend(right);
    let range = window_byte_range(&window)?;
    (range.start <= required.start && range.end >= required.end).then_some(window)
}

fn bounded_full_grapheme_range(
    text: &str,
    required: &Range<usize>,
    envelope: &Range<usize>,
) -> Option<Range<usize>> {
    let start = bounded_grapheme_boundary(text, required.start, envelope, false)?;
    let end = bounded_grapheme_boundary(text, required.end, envelope, true)?;
    (start < end && start >= envelope.start && end <= envelope.end).then_some(start..end)
}

fn bounded_grapheme_boundary(
    text: &str,
    offset: usize,
    envelope: &Range<usize>,
    forward: bool,
) -> Option<usize> {
    let mut cursor = GraphemeCursor::new(offset, text.len(), true);
    let chunk = &text[envelope.clone()];
    if cursor.is_boundary(chunk, envelope.start).ok()? {
        return Some(offset);
    }
    if forward {
        cursor.next_boundary(chunk, envelope.start).ok().flatten()
    } else {
        cursor.prev_boundary(chunk, envelope.start).ok().flatten()
    }
}

fn fallback_local_grapheme_window(
    text: &str,
    envelope: &Range<usize>,
    work: &mut ExcerptWorkStats,
) -> Option<VecDeque<Range<usize>>> {
    let mut window = VecDeque::with_capacity(SEARCH_SNIPPET_MAX_CHARS);
    for (relative, grapheme) in text[envelope.clone()]
        .grapheme_indices(true)
        .take(SEARCH_SNIPPET_MAX_CHARS)
    {
        let start = envelope.start.saturating_add(relative);
        let range = start..start.saturating_add(grapheme.len());
        record_local_grapheme(work, &range);
        window.push_back(range);
    }
    if envelope.end < text.len()
        && window
            .back()
            .is_some_and(|grapheme| grapheme.end == envelope.end)
    {
        let _ = window.pop_back();
    }
    (!window.is_empty()).then_some(window)
}

fn record_local_grapheme(work: &mut ExcerptWorkStats, grapheme: &Range<usize>) {
    work.local_context_bytes_traversed = work
        .local_context_bytes_traversed
        .saturating_add(grapheme.len());
    work.local_context_graphemes_traversed =
        work.local_context_graphemes_traversed.saturating_add(1);
}

fn trim_local_window(
    window: &mut VecDeque<Range<usize>>,
    required: Option<&Range<usize>>,
    maximum_graphemes: usize,
    maximum_bytes: usize,
) -> Option<()> {
    while window.len() > maximum_graphemes || window_byte_range(window)?.len() > maximum_bytes {
        let front = window.front()?.clone();
        let back = window.back()?.clone();
        let remove_front = match required {
            Some(required) => {
                let front_is_context = front.end <= required.start;
                let back_is_context = back.start >= required.end;
                match (front_is_context, back_is_context) {
                    (true, true) => {
                        required.start.saturating_sub(front.start)
                            > back.end.saturating_sub(required.end)
                    }
                    (true, false) => true,
                    (false, true) => false,
                    (false, false) => return None,
                }
            }
            None => front.len() > maximum_bytes,
        };
        if remove_front {
            let _ = window.pop_front();
        } else {
            let _ = window.pop_back();
        }
    }
    (!window.is_empty()).then_some(())
}

fn window_byte_range(window: &VecDeque<Range<usize>>) -> Option<Range<usize>> {
    Some(window.front()?.start..window.back()?.end)
}

fn snap_to_safe_boundaries(
    text: &str,
    window: &VecDeque<Range<usize>>,
    raw: Range<usize>,
    required: Option<&Range<usize>>,
) -> Range<usize> {
    let required_start = required.map_or(raw.end, |range| range.start);
    let required_end = required.map_or(raw.start, |range| range.end);
    let maximum_discarded_graphemes = window.len().saturating_div(2);

    let start = window
        .iter()
        .enumerate()
        .take_while(|(_, grapheme)| grapheme.start <= required_start)
        .find(|(index, grapheme)| {
            *index <= maximum_discarded_graphemes && is_safe_text_boundary(text, grapheme.start)
        })
        .map_or(raw.start, |(_, grapheme)| grapheme.start);
    let end = window
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, grapheme)| grapheme.end >= required_end)
        .find(|(index, grapheme)| {
            window.len().saturating_sub(index.saturating_add(1)) <= maximum_discarded_graphemes
                && is_safe_text_boundary(text, grapheme.end)
        })
        .map_or(raw.end, |(_, grapheme)| grapheme.end);

    let untrimmed = start..end;
    let mut snapped = untrimmed.clone();
    let leading_limit = required.map_or(snapped.end, |range| range.start);
    let leading = &text[snapped.start..leading_limit];
    snapped.start += leading.len().saturating_sub(leading.trim_start().len());
    let trailing_limit = required.map_or(snapped.start, |range| range.end);
    let trailing = &text[trailing_limit..snapped.end];
    snapped.end -= trailing.len().saturating_sub(trailing.trim_end().len());
    if snapped.is_empty() {
        untrimmed
    } else {
        snapped
    }
}

fn is_safe_text_boundary(text: &str, boundary: usize) -> bool {
    if boundary == 0 || boundary == text.len() {
        return true;
    }
    let Some(previous) = text[..boundary].chars().next_back() else {
        return true;
    };
    let Some(next) = text[boundary..].chars().next() else {
        return true;
    };
    previous == '\n'
        || next == '\n'
        || previous.is_whitespace()
        || next.is_whitespace()
        || previous.is_alphanumeric() != next.is_alphanumeric()
        || (!previous.is_alphanumeric() && !next.is_alphanumeric())
}

fn fragment_readability(kind: SearchFragmentKind, source: ExcerptTextSource) -> u8 {
    use SearchFragmentKind as Kind;
    match kind {
        Kind::NormalizedBody | Kind::ResultText => 0,
        Kind::ResultStructuredContent if source == ExcerptTextSource::DecodedJsonString => 0,
        Kind::StructuredContent if source == ExcerptTextSource::DecodedJsonString => 1,
        Kind::ResultStatus | Kind::LiteralFact => 1,
        Kind::StructuredContent | Kind::ResultStructuredContent => 2,
        Kind::InvocationProtocol
        | Kind::InvocationServer
        | Kind::InvocationTool
        | Kind::InvocationArguments => 3,
    }
}

fn retain_prepared_excerpt(best: &mut Option<PreparedExcerpt>, candidate: Option<PreparedExcerpt>) {
    let Some(candidate) = candidate else {
        return;
    };
    if best
        .as_ref()
        .is_none_or(|current| prepared_excerpt_is_preferred(&candidate, current))
    {
        *best = Some(candidate);
    }
}

fn prepared_excerpt_is_preferred(candidate: &PreparedExcerpt, current: &PreparedExcerpt) -> bool {
    candidate.coverage > current.coverage
        || (candidate.coverage == current.coverage && candidate.readability < current.readability)
        || (candidate.coverage == current.coverage
            && candidate.readability == current.readability
            && candidate.tight_graphemes < current.tight_graphemes)
        || (candidate.coverage == current.coverage
            && candidate.readability == current.readability
            && candidate.tight_graphemes == current.tight_graphemes
            && candidate.fragment_index < current.fragment_index)
        || (candidate.coverage == current.coverage
            && candidate.readability == current.readability
            && candidate.tight_graphemes == current.tight_graphemes
            && candidate.fragment_index == current.fragment_index
            && candidate.match_byte_start < current.match_byte_start)
}

fn render_prepared_excerpt(
    text: &str,
    excerpt: &PreparedExcerpt,
    include_ellipses: bool,
) -> (String, bool) {
    let content = &text[excerpt.display_range.clone()];
    let mut snippet = String::with_capacity(content.len().saturating_add(6));
    if include_ellipses && excerpt.leading_ellipsis {
        snippet.push_str(SEARCH_EXCERPT_ELLIPSIS);
    }
    snippet.push_str(content);
    if include_ellipses && excerpt.trailing_ellipsis {
        snippet.push_str(SEARCH_EXCERPT_ELLIPSIS);
    }
    let truncated = excerpt.leading_ellipsis || excerpt.trailing_ellipsis;
    (snippet, truncated)
}

/// Compatibility adapter for direct snippet callers. It uses the same exact
/// analyzer, sliding-window selector, boundary snap, and byte/grapheme clip as
/// hydrated search presentation; only the historical omission of visible
/// ellipses is retained here.
pub fn search_snippet_fragment(body: &str, query_texts: &[&str]) -> (String, bool) {
    let query_terms = analyzed_query_terms(query_texts);
    let mut work = ExcerptWorkStats::default();
    let selected = select_text_window(body, &query_terms, u32::MAX, false, false, false, &mut work);
    let prepared = prepare_excerpt(
        body,
        &selected,
        ExcerptContext {
            kind: SearchFragmentKind::NormalizedBody,
            fragment_index: 0,
            fragment_count: 1,
            source: ExcerptTextSource::Exact,
            include_ellipses: false,
        },
        &mut work,
    );
    prepared.map_or((String::new(), true), |prepared| {
        render_prepared_excerpt(body, &prepared, false)
    })
}

#[cfg(test)]
fn fragment_aware_search_excerpt(
    projection: &SearchContentProjection,
    query_texts: &[&str],
) -> (String, bool) {
    let query_terms = analyzed_query_terms(query_texts);
    search_excerpt(projection, &query_terms)
}

#[cfg(test)]
fn fragment_aware_search_excerpt_with_work(
    projection: &SearchContentProjection,
    query_texts: &[&str],
) -> ((String, bool), ExcerptWorkStats) {
    let query_terms = analyzed_query_terms(query_texts);
    let mut work = ExcerptWorkStats::default();
    let excerpt = search_excerpt_with_work(projection, &query_terms, &mut work);
    (excerpt, work)
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
