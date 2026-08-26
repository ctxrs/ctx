use super::*;

#[test]
fn dense_limit_windows_preserve_exact_order_more_available_and_winner_only_hydration() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let filters = EventSearchFilters::default();
    let full = collect_search_hits(
        &lexical_request(),
        &index,
        &filters,
        SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
        &UnusedSemanticPort,
    )
    .unwrap();
    let exact_order = full
        .result_window
        .hits
        .iter()
        .map(|hit| hit.event.event_id)
        .collect::<Vec<_>>();
    assert_eq!(exact_order.len(), records.len());

    for limit in 1..=records.len() {
        let mut request = lexical_request();
        request.limit = limit;
        ctx_history_index_query::reset_stored_event_record_materializations();
        ctx_history_index_query::reset_stored_core_event_record_materializations();
        ctx_history_index_query::reset_core_record_decodes();
        let collection = collect_search_hits(
            &request,
            &index,
            &filters,
            SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
            &UnusedSemanticPort,
        )
        .unwrap();
        assert_eq!(
            collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.event_id)
                .collect::<Vec<_>>(),
            exact_order[..limit]
        );
        assert_eq!(
            collection.result_window.more_available,
            limit < exact_order.len()
        );
        assert_eq!(
            ctx_history_index_query::stored_event_record_materializations(),
            0
        );
        assert_eq!(
            ctx_history_index_query::stored_core_event_record_materializations(),
            limit
        );
        assert_eq!(ctx_history_index_query::core_record_decodes(), limit);
    }
}

#[test]
fn search_application_pins_once_opens_semantics_once_and_requests_peer_lazily() {
    let temp = tempdir().unwrap();
    let (index, _) = publish(temp.path());
    let mut request = lexical_request();
    request.backend = Some(SearchBackend::Hybrid);
    let plan = plan_search(
        request,
        SearchPolicy {
            default_backend: SearchBackend::Hybrid,
            semantic: SemanticAvailability::Available,
        },
    )
    .unwrap();
    let mut generation = RecordingGenerationPort::new(index);
    let semantic = CountingSemanticPort(AtomicUsize::new(0));

    let result = execute_search(
        SearchApplicationRequest {
            plan,
            generation_target: GenerationReadTarget::Active,
            compact_projection: false,
            active_session: None,
        },
        &mut generation,
        &semantic,
    )
    .unwrap();

    assert_eq!(generation.calls.get(), 1);
    assert_eq!(generation.retained_peer.get(), Some(RetainedPeerRead::Omit));
    assert_eq!(semantic.0.load(Ordering::Relaxed), 1);
    assert_eq!(result.query().collection.result_window.hits.len(), 3);
    assert_eq!(
        result.query().collection.diversification.status,
        SearchDiversificationStatus::NotApplicable
    );
    assert_eq!(
        result.receipt().generation_id,
        result.index().generation_id()
    );
    let commands = result
        .query()
        .collection
        .result_window
        .hits
        .iter()
        .map(|_| SearchResultCommands {
            suggested_next_commands: Vec::new(),
        })
        .collect::<Vec<_>>();
    let read_model = result
        .render_read_model(SearchApplicationReadModelInput {
            commands: &commands,
            freshness_mode: "test",
            generated_at: "2026-08-11T12:00:00.000Z",
            semantic_fallback_code: None,
            semantic_fallback_detail: None,
            metrics: SearchRenderMetrics {
                refresh_status: "existing_generation",
                refresh_source_count: 1,
                query_duration: result.query_duration(),
            },
        })
        .unwrap();
    assert_eq!(read_model["results"].as_array().unwrap().len(), 3);
    assert_eq!(
        read_model["retrieval"]["generation_id"],
        result.receipt().generation_id
    );
    ctx_history_index_query::reset_stored_event_record_materializations();
    ctx_history_index_query::reset_stored_core_event_record_materializations();
    let compact_read_model = result.project_read_model(&read_model).unwrap();
    assert_eq!(compact_read_model["results"].as_array().unwrap().len(), 3);
    assert_eq!(
        ctx_history_index_query::stored_event_record_materializations(),
        0,
        "compact Search rendering must resolve indexed IDs without reloading EventRecord"
    );
    assert_eq!(
        ctx_history_index_query::stored_core_event_record_materializations(),
        0,
        "compact Search rendering must not hydrate Core after final winners"
    );

    let compact_index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut compact_generation = RecordingGenerationPort::new(compact_index);
    let compact = execute_search(
        SearchApplicationRequest {
            plan: plan_search(
                lexical_request(),
                SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
            )
            .unwrap(),
            generation_target: GenerationReadTarget::Active,
            compact_projection: true,
            active_session: None,
        },
        &mut compact_generation,
        &UnusedSemanticPort,
    )
    .unwrap();
    assert_eq!(compact_generation.calls.get(), 1);
    assert_eq!(
        compact_generation.retained_peer.get(),
        Some(RetainedPeerRead::IfAvailable)
    );
    assert_eq!(compact.query().collection.result_window.hits.len(), 3);
}

