use std::fs::OpenOptions;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

use super::*;

#[test]
fn opaque_repository_scope_uses_a_fixed_digest_key() -> Result<(), FlatSegmentError> {
    let family = FactFamily::new("git.commit.produced")?;
    let exact = "r".repeat(MAX_REPOSITORY_ID_BYTES);
    let other = format!("{}s", "r".repeat(MAX_REPOSITORY_ID_BYTES - 1));
    let exact_key = index_key(&exact, &family, "term")?;
    let other_key = index_key(&other, &family, "term")?;
    assert!(exact_key.len() <= MAX_FST_KEY_BYTES);
    assert_ne!(exact_key, other_key);
    assert!(index_key(&format!("{exact}x"), &family, "term").is_err());
    Ok(())
}

use super::super::model::{
    AttributeValue, EventOwner, EvidenceRelationship, FILE_MENTIONED, FILE_TOUCHED,
    GIT_COMMIT_PRODUCED, LineRange, MAX_ATTRIBUTES_PER_RECORD, ObservationOrigin,
    SCHEMA_CRITICAL_FACT_FAMILIES, ServingCitation, ServingConfidence, ServingFactState,
    ServingResource,
};

const TEST_GENERATION: [u8; 32] = [0x71; 32];
const TEST_CHUNK_BYTES: u32 = 16 * 1024;

#[test]
fn exact_and_prefix_queries_are_repository_and_family_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let records = vec![
        record("a-z", "event-a-z", 1, "repo-a", FILE_TOUCHED, &["src/z.rs"])?,
        record("a-a", "event-a-a", 2, "repo-a", FILE_TOUCHED, &["src/a.rs"])?,
        record("b-z", "event-b-z", 3, "repo-b", FILE_TOUCHED, &["src/z.rs"])?,
        record(
            "mentioned-z",
            "event-mentioned-z",
            4,
            "repo-a",
            FILE_MENTIONED,
            &["src/z.rs"],
        )?,
    ];
    let (mut reader, stats) = write_and_open(&directory.path().join("segment"), records, vec![])?;
    assert_eq!(stats.record_count, 4);
    assert_eq!(stats.index_key_count, 7);
    assert_eq!(stats.index_association_count, 8);
    assert!(stats.plaintext_bytes > stats.fst_bytes);

    let touched = FactFamily::new(FILE_TOUCHED)?;
    let exact = reader.query_exact("repo-a", &touched, "src/z.rs", 8)?;
    assert_eq!(record_ids(&exact), vec!["a-z"]);

    let prefix = reader.query_prefix("repo-a", &touched, "src/", 8)?;
    // Prefix FST traversal is lexical, but answers retain canonical
    // observation chronology rather than term ordering.
    assert_eq!(record_ids(&prefix), vec!["a-z", "a-a"]);
    assert!(
        reader
            .query_exact("repo-b", &touched, "src/a.rs", 8)?
            .is_empty()
    );
    assert!(
        reader
            .query_exact("repo-a", &FactFamily::new(FILE_MENTIONED)?, "src/a.rs", 8,)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn unscoped_exact_and_prefix_queries_return_distinct_repositories_and_ambiguity()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repo_a = record(
        "repo-a-observation",
        "repo-a-event",
        1,
        "repo-a",
        FILE_TOUCHED,
        &["src/lib.rs"],
    )?;
    let duplicate_repo_a_observation = repo_a.clone();
    let repo_b = record(
        "repo-b-observation",
        "repo-b-event",
        2,
        "repo-b",
        FILE_TOUCHED,
        &["src/lib.rs"],
    )?;
    let (mut reader, _) = write_and_open(
        &directory.path().join("segment"),
        vec![repo_b, duplicate_repo_a_observation, repo_a],
        vec![],
    )?;
    let family = FactFamily::new(FILE_TOUCHED)?;

    let exact = reader.query_exact_unscoped(&family, "src/lib.rs", 8)?;
    assert!(exact.repository_ambiguous);
    assert_eq!(
        record_ids(&exact.records),
        vec![
            "repo-a-observation",
            "repo-a-observation",
            "repo-b-observation",
        ]
    );
    assert_eq!(exact.records[0].repository_id, "repo-a");
    assert_eq!(exact.records[1].repository_id, "repo-a");
    assert_eq!(exact.records[2].repository_id, "repo-b");

    let prefix = reader.query_prefix_unscoped(&family, "src/", 8)?;
    assert!(prefix.repository_ambiguous);
    assert_eq!(record_ids(&prefix.records), record_ids(&exact.records));

    let bounded = reader.query_exact_unscoped(&family, "src/lib.rs", 1)?;
    assert_eq!(bounded.records.len(), 1);
    assert!(bounded.repository_ambiguous);

    let scoped = reader.query_exact("repo-a", &family, "src/lib.rs", 8)?;
    assert_eq!(
        record_ids(&scoped),
        vec!["repo-a-observation", "repo-a-observation"]
    );
    Ok(())
}

