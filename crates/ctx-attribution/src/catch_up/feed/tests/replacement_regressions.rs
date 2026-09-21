//! Synthetic neutral Core records exercise the real writer and materializer.
//! These are consumer-contract regressions, not native provider captures.

use super::*;
use crate::protocol::core_record_sha256;

fn publish_records(fixture: &Fixture, revision: u8, records: &[CoreRecord]) -> String {
    let source = &records[0].source;
    let mut writer = GenerationWriter::open(&fixture.index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        record.validate_contract().unwrap();
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(certificate(source, revision, records.len()))
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

fn assert_reopened_event_states(
    fixture: &Fixture,
    generation: &str,
    records: &[CoreRecord],
) -> bool {
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, generation).unwrap();
    let mut materializer = fixture.materializer();
    let CoreMaterializationSyncOutcome::Finished { receipt, did_work } =
        sync_generation_pinned_core(&fixture.data_root, &snapshot, &mut materializer, None)
            .unwrap();
    assert!(!did_work, "reopened current generation must be a no-op");
    assert_eq!(receipt.core_generation_id, generation);
    assert_eq!(receipt.event_count, records.len() as u64);

    // Ask a prospective source reconciliation for the authenticated active
    // event states, then abandon it without publishing another generation.
    let mut sources = <CoreSnapshot as CoreFeedSnapshot>::source_states(&snapshot).unwrap();
    assert_eq!(sources.len(), 1);
    sources[0].core_record_accumulator = "d".repeat(64);
    let query_generation = "e".repeat(64);
    let head = core_generation_head_from_schema(
        &<CoreSnapshot as CoreFeedSnapshot>::schema(&snapshot),
        &query_generation,
        &sources,
    )
    .unwrap();
    let mut session = match materializer.start_core_generation(head).unwrap() {
        CoreGenerationStart::Current(_) => panic!("prospective generation is not current"),
        CoreGenerationStart::Started(session) => session,
    };
    let reconciliations = session
        .reconcile_source_page(
            CoreSourceDeltaPage::new(
                "0".repeat(64),
                query_generation,
                0,
                true,
                sources.into_iter().map(CoreSourceDelta::Present).collect(),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(reconciliations.len(), 1);
    let (states, terminal) = session.event_states(&reconciliations[0], None).unwrap();
    assert!(terminal);
    assert_eq!(states.len(), records.len());
    for record in records {
        let state = states
            .iter()
            .find(|state| state.event_id == record.event_id)
            .expect("every current Core event survives publication");
        assert_eq!(
            state.core_record_sha256,
            core_record_sha256(record).unwrap()
        );
    }
    let requires_replacement = states[0].requires_replacement;
    assert!(
        states
            .iter()
            .all(|state| state.requires_replacement == requires_replacement)
    );
    requires_replacement
}

#[test]
fn compaction_preserves_equal_hash_event_across_the_layer_limit_and_restart() {
    let fixture = Fixture::new();
    let source = source("compaction-replacements.jsonl");
    let mut retained = record(&source, 1, "unchanged retained event");
    retained.root_session_id = Some(retained.session_id);
    let retained_hash = core_record_sha256(&retained).unwrap();
    let mut materializer = fixture.materializer();

    // One base plus fifteen replacement layers reaches the documented limit.
    // The seventeenth generation must rebuild, including the unchanged event;
    // the eighteenth proves ordinary incremental replacement still works.
    for revision in 1..=18_u8 {
        let mut changing = record(&source, 2, &format!("revision {revision}"));
        changing.root_session_id = Some(changing.session_id);
        let records = [retained.clone(), changing];
        assert_eq!(core_record_sha256(&records[0]).unwrap(), retained_hash);
        let generation = publish_records(&fixture, revision, &records);
        let receipt = sync(&fixture, &generation, &mut materializer);
        assert_eq!(receipt.core_generation_id, generation);
        assert_eq!(receipt.event_count, 2);

        if revision >= 15 {
            drop(materializer);
            // Observe rebuild admission at the actual retained-layer limit,
            // followed by its reset after the compacted publication.
            assert_eq!(
                assert_reopened_event_states(&fixture, &generation, &records),
                revision == 16,
                "rebuild admission after generation {revision}"
            );
            materializer = fixture.materializer();
        }
    }
}

#[test]
fn stable_event_replacement_moves_sequence_in_both_directions_and_then_is_removed() {
    // Cover optional root ownership without constructing Fact/EventOwner DTOs.
    for rooted in [false, true] {
        let fixture = Fixture::new();
        let source = source("sequence-replacements.jsonl");
        let mut event = record(&source, 7, "stable native message");
        if rooted {
            event.root_session_id = Some(event.session_id);
        }
        let stable_id = event.event_id;
        let mut retained = record(&source, 100, "unrelated retained event");
        retained.root_session_id = event.root_session_id;
        for (index, sequence) in [7, 3, 11].into_iter().enumerate() {
            event.event_sequence = sequence;
            assert_eq!(event.event_id, stable_id);
            let records = [event.clone(), retained.clone()];
            let generation = publish_records(&fixture, index as u8 + 1, &records);
            let mut materializer = fixture.materializer();
            let receipt = sync(&fixture, &generation, &mut materializer);
            assert_eq!(receipt.event_count, 2);

            drop(materializer);
            assert!(!assert_reopened_event_states(
                &fixture,
                &generation,
                &records
            ));
        }
        // A subsequent removal must target the replacement's current owner.
        let records = [retained];
        let generation = publish_records(&fixture, 4, &records);
        let mut materializer = fixture.materializer();
        let receipt = sync(&fixture, &generation, &mut materializer);
        assert_eq!(receipt.event_count, 1);

        drop(materializer);
        assert!(!assert_reopened_event_states(
            &fixture,
            &generation,
            &records
        ));
    }
}

#[test]
fn replacement_rejects_another_events_prior_hash_missing_prior_and_foreign_source() {
    let fixture = Fixture::new();
    let source = source("replacement-prior-authority.jsonl");
    let records = [record(&source, 1, "event A"), record(&source, 2, "event B")];
    let generation = publish_records(&fixture, 1, &records);
    let mut materializer = fixture.materializer();
    let receipt = sync(&fixture, &generation, &mut materializer);

    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    let mut sources = <CoreSnapshot as CoreFeedSnapshot>::source_states(&snapshot).unwrap();
    sources[0].core_record_accumulator = "d".repeat(64);
    let query_generation = "e".repeat(64);
    let head = core_generation_head_from_schema(
        &<CoreSnapshot as CoreFeedSnapshot>::schema(&snapshot),
        &query_generation,
        &sources,
    )
    .unwrap();
    for invalid in 0..3 {
        let mut replacement = records[1].clone();
        let prior_hash = core_record_sha256(&records[0]).unwrap();
        match invalid {
            // The API chooses B's prior by B's identity, never by A's hash.
            0 => replacement.event_sequence = records[0].event_sequence,
            1 => replacement = record(&source, 3, "missing prior"),
            2 => replacement = record(&super::source("foreign.jsonl"), 1, "foreign source"),
            _ => unreachable!(),
        }
        let mut session = match materializer.start_core_generation(head.clone()).unwrap() {
            CoreGenerationStart::Current(_) => panic!("prospective generation is not current"),
            CoreGenerationStart::Started(session) => session,
        };
        let reconciliations = session
            .reconcile_source_page(
                CoreSourceDeltaPage::new(
                    "0".repeat(64),
                    query_generation.clone(),
                    0,
                    true,
                    sources
                        .iter()
                        .cloned()
                        .map(CoreSourceDelta::Present)
                        .collect(),
                )
                .unwrap(),
            )
            .unwrap();
        assert!(
            session
                .ingest_event_pages(vec![CoreEventDeltaPage {
                    materialization_id: "0".repeat(64),
                    core_generation_id: query_generation.clone(),
                    reconciliation: reconciliations[0].clone(),
                    page_index: 0,
                    terminal: true,
                    deltas: vec![CoreEventDelta::Replaced(CoreEventReplacement {
                        prior_core_record_sha256: prior_hash,
                        record: replacement,
                    })],
                }])
                .is_err(),
            "invalid replacement case {invalid}"
        );
        drop(session);
        let status = materializer
            .projection_status(&StatusRequest {
                requested_core_generation_id: Some(generation.clone()),
            })
            .unwrap();
        assert_eq!(status.receipt.as_ref(), Some(&receipt));
    }
    drop(materializer);
    assert!(!assert_reopened_event_states(
        &fixture,
        &generation,
        &records
    ));
}
