use super::*;
use tantivy::schema::{Schema, TantivyDocument, STRING};

const TYPES: [&str; 7] = [
    "message",
    "summary",
    "tool_call",
    "command_started",
    "tool_output",
    "command_output",
    "command_finished",
];
const CLASSES: [usize; 7] = [0, 1, 2, 2, 3, 3, 3];

fn segment(rows: &[Vec<&str>], deleted: &[usize]) -> (SegmentReader, Field) {
    let mut schema = Schema::builder();
    let field = schema.add_text_field("event_type", STRING);
    let identity = schema.add_text_field("identity", STRING);
    let index = tantivy::Index::create_in_ram(schema.build());
    let mut writer = index.writer_with_num_threads(1, 15_000_000).unwrap();
    for (doc, types) in rows.iter().enumerate() {
        let mut row = TantivyDocument::default();
        row.add_text(identity, doc.to_string());
        for name in types {
            row.add_text(field, name);
        }
        writer.add_document(row).unwrap();
    }
    writer.commit().unwrap();
    for doc in deleted {
        writer.delete_term(Term::from_field_text(identity, &doc.to_string()));
    }
    writer.commit().unwrap();
    let reader = index.reader().unwrap();
    let searcher = reader.searcher();
    assert_eq!(searcher.segment_readers().len(), 1);
    (searcher.segment_reader(0).clone(), field)
}

fn context(reader: &SegmentReader) -> LexicalSegmentContext {
    LexicalSegmentContext {
        stable_segment_index: 0,
        segment_id: reader.segment_id().uuid_string(),
        segment_ord: 0,
    }
}

#[test]
fn homogeneous_segments_classify_without_posting_work() {
    for (name, class) in TYPES.into_iter().zip(CLASSES) {
        let (reader, field) = segment(&vec![vec![name]; 257], &[17]);
        let context = context(&reader);
        for scope in [
            SearchContentScope::All,
            SearchContentScope::Transcript,
            SearchContentScope::Calls,
            SearchContentScope::Outputs,
        ] {
            let expected = match scope {
                SearchContentScope::All => class,
                SearchContentScope::Transcript => usize::from(name != "message"),
                _ => 0,
            };
            // An exact segment class requires dictionary work, but no posting
            // opens, membership probes, block inspections or cursor movements.
            let mut meter = LexicalWorkMeter::new(LexicalWorkBudget {
                maximum_posting_opens: 0,
                maximum_body_posting_advances: 0,
                maximum_filter_probes: 0,
                maximum_filter_seeks: 0,
                ..LEXICAL_WORK_BUDGET_V1
            });
            let mut classes = SegmentClasses::open(&reader, field, scope, &mut meter, &context)
                .unwrap()
                .expect("segment-wide class needs no posting budget");
            let docs: Vec<_> = (129..257).collect();
            classes.prepare_bounds(129, &mut meter, &context).unwrap();
            assert_eq!(
                classes
                    .block_classes(&docs, &reader, &mut meter, &context)
                    .unwrap(),
                [expected; 128]
            );
            assert_eq!(
                classes.weight(256, &mut meter, &context),
                Some(WEIGHTS[expected])
            );
            assert!(!meter.exhausted());
        }
    }
}

#[test]
fn segment_wide_class_preserves_earlier_class_precedence() {
    for fallback in ["summary", "tool_output"] {
        let rows: Vec<_> = (0..513)
            .map(|doc| {
                let mut types = vec![fallback];
                if doc % 3 == 0 {
                    types.push("message");
                }
                if doc % 5 == 0 {
                    types.push("summary");
                }
                types
            })
            .collect();
        let (reader, field) = segment(&rows, &[16]);
        let context = context(&reader);
        let mut meter = LexicalWorkMeter::new(LEXICAL_WORK_BUDGET_V1);
        let mut classes = SegmentClasses::open(
            &reader,
            field,
            SearchContentScope::All,
            &mut meter,
            &context,
        )
        .unwrap()
        .unwrap();
        for start in [0, 127, 128, 129, 385] {
            let docs: Vec<_> = (start..start + 128).collect();
            classes.prepare_bounds(start, &mut meter, &context).unwrap();
            let actual = classes
                .block_classes(&docs, &reader, &mut meter, &context)
                .unwrap();
            for (&doc, class) in docs.iter().zip(actual) {
                if reader.is_deleted(doc) {
                    continue;
                }
                let expected = if doc % 3 == 0 {
                    0
                } else if fallback == "summary" || doc % 5 == 0 {
                    1
                } else {
                    3
                };
                assert_eq!(class, expected);
            }
            // Lookahead must leave the shared scoring cursor at the first doc.
            assert_eq!(
                classes.weight(start, &mut meter, &context),
                Some(WEIGHTS[actual[0]])
            );
        }
    }
}

#[test]
fn live_doc_count_does_not_prove_a_homogeneous_segment() {
    let mut rows = vec![vec!["message"]; 257];
    rows[256] = vec!["summary"];
    let (reader, field) = segment(&rows, &[0]);
    assert_eq!(reader.num_docs(), 256);
    assert_eq!(reader.max_doc(), 257);
    let context = context(&reader);
    let mut meter = LexicalWorkMeter::new(LEXICAL_WORK_BUDGET_V1);
    let mut classes = SegmentClasses::open(
        &reader,
        field,
        SearchContentScope::All,
        &mut meter,
        &context,
    )
    .unwrap()
    .unwrap();
    classes.prepare_bounds(256, &mut meter, &context).unwrap();
    assert_eq!(
        classes
            .block_classes(&[256], &reader, &mut meter, &context)
            .unwrap()[0],
        1
    );
    assert_eq!(classes.weight(256, &mut meter, &context), Some(0.9));
}

#[test]
fn mixed_memberships_merge_across_blocks_gaps_and_tails() {
    let rows: Vec<_> = (0..4099)
        .map(|doc| vec![TYPES[doc % TYPES.len()]])
        .collect();
    let deleted: Vec<_> = (0..rows.len()).step_by(31).collect();
    let (reader, field) = segment(&rows, &deleted);
    let context = context(&reader);
    for stride in [1, 3, 17] {
        let mut meter = LexicalWorkMeter::new(LEXICAL_WORK_BUDGET_V1);
        let mut classes = SegmentClasses::open(
            &reader,
            field,
            SearchContentScope::All,
            &mut meter,
            &context,
        )
        .unwrap()
        .unwrap();
        let docs: Vec<_> = (0..reader.max_doc()).step_by(stride).collect();
        for block in docs.chunks(128) {
            classes
                .prepare_bounds(block[0], &mut meter, &context)
                .unwrap();
            for suffix in [block, &block[block.len() / 2..]] {
                let actual = classes
                    .block_classes(suffix, &reader, &mut meter, &context)
                    .unwrap();
                for (&doc, class) in suffix.iter().zip(actual) {
                    if !reader.is_deleted(doc) {
                        assert_eq!(class, CLASSES[doc as usize % TYPES.len()]);
                    }
                }
            }
        }
        let (counters, exhaustion) = meter.into_parts();
        assert!(exhaustion.is_none());
        assert!(counters.body_posting_advances > 0 && counters.filter_seeks > 0);
    }
}