#[test]
fn physical_continuations_cover_256_and_257_rows_without_prefix_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let family = FactFamily::new(FILE_TOUCHED)?;
    let make_records = |count: usize| -> Result<Vec<_>, ServingModelError> {
        (0..count)
            .map(|index| {
                record(
                    "one-logical-fact",
                    &format!("event-{index:04}"),
                    u64::try_from(index).unwrap_or(u64::MAX),
                    "repo",
                    FILE_TOUCHED,
                    &["needle", "needle-extra"],
                )
            })
            .collect()
    };

    let (mut exactly_256, _) = write_and_open(
        &directory.path().join("exactly-256"),
        make_records(MAX_QUERY_RESULTS)?,
        vec![],
    )?;
    let page = exactly_256.query_exact_page("repo", &family, "needle", None, MAX_QUERY_RESULTS)?;
    assert_eq!(page.records.len(), MAX_QUERY_RESULTS);
    assert!(page.continuation.is_none());
    assert!(matches!(
        exactly_256.query_exact_page("repo", &family, "needle", None, 0),
        Err(FlatSegmentError::Bound("query result count"))
    ));

    let (mut reader, _) = write_and_open(
        &directory.path().join("exactly-257"),
        make_records(MAX_QUERY_RESULTS + 1)?,
        vec![],
    )?;
    let first = reader.query_exact_page("repo", &family, "needle", None, MAX_QUERY_RESULTS)?;
    assert_eq!(first.records.len(), MAX_QUERY_RESULTS);
    let second = reader.query_exact_page(
        "repo",
        &family,
        "needle",
        first.continuation.as_ref(),
        MAX_QUERY_RESULTS,
    )?;
    assert_eq!(second.records.len(), 1);
    assert!(second.continuation.is_none());

    let mut continuation = None;
    let mut prefix_event_ids = Vec::new();
    loop {
        let page =
            reader.query_prefix_page("repo", &family, "needle", continuation.as_ref(), 128)?;
        prefix_event_ids.extend(
            page.records
                .iter()
                .map(|record| record.event_owner.event_id.clone()),
        );
        continuation = page.continuation;
        if continuation.is_none() {
            break;
        }
    }
    assert_eq!(prefix_event_ids.len(), MAX_QUERY_RESULTS + 1);
    prefix_event_ids.sort();
    prefix_event_ids.dedup();
    assert_eq!(prefix_event_ids.len(), MAX_QUERY_RESULTS + 1);

    let mut continuation = None;
    let mut unscoped_exact_count = 0;
    loop {
        let page = reader.query_exact_unscoped_page(
            &family,
            "needle",
            continuation.as_ref(),
            MAX_QUERY_RESULTS,
        )?;
        unscoped_exact_count += page.records.len();
        assert!(!page.repository_ambiguous);
        continuation = page.continuation;
        if continuation.is_none() {
            break;
        }
    }
    assert_eq!(unscoped_exact_count, MAX_QUERY_RESULTS + 1);

    let mut continuation = None;
    let mut unscoped_prefix_count = 0;
    loop {
        let page = reader.query_prefix_unscoped_page(
            &family,
            "needle",
            continuation.as_ref(),
            MAX_QUERY_RESULTS,
        )?;
        unscoped_prefix_count += page.records.len();
        continuation = page.continuation;
        if continuation.is_none() {
            break;
        }
    }
    assert_eq!(unscoped_prefix_count, MAX_QUERY_RESULTS + 1);
    Ok(())
}

#[test]
fn v2_exact_prefix_and_continuation_cross_fst_shard_fences()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let records = (0..=MAX_FST_KEYS_PER_SHARD)
        .map(|index| {
            let term = format!("cross-{index:04}");
            record(
                &format!("record-{index:04}"),
                &format!("event-{index:04}"),
                u64::try_from(index).unwrap_or(u64::MAX),
                "repo",
                FILE_TOUCHED,
                &[term.as_str()],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (mut reader, stats) =
        write_and_open(&directory.path().join("shard-boundary"), records, vec![])?;
    assert!(stats.fst_shard_count >= 3);
    assert!(stats.fst_bytes > 0);
    assert!(stats.directory_bytes < stats.fst_bytes);
    let opened = reader.observability();
    assert_eq!(
        opened.bytes_read,
        u64::try_from(HEADER_BYTES).unwrap_or(u64::MAX) + stats.directory_bytes
    );
    let family = FactFamily::new(FILE_TOUCHED)?;

    for index in [MAX_FST_KEYS_PER_SHARD - 1, MAX_FST_KEYS_PER_SHARD] {
        let term = format!("cross-{index:04}");
        let exact = reader.query_exact("repo", &family, &term, 2)?;
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].record_id, format!("record-{index:04}"));
    }

    let mut continuation = None;
    let mut prior_shard = None;
    let mut crossed_shard = false;
    let mut event_ids = Vec::new();
    loop {
        let page = reader.query_prefix_page(
            "repo",
            &family,
            "cross-",
            continuation.as_ref(),
            MAX_QUERY_RESULTS,
        )?;
        event_ids.extend(
            page.records
                .iter()
                .map(|record| record.event_owner.event_id.clone()),
        );
        if let Some(next) = page.continuation.as_ref() {
            if prior_shard.is_some_and(|prior| prior != next.shard_index) {
                crossed_shard = true;
            }
            prior_shard = Some(next.shard_index);
        }
        continuation = page.continuation;
        if continuation.is_none() {
            break;
        }
    }
    assert!(crossed_shard);
    assert_eq!(event_ids.len(), MAX_FST_KEYS_PER_SHARD + 1);
    let mut unique = event_ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), event_ids.len());
    assert!(
        reader
            .query_prefix("repo", &family, "cross-z", 2)?
            .is_empty()
    );

    let mut continuation = None;
    let mut unscoped_count = 0_usize;
    loop {
        let page = reader.query_prefix_unscoped_page(
            &family,
            "cross-",
            continuation.as_ref(),
            MAX_QUERY_RESULTS,
        )?;
        unscoped_count += page.records.len();
        continuation = page.continuation;
        if continuation.is_none() {
            break;
        }
    }
    assert_eq!(unscoped_count, MAX_FST_KEYS_PER_SHARD + 1);

    let observed = reader.observability();
    assert!(observed.flat_cache_entries <= 1);
    assert!(observed.flat_cache_high_water_bytes <= MAX_FST_SHARD_BYTES);
    assert!(observed.block_cache_bytes <= crate::MAX_BLOCK_CACHE_BYTES);
    assert!(observed.block_cache_entries <= crate::MAX_BLOCK_CACHE_ENTRIES);
    Ok(())
}

