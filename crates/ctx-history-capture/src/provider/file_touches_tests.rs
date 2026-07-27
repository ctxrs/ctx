use std::convert::Infallible;

use super::*;

fn collect_drafts(
    raw: &Value,
    limit: usize,
) -> (Vec<(u64, FileTouchDraft)>, ProviderFileTouchVisitOutcome) {
    let mut drafts = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(raw, true, limit, |draft| {
        drafts.push(draft);
        Ok::<(), Infallible>(())
    })
    .expect("an infallible draft sink cannot fail");
    (drafts, outcome)
}

#[test]
fn structured_touch_retains_only_bounded_canonical_path_key_provenance() {
    let raw_key = format!("{}path", "!".repeat(1024 * 1024));
    let mut object = serde_json::Map::new();
    object.insert(raw_key.clone(), json!("src/ignored-private-output.rs"));
    object.insert("file_path".to_owned(), json!("src/private-output.rs"));

    let (drafts, outcome) =
        collect_drafts(&Value::Object(object), MAX_PROVIDER_FILE_TOUCHES_PER_EVENT);

    assert_eq!(outcome.emitted(), 1);
    assert!(!outcome.limit_exceeded());
    assert_eq!(drafts[0].1.path, "src/private-output.rs");
    assert_eq!(drafts[0].1.metadata["path_key"], "filepath");
    let rendered = serde_json::to_string(&drafts[0].1.metadata).unwrap();
    assert!(rendered.len() < 256);
    assert!(!rendered.contains(&raw_key));
}

#[test]
fn nested_touches_stream_in_order_and_ignore_duplicates() {
    const PATH_COUNT: usize = 4;
    let paths = (0..PATH_COUNT)
        .map(|index| {
            json!({
                "tool": "write_file",
                "nested": { "path": format!("src/generated/{index}.rs") },
            })
        })
        .chain(std::iter::once(json!({
            "tool": "write_file",
            "nested": { "path": "src/generated/0.rs" },
        })))
        .collect();

    let (drafts, outcome) =
        collect_drafts(&Value::Array(paths), MAX_PROVIDER_FILE_TOUCHES_PER_EVENT);

    assert_eq!(outcome.emitted(), PATH_COUNT);
    assert!(!outcome.limit_exceeded());
    assert_eq!(drafts.first().unwrap().0, 0);
    assert_eq!(drafts.first().unwrap().1.path, "src/generated/0.rs");
    assert_eq!(drafts.last().unwrap().0, PATH_COUNT as u64 - 1);
    assert_eq!(
        drafts.last().unwrap().1.path,
        format!("src/generated/{}.rs", PATH_COUNT - 1)
    );
}

#[test]
fn tiny_touch_limit_preserves_the_first_unique_prefix() {
    const TOUCH_LIMIT: usize = 3;
    let (drafts, outcome) = collect_drafts(
        &json!([
            { "path": ".p0" },
            { "path": ".p0" },
            { "path": ".p1" },
            { "path": ".p2" },
            { "path": ".p3" },
            { "path": ".unvisited" },
        ]),
        TOUCH_LIMIT,
    );

    assert!(outcome.limit_exceeded());
    assert_eq!(outcome.emitted(), TOUCH_LIMIT);
    assert_eq!(
        drafts
            .into_iter()
            .map(|(ordinal, draft)| (ordinal, draft.path))
            .collect::<Vec<_>>(),
        vec![
            (0, ".p0".to_owned()),
            (1, ".p1".to_owned()),
            (2, ".p2".to_owned()),
        ]
    );
}

#[test]
fn production_limit_matches_the_sixteen_bit_identity_boundary() {
    let touch_space = u64::try_from(MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap();
    let event_index = 17_u64;
    let event_base = event_index << 16;
    let final_touch = event_base | (touch_space - 1);
    let next_event_base = (event_index + 1) << 16;

    assert_eq!(MAX_PROVIDER_FILE_TOUCHES_PER_EVENT, 1_usize << 16);
    assert_eq!(
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        usize::from(u16::MAX) + 1
    );
    assert_eq!(MAX_PACKED_PROVIDER_EVENT_INDEX, u64::MAX >> 16);
    assert_eq!(final_touch + 1, next_event_base);
    assert_eq!(MAX_PACKED_PROVIDER_EVENT_INDEX << 16, !0xffff_u64);
}
