// Parallel staging behavior.

#[test]
fn staging_page_projection_is_exact_across_worker_counts() -> TestResult {
    let template = one_record()?;
    let records = (0..4_u32)
        .map(|index| indexed_record(&template, index))
        .collect::<TestResult<Vec<_>>>()?;
    let mut source = source_state(&records[0], 0x62);
    source.event_count = u64::try_from(records.len())?;
    let reconciliation = CoreSourceReconciliation {
        materialize_index: 0,
        delta: CoreSourceDelta::Present(source),
    };
    let mut prepared = records
        .into_iter()
        .enumerate()
        .map(|(page_index, record)| {
            prepared_page(CoreEventDeltaPage {
                materialization_id: "62".repeat(32),
                core_generation_id: "72".repeat(32),
                reconciliation: reconciliation.clone(),
                page_index: u32::try_from(page_index)?,
                terminal: page_index == 3,
                deltas: vec![CoreEventDelta::Added(record)],
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    for (index, page) in prepared.iter_mut().enumerate() {
        for unit in page.units.values_mut() {
            let mut subject = ResourceRef::new(ResourceKind::File, format!("src/{index}.rs"));
            subject.repository_id = Some("repo".to_owned());
            let direct_session_id = unit
                .evidence
                .as_ref()
                .ok_or_else(|| io::Error::other("projection parity evidence is missing"))?
                .citation
                .session_id
                .to_string();
            let root_session_id = unit.facts[0].root_session_id.clone();
            unit.facts = vec![Fact::create(
                FILE_TOUCHED,
                subject,
                "touches",
                None,
                None,
                Confidence::Verified,
                FactState::Asserted,
                "segment_materializer.test",
                "1",
                direct_session_id,
                root_session_id,
                Vec::new(),
                BTreeMap::from([("line_count_delta".to_owned(), index.to_string())]),
            )];
        }
    }

    let reference = super::staging::prepare_page_projections_for_test(&prepared, 1)?;
    assert!(
        reference
            .iter()
            .all(|projection| !projection.record_evidence.is_empty())
    );
    for workers in [2, 4, 16] {
        let candidate = super::staging::prepare_page_projections_for_test(&prepared, workers)?;
        assert_eq!(candidate, reference, "workers={workers}");
    }
    Ok(())
}

#[test]
fn staging_page_projection_rejects_aggregate_prepared_output() {
    let cap = crate::protocol::MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES;
    assert!(
        super::staging::validate_projection_output_lengths_for_test([cap / 2, cap / 2]).is_ok()
    );
    assert!(matches!(
        super::staging::validate_projection_output_lengths_for_test([cap / 2 + 1, cap / 2 + 1]),
        Err(SegmentMaterializerError::Bounds)
    ));
    assert!(matches!(
        super::staging::validate_projection_output_lengths_for_test([usize::MAX, 1]),
        Err(SegmentMaterializerError::Bounds)
    ));
}