#[test]
fn v2_tombstone_membership_is_exact_and_page_bounded() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let tombstones = (0..2_500)
        .map(|index| EventTombstone {
            source_id: format!("source-{index:04}"),
            event_id: format!("event-{index:04}-{}", "x".repeat(48)),
            event_sequence: u64::try_from(index).unwrap_or(u64::MAX),
        })
        .collect::<Vec<_>>();
    let (mut reader, stats) = write_and_open(
        &directory.path().join("tombstone-pages"),
        Vec::new(),
        tombstones.clone(),
    )?;
    assert!(stats.tombstone_page_count >= 2);
    let before = reader.observability();
    let requested = vec![
        tombstones[2_499].key(),
        EventOwnerKey {
            source_id: "source-1249".to_owned(),
            event_id: "event-absent".to_owned(),
        },
        tombstones[0].key(),
        EventOwnerKey {
            source_id: "source-9999".to_owned(),
            event_id: "event-9999".to_owned(),
        },
        tombstones[1_249].key(),
    ];
    let planned = reader.tombstone_query_work(&requested)?;
    assert!((1..=3).contains(&planned.pages));
    assert!(planned.page_bytes <= 3 * MAX_TOMBSTONE_PAGE_BYTES as u64);
    assert!(planned.checked_chunks > 0);
    assert!(planned.checked_bytes >= planned.page_bytes);
    assert_eq!(
        reader.tombstone_membership(&requested)?,
        vec![true, false, true, false, true]
    );
    let after = reader.observability();
    assert_eq!(after.bytes_read - before.bytes_read, planned.page_bytes);
    assert!(after.flat_cache_entries <= 1);
    assert!(after.flat_cache_high_water_bytes <= MAX_TOMBSTONE_PAGE_BYTES);
    assert!(after.block_cache_bytes <= crate::MAX_BLOCK_CACHE_BYTES);
    assert!(after.block_cache_entries <= crate::MAX_BLOCK_CACHE_ENTRIES);
    Ok(())
}

#[test]
fn canonical_order_preserves_duplicate_observations_and_citations()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let mut direct_late = record(
        "direct-late",
        "event-direct-late",
        30,
        "repo",
        GIT_COMMIT_PRODUCED,
        &["abc123"],
    )?;
    direct_late.occurred_at_unix_ms = Some(300);
    let duplicate_citation = citation("citation-duplicate", "event-direct-late", 30);
    direct_late.citations = vec![duplicate_citation.clone(), duplicate_citation];

    let mut direct_early = record(
        "direct-early",
        "event-direct-early",
        20,
        "repo",
        GIT_COMMIT_PRODUCED,
        &["abc123"],
    )?;
    direct_early.occurred_at_unix_ms = Some(200);
    let duplicate_observation = direct_early.clone();

    let mut copied = record(
        "copied",
        "event-copied",
        10,
        "repo",
        GIT_COMMIT_PRODUCED,
        &["abc123"],
    )?;
    copied.occurred_at_unix_ms = Some(100);
    copied.origin = ObservationOrigin::Copied {
        origin_event_id: "event-original".to_owned(),
        origin_session_id: "session-original".to_owned(),
    };
    copied.state = ServingFactState::Ambiguous;

    let (mut reader, _) = write_and_open(
        &directory.path().join("segment"),
        vec![copied, direct_late, duplicate_observation, direct_early],
        vec![],
    )?;
    let results =
        reader.query_exact("repo", &FactFamily::new(GIT_COMMIT_PRODUCED)?, "abc123", 8)?;
    assert_eq!(
        record_ids(&results),
        vec!["direct-early", "direct-early", "direct-late", "copied"]
    );
    assert_eq!(results[2].citations.len(), 2);
    assert_eq!(results[2].citations[0], results[2].citations[1]);
    Ok(())
}

