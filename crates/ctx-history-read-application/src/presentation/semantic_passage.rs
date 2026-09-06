use ctx_history_index_query::{EventRecord, SemanticPassageSource, SemanticSearchEvidence};
use uuid::Uuid;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPassagePresentation {
    pub evidence: SemanticSearchEvidence,
    pub citations: Vec<SearchPassageCitation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPassageCitation {
    pub event: EventRecord,
    pub content_char_range: Range<usize>,
}

pub(crate) fn semantic_passage_presentation(
    event_id: Uuid,
    evidence: SemanticSearchEvidence,
    source: SemanticPassageSource,
    query: &str,
) -> SearchPresentation {
    let undisplayable = || SearchPresentation {
        event_id,
        snippet: String::new(),
        snippet_truncated: true,
        semantic_passage: Some(SearchPassagePresentation {
            evidence: evidence.clone(),
            citations: Vec::new(),
        }),
    };
    // The vector span uses Unicode scalars. Clip inward against the complete
    // source's grapheme boundaries before selecting a bounded excerpt, so a
    // chunk edge cannot manufacture a partial combining or emoji cluster.
    let mut start = None;
    let mut end = None;
    for boundary in source
        .text
        .grapheme_indices(true)
        .map(|(byte, _)| byte)
        .chain(std::iter::once(source.text.len()))
    {
        if boundary >= source.byte_range.start && start.is_none() {
            start = Some(boundary);
        }
        if boundary <= source.byte_range.end {
            end = Some(boundary);
        } else {
            break;
        }
    }
    let range = start.unwrap_or(0)..end.unwrap_or(0);
    let Some(body) = source
        .text
        .get(range.clone())
        .filter(|body| !body.is_empty())
    else {
        return undisplayable();
    };
    let terms = analyzed_query_terms(&[query]);
    let mut work = ExcerptWorkStats::default();
    let selected = select_text_window(body, &terms, u32::MAX, false, false, true, &mut work);
    let Some(prepared) = prepare_excerpt(
        body,
        &selected,
        ExcerptContext {
            kind: SearchFragmentKind::NormalizedBody,
            fragment_index: 0,
            fragment_count: 1,
            source: ExcerptTextSource::Exact,
            include_ellipses: true,
        },
        &mut work,
    ) else {
        return undisplayable();
    };
    let displayed =
        range.start + prepared.display_range.start..range.start + prepared.display_range.end;
    let (snippet, clipped) = render_prepared_excerpt(body, &prepared, true);
    let citations = source
        .members
        .into_iter()
        .filter_map(|member| {
            let start = displayed.start.max(member.byte_range.start);
            let end = displayed.end.min(member.byte_range.end);
            (start < end).then(|| SearchPassageCitation {
                content_char_range: member.content_start_char
                    + source.text[member.byte_range.start..start].chars().count()
                    ..member.content_start_char
                        + source.text[member.byte_range.start..end].chars().count(),
                event: member.event,
            })
        })
        .collect::<Vec<_>>();
    if citations.is_empty() {
        return undisplayable();
    }
    SearchPresentation {
        event_id,
        snippet,
        snippet_truncated: source.truncated || clipped || range != source.byte_range,
        semantic_passage: Some(SearchPassagePresentation {
            evidence,
            citations,
        }),
    }
}
