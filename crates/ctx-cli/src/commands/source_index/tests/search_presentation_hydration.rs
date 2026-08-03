#[test]
fn file_only_search_keeps_an_oversized_grapheme_body_without_requiring_a_body_match() {
    let temp = tempdir().unwrap();
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 94, 1);
    let oversized_cluster = format!("x{}", "\u{301}".repeat(SEARCH_SNIPPET_MAX_BYTES));
    let mut stored = fixture_core_event(&event, oversized_cluster);
    stored
        .core_record
        .repository_bindings
        .push(RepositoryBinding {
            binding_id: "binding-1".to_owned(),
            logical_repository_id: "repo-1".to_owned(),
            checkout_id: None,
            worktree_id: None,
            aliases: Vec::new(),
            git_object_format: None,
            local_root_authorization: None,
            evidence: vec![RepositoryEvidence {
                kind: RepositoryEvidenceKind::FileActivity,
                confidence: RepositoryEvidenceConfidence::Explicit,
            }],
            association_policy_revision:
                ctx_history_core::CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
        });
    stored.core_record.repository_file_observations = vec![RepositoryFileObservation {
        repository_binding_id: "binding-1".to_owned(),
        relative_path: "src/huge.rs".to_owned(),
        kind: RepositoryFileObservationKind::Modified,
        prior_relative_path: None,
    }];
    stored.core_record.validate_contract().unwrap();
    append_fixture_session(temp.path(), std::slice::from_ref(&stored), 94);

    let mut source_request = request(RefreshArg::Off);
    source_request.query.clear();
    source_request.file = Some(PathBuf::from("src/huge.rs"));
    source_request.events = true;
    source_request.limit = 1;
    let (value, collection, _) = search_existing_generation(
        &source_request,
        open_index(temp.path()).unwrap(),
        temp.path(),
        source_request.semantic_weight,
        "existing_generation",
        1,
    )
    .unwrap();

    assert_eq!(collection.result_window.hits.len(), 1);
    assert_eq!(
        value["results"][0]["ctx_event_id"],
        json!(event.event_id.as_uuid())
    );
    assert_eq!(value["results"][0]["snippet"], "");
    assert_eq!(value["results"][0]["snippet_truncated"], true);

    let (mcp, _) = mcp_search(source_request, temp.path()).unwrap();
    assert_eq!(
        mcp["results"][0]["ctx_event_id"],
        json!(event.event_id.as_uuid())
    );
    assert_eq!(mcp["results"][0]["snippet"], "");
    assert_eq!(mcp["results"][0]["snippet_truncated"], true);
}

#[test]
fn search_presentation_hydration_rejects_missing_duplicate_and_misaligned_hits() {
    let temp = tempdir().unwrap();
    let event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 92, 1);
    let stored = fixture_core_event(&event, format!("stored {TEST_QUERY} body"));
    append_fixture_session(temp.path(), std::slice::from_ref(&stored), 92);
    let index = open_index(temp.path()).unwrap();
    let query = NormalizedSearchQuery::from_request(&request(RefreshArg::Off));
    let expected_event = SearchEventMetadata::from(&event);
    let hit = SearchHit {
        event: expected_event.clone(),
        score: 1.0,
        more_matches_in_session: 0,
    };

    let presentations = presentations_for_search_hits_with_budget(
        &index,
        std::slice::from_ref(&hit),
        &query,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
    .unwrap();
    assert_eq!(presentations.len(), 1);
    assert_eq!(presentations[0].event, &expected_event);

    let duplicate_error = presentations_for_search_hits_with_budget(
        &index,
        &[hit.clone(), hit.clone()],
        &query,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
    .unwrap_err();
    assert!(duplicate_error
        .to_string()
        .contains("search result duplicated Core event"));

    let missing_event = fixture_event(CaptureProvider::Codex, "codex_session_jsonl", 93, 1);
    let missing_error = presentations_for_search_hits_with_budget(
        &index,
        &[SearchHit {
            event: SearchEventMetadata::from(&missing_event),
            score: 1.0,
            more_matches_in_session: 0,
        }],
        &query,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
    .unwrap_err();
    assert_eq!(
        missing_error.to_string(),
        format!(
            "pinned Core lookup omitted search event {}",
            missing_event.event_id
        )
    );

    let mut misaligned = hit;
    misaligned.event.event_sequence += 1;
    let misaligned_error = presentations_for_search_hits_with_budget(
        &index,
        &[misaligned],
        &query,
        SEARCH_PRESENTATION_HYDRATION_BUDGET,
    )
    .unwrap_err();
    assert!(misaligned_error.to_string().contains("misaligned metadata"));
}
