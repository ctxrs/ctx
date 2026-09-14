use super::*;
use ctx_history_core::{EventRole, EventType, LiteralFactKind, ProviderDeclaredFact};
use uuid::Uuid;

fn document(body: &str) -> SemanticEventDocument {
    SemanticEventDocument::new(
        Uuid::from_u128(1),
        Some(Uuid::from_u128(2)),
        7,
        0,
        EventType::Message,
        Some(EventRole::User),
        "lite_turn".to_owned(),
        None,
        None,
        None,
        Vec::new(),
        body.to_owned(),
    )
}

fn body(input: &str) -> &str {
    input.split_once("\n\n").map_or(input, |(_, body)| body)
}

fn check_spans(doc: &SemanticEventDocument, chunks: &[SemanticChunkDocument]) {
    let source: Vec<char> = doc.text.chars().collect();
    let mut covered = vec![false; source.len()];
    let mut previous_start = None;
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.event_id, doc.event_id);
        assert_eq!(chunk.seq, 7);
        assert_eq!(chunk.source_text_hash, "source-hash");
        assert_eq!(chunk.chunk_index, index);
        assert!(chunk.start_char < chunk.end_char && chunk.end_char <= source.len());
        if let Some(previous) = previous_start {
            assert!(chunk.start_char > previous);
        }
        previous_start = Some(chunk.start_char);
        assert_eq!(
            body(&chunk.text),
            source[chunk.start_char..chunk.end_char]
                .iter()
                .collect::<String>()
        );
        covered[chunk.start_char..chunk.end_char].fill(true);
    }
    assert!(covered.iter().all(|c| *c));
}

#[test]
fn fitting_inputs_preserve_existing_windows_and_exact_metadata() -> Result<()> {
    let doc = document(&"ordinary code and conversation ".repeat(160));
    let old = semantic_chunks_for_document(&doc, &doc.text, "source-hash");
    let new =
        semantic_chunks_for_document_with_fit(&doc, &doc.text, "source-hash", &mut |_| Ok(true))?;
    assert_eq!(old.len(), new.len());
    for (old, new) in old.iter().zip(&new) {
        assert_eq!(
            (old.start_char, old.end_char, &old.text),
            (new.start_char, new.end_char, &new.text)
        );
    }
    check_spans(&doc, &new);
    Ok(())
}

#[test]
fn dense_body_refines_a_tested_fit_and_preserves_unicode_source_coverage() -> Result<()> {
    let doc = document(&"世界🌍".repeat(1500));
    let mut calls = 0;
    let chunks =
        semantic_chunks_for_document_with_fit(&doc, &doc.text, "source-hash", &mut |input| {
            calls += 1;
            Ok(body(input).chars().count() <= 810)
        })?;
    assert_eq!(
        chunks[0].end_char, 750,
        "bounded refinement recovers capacity above600"
    );
    assert!(chunks.iter().all(|c| body(&c.text).chars().count() <= 810));
    assert!(calls <= chunks.len() * MAX_WINDOW_ASSESSMENTS);
    check_spans(&doc, &chunks);
    Ok(())
}

#[test]
fn dense_metadata_yields_to_original_body_without_changing_core_document() -> Result<()> {
    let mut doc = document(&"x".repeat(3000));
    doc.literal_facts = (0..10)
        .map(|_| ProviderDeclaredFact {
            kind: LiteralFactKind::Workspace,
            value: "世界".repeat(120),
        })
        .collect();
    let original = doc.clone();
    let header = semantic_document_header(&doc);
    assert!(header.chars().count() > 500);
    let chunks =
        semantic_chunks_for_document_with_fit(&doc, &doc.text, "source-hash", &mut |input| {
            Ok(input.chars().count() <= 512)
        })?;
    assert!(chunks.iter().all(|c| c.text.chars().count() <= 512));
    assert!(body(&chunks[0].text).chars().count() >= BODY_RESERVE_CHARS);
    assert_eq!(doc, original);
    check_spans(&doc, &chunks);
    Ok(())
}

#[test]
fn nonmonotone_fit_accepts_only_tested_candidates_and_retains_viable_reserve() -> Result<()> {
    let doc = document(&"z".repeat(3000));
    let chunks =
        semantic_chunks_for_document_with_fit(&doc, &doc.text, "source-hash", &mut |input| {
            let n = body(input).len();
            Ok(n == 256 || n <= 80)
        })?;
    assert_eq!(chunks[0].end_char, 256);
    assert!(chunks
        .iter()
        .all(|c| body(&c.text).len() == 256 || body(&c.text).len() <= 80));
    check_spans(&doc, &chunks);
    Ok(())
}

#[test]
fn empty_input_has_no_fit_calls_and_impossible_input_fails_with_a_finite_bound() -> Result<()> {
    let empty = document("");
    assert!(
        semantic_chunks_for_document_with_fit(&empty, "", "source-hash", &mut |_| panic!(
            "empty fit"
        ))?
        .is_empty()
    );
    let doc = document(&"x".repeat(1200));
    let mut calls = 0;
    let error = semantic_chunks_for_document_with_fit(&doc, &doc.text, "source-hash", &mut |_| {
        calls += 1;
        Ok(false)
    })
    .unwrap_err();
    assert_eq!(
        crate::semantic_vector_failure_kind(&error),
        Some(crate::SemanticVectorFailureKind::Unavailable)
    );
    assert!(calls <= MAX_WINDOW_ASSESSMENTS);
    Ok(())
}

#[test]
fn excessive_tiny_windows_fail_before_unbounded_chunk_amplification() {
    let doc = document(&"x".repeat(MAX_DOCUMENT_CHUNKS + 1));
    let mut calls = 0;
    let result =
        semantic_chunks_for_document_with_fit(&doc, &doc.text, "source-hash", &mut |input| {
            calls += 1;
            Ok(body(input).len() == 1)
        });
    assert!(result.is_err());
    assert!(calls <= MAX_DOCUMENT_CHUNKS * MAX_WINDOW_ASSESSMENTS);
}

#[test]
fn changing_fit_cannot_stall_at_a_previously_accepted_reserve() {
    let doc = document(&"x".repeat(1200));
    let mut calls = 0;
    let result = semantic_chunks_for_document_with_fit(&doc, &doc.text, "source-hash", &mut |_| {
        calls += 1;
        // The original window fails, its reserve fits once, then authority changes.
        Ok(calls == 2)
    });
    let error = result.unwrap_err();
    assert_eq!(
        crate::semantic_vector_failure_kind(&error),
        Some(crate::SemanticVectorFailureKind::Unavailable)
    );
    assert!(calls <= MAX_WINDOW_ASSESSMENTS);
    assert!(error.to_string().contains("fit changed"));
}