#[test]
fn candidate_reference_failure_reaches_the_application_terminal_error_with_partial_work() {
    let temp = tempdir().unwrap();
    let (index, _) = publish(temp.path());
    let plan = plan_search(
        lexical_request(),
        SearchPolicy::lexical_only(SemanticReason::PolicyDisabled),
    )
    .unwrap();
    let mut generation = RecordingGenerationPort::new(index);
    ctx_history_index_query::fail_lexical_candidate_materialization_after(1);

    let failure = match execute_search_observed(
        SearchApplicationRequest {
            plan,
            generation_target: GenerationReadTarget::Active,
            compact_projection: false,
            active_session: None,
        },
        &mut generation,
        &UnusedSemanticPort,
    ) {
        Ok(_) => {
            panic!("injected candidate-reference failure must reach the application terminal error")
        }
        Err(failure) => failure,
    };

    assert_eq!(
        failure.failure_phase(),
        SearchFailurePhase::IndexQueryDecode
    );
    let work = failure.work();
    assert_eq!(work.retrieval_rounds, Some(1));
    assert_eq!(work.query_executions, Some(1));
    assert_eq!(work.candidate_rows, Some(3));
    assert_eq!(work.records_decoded, Some(0));
    assert_eq!(work.encoded_core_bytes_decoded, Some(0));
}

#[test]
fn explicit_session_lexical_search_is_dense_and_never_diversified() {
    let temp = tempdir().unwrap();
    let (index, records) = publish(temp.path());
    let mut request = lexical_request();
    request.events = false;
    request.session = Some(records[0].session_id.to_string());
    let filters = search_filters(&request, &index, None).unwrap();
    let collection = collect_search_hits(
        &request,
        &index,
        &filters,
        SemanticAvailability::Unavailable(SemanticReason::PolicyDisabled),
        &UnusedSemanticPort,
    )
    .unwrap();

    assert_eq!(collection.result_window.hits.len(), 3);
    assert!(collection
        .result_window
        .hits
        .iter()
        .all(|hit| hit.more_matches_in_session == 0));
    assert_eq!(
        collection.diversification.status,
        SearchDiversificationStatus::NotApplicable
    );
}

#[test]
fn semantic_and_hybrid_share_coalesced_family_shaping_and_remain_indeterminate() {
    let temp = tempdir().unwrap();
    let (index, records) = publish_grouped_search(temp.path());
    let child_absent = index
        .event_by_id(records[1].event_id.as_uuid())
        .unwrap()
        .unwrap();
    let sibling = index
        .event_by_id(records[3].event_id.as_uuid())
        .unwrap()
        .unwrap();
    let independent = index
        .event_by_id(records[4].event_id.as_uuid())
        .unwrap()
        .unwrap();
    let semantic_candidates = vec![
        EventSearchCandidate {
            event: RankedEventRef::from(&child_absent),
            score: 100.0,
        },
        EventSearchCandidate {
            event: RankedEventRef::from(&sibling),
            score: 90.0,
        },
        EventSearchCandidate {
            event: RankedEventRef::from(&independent),
            score: 80.0,
        },
    ];
    let filters = ctx_history_index_query::EventSearchFilters::default();

    for backend in [SearchBackend::Semantic, SearchBackend::Hybrid] {
        let mut request = lexical_request();
        request.query = "zzzz-backend-only-token".to_owned();
        request.events = false;
        request.limit = 3;
        request.backend = Some(backend);
        request.semantic_weight = 1.0;
        let collection = collect_search_hits_using(
            &request,
            &index,
            &filters,
            SemanticAvailability::Available,
            |_, _, _| {
                Ok(HistorySemanticBatch {
                    candidates: semantic_candidates.clone(),
                    diagnostics: json!({"candidate_count": 3}),
                })
            },
        )
        .unwrap();

        assert_eq!(
            collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.provider_session_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["family-child", "independent", "family-sibling"]
        );
        assert_eq!(
            collection.diversification.status,
            SearchDiversificationStatus::Indeterminate
        );
        assert_eq!(collection.diversification.changed_final_top_n, None);
    }
}