#[test]
fn canonical_bytes_do_not_depend_on_input_order() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let first_path = directory.path().join("first");
    let second_path = directory.path().join("second");
    let first = record("first", "event-first", 1, "repo", FILE_TOUCHED, &["a"])?;
    let second = record("second", "event-second", 2, "repo", FILE_TOUCHED, &["b"])?;
    let early_tombstone = EventTombstone {
        source_id: "source".to_owned(),
        event_id: "deleted-a".to_owned(),
        event_sequence: 4,
    };
    let late_tombstone = EventTombstone {
        source_id: "source".to_owned(),
        event_id: "deleted-b".to_owned(),
        event_sequence: 5,
    };
    write_segment(
        &first_path,
        vec![second.clone(), first.clone()],
        vec![late_tombstone.clone(), early_tombstone.clone()],
    )?;
    write_segment(
        &second_path,
        vec![first, second],
        vec![early_tombstone, late_tombstone],
    )?;
    assert_eq!(std::fs::read(first_path)?, std::fs::read(second_path)?);
    Ok(())
}

#[test]
fn event_tombstones_shadow_older_segments_without_hiding_same_generation_replacements()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let replacement = record(
        "replacement",
        "event-replaced",
        1,
        "repo",
        FILE_TOUCHED,
        &["src/lib.rs"],
    )?;
    let retained = record(
        "retained",
        "event-retained",
        2,
        "repo",
        FILE_TOUCHED,
        &["src/lib.rs"],
    )?;
    let tombstone = EventTombstone {
        source_id: replacement.event_owner.source_id.clone(),
        event_id: replacement.event_owner.event_id.clone(),
        event_sequence: 3,
    };
    let (mut reader, _) = write_and_open(
        &directory.path().join("segment"),
        vec![replacement.clone(), retained],
        vec![tombstone.clone()],
    )?;
    assert!(reader.contains_tombstone(&tombstone.key())?);
    let results = reader.query_exact("repo", &FactFamily::new(FILE_TOUCHED)?, "src/lib.rs", 8)?;
    assert_eq!(record_ids(&results), vec!["replacement", "retained"]);
    assert!(reader.contains_tombstone(&replacement.event_owner.key())?);
    Ok(())
}

#[test]
fn schema_critical_families_and_typed_payloads_round_trip() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let mut records = Vec::new();
    for (sequence, family) in SCHEMA_CRITICAL_FACT_FAMILIES.into_iter().enumerate() {
        let mut item = record(
            family,
            &format!("event-{sequence}"),
            u64::try_from(sequence)?,
            "repo",
            family,
            &["lookup"],
        )?;
        item.attributes.insert(
            "ordered_parents".to_owned(),
            AttributeValue::Strings(vec!["parent-2".to_owned(), "parent-1".to_owned()]),
        );
        item.attributes.insert(
            "changed_files".to_owned(),
            AttributeValue::Strings(vec!["z.rs".to_owned(), "a.rs".to_owned()]),
        );
        records.push(item);
    }
    let (mut reader, _) = write_and_open(&directory.path().join("segment"), records, vec![])?;
    for family in SCHEMA_CRITICAL_FACT_FAMILIES {
        let results = reader.query_exact("repo", &FactFamily::new(family)?, "lookup", 2)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fact_family.as_str(), family);
        if family == FILE_TOUCHED {
            assert_eq!(
                results[0].line_range,
                Some(LineRange {
                    start_line: 10,
                    end_line_inclusive: 20,
                })
            );
        } else {
            assert_eq!(results[0].line_range, None);
        }
        assert_eq!(
            results[0].attributes.get("ordered_parents"),
            Some(&AttributeValue::Strings(vec![
                "parent-2".to_owned(),
                "parent-1".to_owned(),
            ]))
        );
    }
    Ok(())
}

