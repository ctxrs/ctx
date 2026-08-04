use super::*;

const LONG_SOURCE_RECORDS: usize = 513;
const LONG_BODY_BYTES: usize = 1_024;

fn long_bodies(label: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{label}-{index:04}-{}", "s".repeat(LONG_BODY_BYTES)))
        .collect()
}

fn encoded_record_bytes(
    fixture: &Fixture,
    source_index: usize,
    bodies: &[String],
    sequence_offset: usize,
) -> Result<u64> {
    bodies
        .iter()
        .enumerate()
        .try_fold(0_u64, |total, (index, body)| {
            let sequence = u64::try_from(sequence_offset + index + 1)?;
            let bytes = fixture
                .record(source_index, sequence, body)?
                .encode_stored()?
                .len();
            Ok(total
                .checked_add(u64::try_from(bytes)?)
                .ok_or_else(|| anyhow!("semantic proportionality byte count overflowed"))?)
        })
}

#[test]
fn changed_logical_source_replay_work_is_measured_for_jsonl_and_sqlite() -> Result<()> {
    for source_format in ["codex_session_jsonl", "shelley_sqlite"] {
        let fixture = Fixture::new_with_source_format(1, source_format)?;
        let initial_bodies = long_bodies(source_format, LONG_SOURCE_RECORDS);
        let initial = fixture.publish("proportional-initial", &[(0, initial_bodies.clone())])?;
        let mut appended_bodies = initial_bodies.clone();
        appended_bodies.push(format!("{source_format}-appended-one-event"));
        let appended = fixture.publish("proportional-append", &[(0, appended_bodies.clone())])?;
        let mut replacement_bodies = appended_bodies.clone();
        replacement_bodies[LONG_SOURCE_RECORDS / 2] =
            format!("{source_format}-one-record-replacement");
        let replacement = fixture.publish(
            "proportional-replacement",
            &[(0, replacement_bodies.clone())],
        )?;
        let removed = fixture.publish("proportional-removed", &[])?;
        let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
        let mut builder = CoreBuilder::default();
        let mut embedder = MarkerEmbedder::default();
        reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;

        let append = reconcile_all(&mut store, &appended, &mut builder, &mut embedder)?;
        let append_bytes = encoded_record_bytes(&fixture, 0, &appended_bodies, 0)?;
        let appended_event_bytes = encoded_record_bytes(
            &fixture,
            0,
            &appended_bodies[LONG_SOURCE_RECORDS..],
            LONG_SOURCE_RECORDS,
        )?;
        assert_eq!(append.records_decoded, LONG_SOURCE_RECORDS + 1);
        assert_eq!(append.record_bytes_decoded, append_bytes);
        assert_eq!(append.records_reused, LONG_SOURCE_RECORDS);
        assert_eq!(append.records_embedded, 1);
        assert!(append_bytes / appended_event_bytes >= 128);
        eprintln!(
            "semantic format={source_format} transition=one_event_append changed_events=1 decoded_records={} decoded_bytes={append_bytes} changed_record_bytes={appended_event_bytes} byte_amplification={}x",
            append.records_decoded,
            append_bytes / appended_event_bytes
        );

        drop(store);
        let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
        let restarted = reconcile_all(&mut store, &appended, &mut builder, &mut embedder)?;
        assert_eq!(restarted.records_decoded, 0);
        assert_eq!(restarted.record_bytes_decoded, 0);
        eprintln!(
            "semantic format={source_format} transition=completed_restart decoded_records=0 decoded_bytes=0"
        );

        let replacement_outcome =
            reconcile_all(&mut store, &replacement, &mut builder, &mut embedder)?;
        let replacement_bytes = encoded_record_bytes(&fixture, 0, &replacement_bodies, 0)?;
        let replacement_index = LONG_SOURCE_RECORDS / 2;
        let replaced_record_bytes = encoded_record_bytes(
            &fixture,
            0,
            &replacement_bodies[replacement_index..replacement_index + 1],
            replacement_index,
        )?;
        assert_eq!(replacement_outcome.records_decoded, LONG_SOURCE_RECORDS + 1);
        assert_eq!(replacement_outcome.record_bytes_decoded, replacement_bytes);
        assert_eq!(replacement_outcome.records_reused, LONG_SOURCE_RECORDS);
        assert_eq!(replacement_outcome.records_embedded, 1);
        assert!(replacement_bytes / replaced_record_bytes >= 128);
        eprintln!(
            "semantic format={source_format} transition=one_record_replacement changed_events=1 decoded_records={} decoded_bytes={replacement_bytes} changed_record_bytes={replaced_record_bytes} byte_amplification={}x",
            replacement_outcome.records_decoded,
            replacement_bytes / replaced_record_bytes
        );

        let removal = reconcile_all(&mut store, &removed, &mut builder, &mut embedder)?;
        assert_eq!(removal.records_decoded, 0);
        assert_eq!(removal.record_bytes_decoded, 0);
        assert_eq!(removal.deleted_chunks, LONG_SOURCE_RECORDS + 1);
        eprintln!(
            "semantic format={source_format} transition=source_removal removed_events={} decoded_records=0 decoded_bytes=0",
            LONG_SOURCE_RECORDS + 1
        );
    }
    Ok(())
}

#[test]
fn interrupted_restart_redecodes_the_uncommitted_page_for_jsonl_and_sqlite() -> Result<()> {
    for source_format in ["codex_session_jsonl", "shelley_sqlite"] {
        let fixture = Fixture::new_with_source_format(1, source_format)?;
        let bodies = long_bodies(source_format, LONG_SOURCE_RECORDS);
        let index = fixture.publish("proportional-resume", &[(0, bodies.clone())])?;
        let mut builder = CoreBuilder {
            fail_after: Some(256),
            ..CoreBuilder::default()
        };
        let mut embedder = MarkerEmbedder::default();
        {
            let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
            let error = store
                .reconcile_source_backed_index(&index, &mut builder, &mut embedder)
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("forced Core projection interruption"));
        }

        builder.fail_after = None;
        builder.calls.clear();
        let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
        let resumed = reconcile_all(&mut restarted, &index, &mut builder, &mut embedder)?;
        let resumed_bytes = encoded_record_bytes(&fixture, 0, &bodies, 0)?;
        assert_eq!(resumed.records_decoded, LONG_SOURCE_RECORDS);
        assert_eq!(resumed.record_bytes_decoded, resumed_bytes);
        assert_eq!(active_events(&restarted)?, LONG_SOURCE_RECORDS);
        eprintln!(
            "semantic format={source_format} transition=interrupted_restart decoded_records={LONG_SOURCE_RECORDS} decoded_bytes={resumed_bytes}"
        );
    }
    Ok(())
}