#[test]
fn grouped_search_decodes_only_final_winners_for_every_backend() {
    let temp = tempdir().unwrap();
    let (index, records) = publish_grouped_search(temp.path());
    let semantic_candidates = records
        .iter()
        .enumerate()
        .map(|(position, record)| {
            let event = index
                .event_by_id(record.event_id.as_uuid())
                .unwrap()
                .unwrap();
            EventSearchCandidate {
                event: RankedEventRef::from(&event),
                score: (records.len() - position) as f32,
            }
        })
        .collect::<Vec<_>>();
    let semantic = FixedSemanticPort(semantic_candidates);
    let query = PinnedHistoryQuery::new(&index, None);

    for backend in [
        SearchBackend::Lexical,
        SearchBackend::Semantic,
        SearchBackend::Hybrid,
    ] {
        let mut request = lexical_request();
        request.query = "groupneedle".to_owned();
        request.events = false;
        request.limit = 3;
        request.backend = Some(backend);
        request.semantic_weight = 0.5;
        let plan = plan_search(
            request,
            SearchPolicy {
                default_backend: backend,
                semantic: SemanticAvailability::Available,
            },
        )
        .unwrap();

        ctx_history_index_query::reset_stored_event_record_materializations();
        ctx_history_index_query::reset_stored_core_event_record_materializations();
        ctx_history_index_query::reset_core_record_decodes();
        let search = query.search(plan, None, &semantic).unwrap();
        let winners = search.collection.result_window.hits.len();

        assert_eq!(winners, 3);
        assert_eq!(
            ctx_history_index_query::stored_event_record_materializations(),
            0
        );
        assert_eq!(
            ctx_history_index_query::stored_core_event_record_materializations(),
            winners
        );
        assert_eq!(ctx_history_index_query::core_record_decodes(), winners);
    }
}

#[test]
fn provider_root_and_group_selectors_share_one_source_predicate_across_backends() {
    let temp = tempdir().unwrap();
    let (index, records) = publish_provider_root_search(temp.path());
    let semantic_candidates = [(4, 400.0), (1, 300.0), (2, 200.0), (3, 100.0)]
        .into_iter()
        .map(|(record, score)| EventSearchCandidate {
            event: RankedEventRef::from(
                &index
                    .event_by_id(records[record].event_id.as_uuid())
                    .unwrap()
                    .unwrap(),
            ),
            score,
        })
        .collect::<Vec<_>>();
    let expected_sources = vec![source_token(&records[0].source)];
    let mut expected_events = vec![records[1].event_id.as_uuid(), records[3].event_id.as_uuid()];
    expected_events.sort();
    let mut expected_sessions = vec![
        records[1].session_id.as_uuid(),
        records[3].session_id.as_uuid(),
    ];
    expected_sessions.sort();

    for (source_roots, source_groups) in [
        (vec!["personal".to_owned()], Vec::new()),
        (Vec::new(), vec!["work".to_owned()]),
    ] {
        for backend in [
            SearchBackend::Lexical,
            SearchBackend::Semantic,
            SearchBackend::Hybrid,
        ] {
            let mut request = lexical_request();
            request.query = "parityneedle".to_owned();
            request.limit = 2;
            request.events = false;
            request.backend = Some(backend);
            request.semantic_weight = 0.5;
            request.source_roots.clone_from(&source_roots);
            request.source_groups.clone_from(&source_groups);
            let filters = search_filters(&request, &index, None).unwrap();
            assert_eq!(
                filters.allowed_source_keys.as_ref(),
                Some(&expected_sources)
            );

            let collection = collect_search_hits_using(
                &request,
                &index,
                &filters,
                SemanticAvailability::Available,
                |_, semantic_filters, _| {
                    assert_eq!(
                        semantic_filters.filters().allowed_source_keys.as_ref(),
                        Some(&expected_sources)
                    );
                    let projection = index.semantic_filter_projection(semantic_filters).unwrap();
                    assert!(projection.contains(records[1].event_id.as_uuid()));
                    assert!(!projection.contains(records[4].event_id.as_uuid()));
                    Ok(HistorySemanticBatch {
                        candidates: semantic_candidates
                            .iter()
                            .filter(|candidate| projection.contains(candidate.event.event_id))
                            .cloned()
                            .collect(),
                        diagnostics: json!({"adapter": "semantic-filter-projection-test"}),
                    })
                },
            )
            .unwrap();

            let mut selected_events = collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.event_id)
                .collect::<Vec<_>>();
            selected_events.sort();
            assert_eq!(selected_events, expected_events);
            let mut selected_sessions = collection
                .result_window
                .hits
                .iter()
                .map(|hit| hit.event.session_id)
                .collect::<Vec<_>>();
            selected_sessions.sort();
            assert_eq!(selected_sessions, expected_sessions);
            match backend {
                SearchBackend::Lexical => {
                    assert_eq!(
                        collection.diversification.status,
                        SearchDiversificationStatus::Applied
                    );
                    assert_eq!(collection.diversification.changed_final_top_n, Some(false));
                }
                SearchBackend::Semantic | SearchBackend::Hybrid => {
                    assert_eq!(
                        collection.diversification.status,
                        SearchDiversificationStatus::Indeterminate
                    );
                    assert_eq!(collection.diversification.changed_final_top_n, None);
                }
            }
        }
    }
}