#[test]
fn malformed_checked_segments_tampering_and_bounds_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;

    let short_path = directory.path().join("short");
    let mut short_writer = segment_writer(&short_path)?;
    short_writer.write_all(b"not-a-flat-header")?;
    short_writer.finish()?;
    let short_file = segment_file(&short_path)?;
    assert!(matches!(
        FlatSegmentReader::open(short_file),
        Err(FlatSegmentError::Corrupt("truncated header"))
    ));

    let valid_path = directory.path().join("valid");
    write_segment(
        &valid_path,
        vec![record(
            "record",
            "event",
            1,
            "repo",
            FILE_TOUCHED,
            &["term"],
        )?],
        vec![],
    )?;
    assert!(matches!(
        FlatSegmentReader::open_bounded(segment_file(&valid_path)?, 0),
        Err(FlatSegmentError::Bound("graph Flat directory bytes"))
    ));
    let mut valid_reader = FlatSegmentReader::open(segment_file(&valid_path)?)?;
    let family = FactFamily::new(FILE_TOUCHED)?;
    assert!(matches!(
        valid_reader.query_exact("repo", &family, "term", 0),
        Err(FlatSegmentError::Bound("query result count"))
    ));
    assert!(matches!(
        valid_reader.query_prefix("repo", &family, "", MAX_QUERY_RESULTS + 1),
        Err(FlatSegmentError::Bound("query result count"))
    ));
    assert!(matches!(
        valid_reader.query_exact("repo", &family, &"x".repeat(MAX_INDEX_TERM_BYTES + 1), 1,),
        Err(FlatSegmentError::Bound("query term bytes"))
    ));

    let tampered_path = directory.path().join("tampered");
    std::fs::copy(&valid_path, &tampered_path)?;
    let mut tampered = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&tampered_path)?;
    let last_offset = tampered
        .metadata()?
        .len()
        .checked_sub(1)
        .ok_or("empty file")?;
    tampered.seek(SeekFrom::Start(last_offset))?;
    let mut byte = [0_u8; 1];
    tampered.read_exact(&mut byte)?;
    byte[0] ^= 0x80;
    tampered.seek(SeekFrom::Start(last_offset))?;
    tampered.write_all(&byte)?;
    drop(tampered);
    let tampered_file = segment_file(&tampered_path)?;
    assert!(matches!(
        FlatSegmentReader::open(tampered_file),
        Err(FlatSegmentError::File(SegmentFileError::Corrupt(
            "block checksum"
        )))
    ));

    let bad_posting_path = directory.path().join("bad-posting");
    write_bad_posting_segment(&bad_posting_path)?;
    let mut bad_posting_reader = FlatSegmentReader::open(segment_file(&bad_posting_path)?)?;
    assert!(matches!(
        bad_posting_reader.query_exact("repo", &family, "term", 1),
        Err(FlatSegmentError::Corrupt("posting offset"))
    ));
    Ok(())
}

#[test]
fn selected_and_unselected_fst_shard_corruption_is_lazy() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let valid_path = directory.path().join("fst-valid");
    let records = (0..4_097)
        .map(|index| {
            let term = pseudo_random_term(index);
            record(
                &format!("record-{index:04}"),
                &format!("event-{index:04}"),
                u64::try_from(index).unwrap_or(u64::MAX),
                "repo",
                FILE_TOUCHED,
                &[term.as_str()],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stats = write_segment(&valid_path, records, Vec::new())?;
    assert!(stats.fst_shard_count >= 8);
    let reader = FlatSegmentReader::open(segment_file(&valid_path)?)?;
    let family = FactFamily::new(FILE_TOUCHED)?;
    let scoped_prefix = index_key_prefix("repo", &family, "")?;
    let directory_first_chunk = reader.layout.directory_start / u64::from(TEST_CHUNK_BYTES);
    let directory_last_chunk = reader
        .layout
        .directory_start
        .checked_add(reader.layout.directory_bytes - 1)
        .ok_or("directory range")?
        / u64::from(TEST_CHUNK_BYTES);
    let candidates = reader
        .directory
        .fst_shards
        .iter()
        .filter(|descriptor| descriptor.first_key.starts_with(&scoped_prefix))
        .filter_map(|descriptor| {
            let plaintext_offset = reader
                .layout
                .fst_shards_start
                .checked_add(descriptor.offset)?;
            let start_chunk = plaintext_offset / u64::from(TEST_CHUNK_BYTES);
            let last_offset = plaintext_offset.checked_add(u64::from(descriptor.length) - 1)?;
            let end_chunk = last_offset / u64::from(TEST_CHUNK_BYTES);
            (!(directory_first_chunk..=directory_last_chunk).contains(&start_chunk)
                && !(directory_first_chunk..=directory_last_chunk).contains(&end_chunk))
            .then_some((descriptor.clone(), plaintext_offset, start_chunk, end_chunk))
        })
        .collect::<Vec<_>>();
    let selected = candidates.first().ok_or("selected FST shard")?;
    let unselected = candidates
        .iter()
        .find(|candidate| candidate.2 > selected.3)
        .ok_or("unselected FST shard")?;
    let selected_term = std::str::from_utf8(
        selected
            .0
            .first_key
            .get(scoped_prefix.len()..)
            .ok_or("selected FST fence")?,
    )?
    .to_owned();
    drop(reader);

    let selected_path = directory.path().join("fst-selected-corrupt");
    std::fs::copy(&valid_path, &selected_path)?;
    flip_block_byte(&selected_path, selected.1)?;
    let mut selected_reader = FlatSegmentReader::open(segment_file(&selected_path)?)?;
    assert!(
        selected_reader
            .query_exact("repo", &family, &selected_term, 1)
            .is_err()
    );

    let unselected_path = directory.path().join("fst-unselected-corrupt");
    std::fs::copy(&valid_path, &unselected_path)?;
    flip_block_byte(&unselected_path, unselected.1)?;
    let mut unselected_reader = FlatSegmentReader::open(segment_file(&unselected_path)?)?;
    assert_eq!(
        unselected_reader
            .query_exact("repo", &family, &selected_term, 1)?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn selected_and_unselected_tombstone_page_corruption_is_lazy()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let valid_path = directory.path().join("tombstone-valid");
    let tombstones = (0..4_000)
        .map(|index| EventTombstone {
            source_id: format!("source-{index:04}"),
            event_id: format!("event-{index:04}-{}", "x".repeat(96)),
            event_sequence: u64::try_from(index).unwrap_or(u64::MAX),
        })
        .collect::<Vec<_>>();
    let stats = write_segment(&valid_path, Vec::new(), tombstones)?;
    assert!(stats.tombstone_page_count >= 4);
    let reader = FlatSegmentReader::open(segment_file(&valid_path)?)?;
    let directory_first_chunk = reader.layout.directory_start / u64::from(TEST_CHUNK_BYTES);
    let directory_last_chunk = reader
        .layout
        .directory_start
        .checked_add(reader.layout.directory_bytes - 1)
        .ok_or("directory range")?
        / u64::from(TEST_CHUNK_BYTES);
    let candidates = reader
        .directory
        .tombstone_pages
        .iter()
        .filter_map(|descriptor| {
            let plaintext_offset = reader
                .layout
                .tombstone_pages_start
                .checked_add(descriptor.offset)?;
            let start_chunk = plaintext_offset / u64::from(TEST_CHUNK_BYTES);
            let last_offset = plaintext_offset.checked_add(u64::from(descriptor.length) - 1)?;
            let end_chunk = last_offset / u64::from(TEST_CHUNK_BYTES);
            (start_chunk != 0
                && !(directory_first_chunk..=directory_last_chunk).contains(&start_chunk)
                && !(directory_first_chunk..=directory_last_chunk).contains(&end_chunk))
            .then_some((descriptor.clone(), plaintext_offset, start_chunk, end_chunk))
        })
        .collect::<Vec<_>>();
    let selected = candidates.first().ok_or("selected tombstone page")?;
    let unselected = candidates
        .iter()
        .find(|candidate| candidate.2 > selected.3)
        .ok_or("unselected tombstone page")?;
    let selected_key = selected.0.first_key.clone();
    drop(reader);

    let selected_path = directory.path().join("tombstone-selected-corrupt");
    std::fs::copy(&valid_path, &selected_path)?;
    flip_block_byte(&selected_path, selected.1)?;
    let mut selected_reader = FlatSegmentReader::open(segment_file(&selected_path)?)?;
    assert!(selected_reader.contains_tombstone(&selected_key).is_err());

    let unselected_path = directory.path().join("tombstone-unselected-corrupt");
    std::fs::copy(&valid_path, &unselected_path)?;
    flip_block_byte(&unselected_path, unselected.1)?;
    let mut unselected_reader = FlatSegmentReader::open(segment_file(&unselected_path)?)?;
    assert!(unselected_reader.contains_tombstone(&selected_key)?);
    Ok(())
}

#[test]
fn canonical_record_payload_accepts_exact_limit_and_rejects_one_over()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let mut exact = record_with_frame_bytes(MAX_FLAT_RECORD_FRAME_BYTES)?;
    assert_eq!(
        FlatSegmentWriter::record_work(&exact)?.frame_bytes,
        MAX_FLAT_RECORD_FRAME_BYTES
    );
    write_segment(
        &directory.path().join("exact"),
        vec![exact.clone()],
        Vec::new(),
    )?;

    let AttributeValue::Strings(padding) = exact
        .attributes
        .get_mut("zz-padding")
        .ok_or("missing exact-size padding")?
    else {
        return Err("unexpected padding type".into());
    };
    let final_padding = padding.last_mut().ok_or("missing final padding value")?;
    assert!(final_padding.len() < MAX_IDENTIFIER_BYTES);
    final_padding.push('x');
    assert!(matches!(
        FlatSegmentWriter::record_work(&exact),
        Err(FlatSegmentError::Bound("record bytes"))
    ));
    assert!(matches!(
        write_segment(&directory.path().join("one-over"), vec![exact], Vec::new(),),
        Err(FlatSegmentError::Bound("record bytes"))
    ));
    Ok(())
}

pub fn record(
    record_id: &str,
    event_id: &str,
    event_sequence: u64,
    repository_id: &str,
    fact_family: &str,
    index_terms: &[&str],
) -> Result<ServingRecord, ServingModelError> {
    let subject_kind = if fact_family.contains("commit") {
        "commit"
    } else if fact_family.contains("pull_request") {
        "pull_request"
    } else if fact_family.contains("remote") {
        "remote"
    } else if fact_family.contains("repository") {
        "repository"
    } else {
        "file"
    };
    let record = ServingRecord {
        record_id: record_id.to_owned(),
        event_owner: EventOwner {
            source_id: "source".to_owned(),
            event_id: event_id.to_owned(),
            direct_session_id: format!("session-{event_sequence}"),
            root_session_id: Some("root-session".to_owned()),
            event_sequence,
        },
        repository_id: repository_id.to_owned(),
        fact_family: FactFamily::new(fact_family)?,
        subject: ServingResource {
            kind: subject_kind.to_owned(),
            id: index_terms.first().copied().unwrap_or("subject").to_owned(),
            repository_id: Some(repository_id.to_owned()),
            worktree_id: Some("worktree".to_owned()),
        },
        object: None,
        scope: None,
        direct_actor: None,
        occurred_at_unix_ms: None,
        confidence: ServingConfidence::Verified,
        state: ServingFactState::Asserted,
        detector_id: "test.detector".to_owned(),
        detector_revision: "1".to_owned(),
        origin: ObservationOrigin::Direct,
        line_range: if fact_family == FILE_TOUCHED {
            Some(LineRange {
                start_line: 10,
                end_line_inclusive: 20,
            })
        } else {
            None
        },
        index_terms: index_terms.iter().map(|term| (*term).to_owned()).collect(),
        attributes: BTreeMap::new(),
        citations: vec![citation("citation", event_id, event_sequence)],
    };
    record.validate()?;
    Ok(record)
}

fn record_with_frame_bytes(target: usize) -> Result<ServingRecord, Box<dyn std::error::Error>> {
    let mut candidate = record(
        "record-at-flat-bound",
        "event-at-flat-bound",
        1,
        "repo",
        FILE_TOUCHED,
        &["src/bound.rs"],
    )?;
    candidate.attributes.insert(
        "bulk-padding".to_owned(),
        AttributeValue::Strings(vec![
            "x".repeat(MAX_IDENTIFIER_BYTES);
            MAX_ATTRIBUTES_PER_RECORD
        ]),
    );
    candidate
        .attributes
        .insert("zz-padding".to_owned(), AttributeValue::Strings(Vec::new()));
    loop {
        let AttributeValue::Strings(padding) = candidate
            .attributes
            .get_mut("zz-padding")
            .ok_or("missing exact-size padding")?
        else {
            return Err("unexpected padding type".into());
        };
        if padding.len() + 1 >= MAX_ATTRIBUTES_PER_RECORD {
            break;
        }
        padding.push("x".repeat(MAX_IDENTIFIER_BYTES));
        if canonical_json(&candidate)?.len() + 4 > target {
            let AttributeValue::Strings(padding) = candidate
                .attributes
                .get_mut("zz-padding")
                .ok_or("missing exact-size padding")?
            else {
                return Err("unexpected padding type".into());
            };
            padding.pop();
            break;
        }
    }
    let AttributeValue::Strings(padding) = candidate
        .attributes
        .get_mut("zz-padding")
        .ok_or("missing exact-size padding")?
    else {
        return Err("unexpected padding type".into());
    };
    padding.push("x".to_owned());
    let current = canonical_json(&candidate)?.len() + 4;
    let additional = target
        .checked_sub(current)
        .ok_or("bulk padding exceeded exact Flat record target")?;
    if additional >= MAX_IDENTIFIER_BYTES {
        return Err("could not construct exact-size Flat record".into());
    }
    let AttributeValue::Strings(padding) = candidate
        .attributes
        .get_mut("zz-padding")
        .ok_or("missing exact-size padding")?
    else {
        return Err("unexpected padding type".into());
    };
    *padding.last_mut().ok_or("missing final padding value")? = "x".repeat(additional + 1);
    candidate.validate()?;
    if canonical_json(&candidate)?.len() + 4 != target {
        return Err("could not construct exact-size Flat record".into());
    }
    Ok(candidate)
}

fn pseudo_random_term(index: usize) -> String {
    let mut state = u64::try_from(index).unwrap_or(u64::MAX).wrapping_add(1);
    let mut term = String::from("shard-");
    for _ in 0..8 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        term.push_str(&format!("{state:016x}"));
    }
    term
}

fn flip_block_byte(path: &Path, plaintext_offset: u64) -> std::io::Result<()> {
    const CONTAINER_DATA_OFFSET: u64 = crate::SEGMENT_HEADER_BYTES;
    const CHECKSUM_BYTES: u64 = crate::SEGMENT_CHECKSUM_BYTES;
    let chunk_bytes = u64::from(TEST_CHUNK_BYTES);
    let chunk = plaintext_offset / chunk_bytes;
    let within_chunk = plaintext_offset % chunk_bytes;
    let physical_offset =
        CONTAINER_DATA_OFFSET + chunk * (chunk_bytes + CHECKSUM_BYTES) + within_chunk;
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::Start(physical_offset))?;
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte)?;
    byte[0] ^= 0x80;
    file.seek(SeekFrom::Start(physical_offset))?;
    file.write_all(&byte)
}

fn citation(citation_id: &str, event_id: &str, event_sequence: u64) -> ServingCitation {
    ServingCitation {
        citation_id: citation_id.to_owned(),
        source_id: "source".to_owned(),
        source_revision_sha256: "a".repeat(64),
        session_id: "session".to_owned(),
        event_id: event_id.to_owned(),
        event_sequence,
        byte_start: Some(7),
        byte_end_exclusive: Some(19),
        evidence_sha256: "b".repeat(64),
        relationship: EvidenceRelationship::Supports,
    }
}

fn record_ids(records: &[ServingRecord]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record.record_id.as_str())
        .collect()
}

fn segment_writer(path: &Path) -> Result<SegmentWriter, SegmentFileError> {
    SegmentWriter::create(path, TEST_GENERATION, FLAT_SERVING_ROLE, TEST_CHUNK_BYTES)
}

pub fn segment_file(path: &Path) -> Result<SegmentFile, SegmentFileError> {
    SegmentFile::open(path, TEST_GENERATION, FLAT_SERVING_ROLE)
}

fn write_segment(
    path: &Path,
    records: Vec<ServingRecord>,
    tombstones: Vec<EventTombstone>,
) -> Result<FlatSegmentStats, FlatSegmentError> {
    FlatSegmentWriter::write(segment_writer(path)?, records, tombstones)
}

#[allow(dead_code)]
pub fn write_with_chunk(
    path: &Path,
    records: Vec<ServingRecord>,
    tombstones: Vec<EventTombstone>,
    chunk_bytes: u32,
) -> Result<FlatSegmentStats, FlatSegmentError> {
    let writer = SegmentWriter::create(path, TEST_GENERATION, FLAT_SERVING_ROLE, chunk_bytes)?;
    FlatSegmentWriter::write(writer, records, tombstones)
}

fn write_and_open(
    path: &Path,
    records: Vec<ServingRecord>,
    tombstones: Vec<EventTombstone>,
) -> Result<(FlatSegmentReader, FlatSegmentStats), FlatSegmentError> {
    let stats = write_segment(path, records, tombstones)?;
    let reader = FlatSegmentReader::open(segment_file(path)?)?;
    Ok((reader, stats))
}

fn write_bad_posting_segment(path: &Path) -> Result<(), FlatSegmentError> {
    let item = record("record", "event", 1, "repo", FILE_TOUCHED, &["term"])?;
    let encoded = canonical_json(&item)?;
    let records_bytes = add_frame_bytes(0, encoded.len(), "record section")?;
    let fst_key = index_key("repo", &FactFamily::new(FILE_TOUCHED)?, "term")?;
    let mut builder = MapBuilder::memory();
    builder.insert(&fst_key, 12)?;
    let fst = builder.into_inner()?;
    let directory = Directory {
        fst_shards: vec![FstShardDescriptor {
            offset: 0,
            length: u32::try_from(fst.len())
                .map_err(|_| FlatSegmentError::Bound("FST shard bytes"))?,
            key_count: 1,
            first_key: fst_key.clone(),
            last_key: fst_key,
        }],
        tombstone_pages: Vec::new(),
    }
    .encode()?;
    let header = Header {
        record_count: 1,
        index_key_count: 1,
        tombstone_count: 0,
        fst_shard_count: 1,
        tombstone_page_count: 0,
        index_association_count: 1,
        records_bytes,
        tombstone_pages_bytes: 0,
        postings_bytes: 12,
        fst_shards_bytes: u64::try_from(fst.len())
            .map_err(|_| FlatSegmentError::Bound("FST shard bytes"))?,
        directory_bytes: u64::try_from(directory.len())
            .map_err(|_| FlatSegmentError::Bound("directory bytes"))?,
    };
    let mut writer = segment_writer(path)?;
    writer.write_all(&header.encode())?;
    write_canonical_frame(&mut writer, &item, MAX_RECORD_BYTES)?;
    write_u32(&mut writer, 1)?;
    write_u64(&mut writer, 0)?;
    writer.write_all(&fst)?;
    writer.write_all(directory.as_slice())?;
    writer.finish()?;
    Ok(())
}

#[test]
fn length_preserving_record_and_posting_changes_are_detected_before_decoding()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let original = temp.path().join("valid");
    let records = (0..128)
        .map(|index| {
            record(
                &format!("record-{index:04}"),
                &format!("event-{index:04}"),
                index,
                "repo",
                FILE_TOUCHED,
                &["src/main.rs"],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    write_segment(&original, records, Vec::new())?;
    let reader = FlatSegmentReader::open(segment_file(&original)?)?;
    let record_offset = reader.layout.records_start + 12;
    let posting_offset = reader.layout.postings_start + 4;
    drop(reader);
    for (name, offset) in [("record", record_offset), ("posting", posting_offset)] {
        let path = temp.path().join(name);
        std::fs::copy(&original, &path)?;
        let length = std::fs::metadata(&path)?.len();
        flip_block_byte(&path, offset)?;
        assert_eq!(std::fs::metadata(&path)?.len(), length);
        match FlatSegmentReader::open(segment_file(&path)?) {
            Ok(mut reader) => assert!(
                reader
                    .query_exact("repo", &FactFamily::new(FILE_TOUCHED)?, "src/main.rs", 128)
                    .is_err()
            ),
            Err(FlatSegmentError::File(SegmentFileError::Corrupt("block checksum"))) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
