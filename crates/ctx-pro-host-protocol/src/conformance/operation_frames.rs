use super::*;

#[test]
#[allow(clippy::too_many_lines)]
fn operation_frames_cover_every_typed_request_response_and_operation_variant() {
    let value = inventory();
    let host_frames = value["golden_vectors"]["operation_frames"]["host_request_frames"]
        .as_object()
        .expect("host operation frames");
    let helper_frames = value["golden_vectors"]["operation_frames"]["helper_response_frames"]
        .as_object()
        .expect("helper operation frames");
    let expected_host = BTreeSet::from([
        "authorize_active",
        "authorize_canceling_paid",
        "authorize_trial",
        "blame_commit",
        "blame_file",
        "blame_file_line",
        "blame_file_range",
        "blame_pull_request_number",
        "blame_pull_request_url",
        "materialize_output_page_append_or_resume_tool_failure",
        "materialize_output_page_new_source_command_success",
        "materialize_output_page_rewrite_command_timeout_and_tool_unknown",
        "observe_output_source_available",
        "observe_output_source_error",
        "observe_output_source_unavailable",
        "sync_journal_full_baseline_upsert",
        "sync_journal_incremental_delete",
    ]);
    let expected_helper = BTreeSet::from([
        "authorized_active",
        "authorized_canceling_paid",
        "authorized_locked",
        "authorized_offline_grace",
        "authorized_trial",
        "blame_commit",
        "blame_file",
        "blame_file_line",
        "blame_file_range",
        "blame_pull_request_activity_without_commit_membership",
        "blame_pull_request_commit_membership",
        "output_source_observed_available",
        "output_source_observed_error",
        "output_source_observed_unavailable",
        "status_needs_rebuild",
        "status_needs_resume",
        "status_not_materialized",
        "status_partial",
        "status_ready",
    ]);
    assert_eq!(
        host_frames
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_host
    );
    assert_eq!(
        helper_frames
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_helper
    );
    assert!(host_frames
        .keys()
        .chain(helper_frames.keys())
        .all(|name| !name.contains("issue")));

    let mut entitlement_access_kind = BTreeSet::new();
    let mut entitlement_access_state = BTreeSet::new();
    let mut entitlement_capability = BTreeSet::new();
    let mut graph_state = BTreeSet::new();
    let mut journal_entity_kind = BTreeSet::new();
    let mut journal_operation = BTreeSet::new();
    let mut journal_sync_mode = BTreeSet::new();
    let mut observation_kind = BTreeSet::new();
    let mut output_observation_kind = BTreeSet::new();
    let mut output_outcome = BTreeSet::new();
    let mut output_source_availability = BTreeSet::new();
    let mut output_source_disposition = BTreeSet::new();
    let mut blame_match_kind = BTreeSet::new();
    let mut blame_target_kind = BTreeSet::new();
    let mut commit_fact_type = BTreeSet::new();
    let mut commit_predicate = BTreeSet::new();
    let mut continuation_reason = BTreeSet::new();
    let mut fact_confidence = BTreeSet::new();
    let mut fact_state = BTreeSet::new();
    let mut production_relationship = BTreeSet::new();
    let mut pull_request_action = BTreeSet::new();
    let mut pull_request_commit_relationship = BTreeSet::new();
    let mut pull_request_relationship_kind = BTreeSet::new();
    let mut worktree_status = BTreeSet::new();

    for (name, encoded) in host_frames {
        let bytes = unhex(encoded.as_str().expect("host operation frame hex"));
        let envelope = read_frame::<_, HostEnvelope>(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(host_operation_kind(&envelope.message), Some(name.as_str()));
        match &envelope.message {
            HostMessage::Authorize(request) => {
                insert_wire(
                    &mut entitlement_access_kind,
                    request.entitlement.grant.access_kind,
                );
                for capability in &request.entitlement.grant.capabilities {
                    insert_wire(&mut entitlement_capability, capability);
                }
            }
            HostMessage::SyncJournal(request) => {
                request.validate().unwrap();
                insert_wire(&mut journal_sync_mode, request.mode);
                for record in &request.records {
                    insert_wire(&mut journal_operation, record.operation);
                    insert_wire(&mut journal_entity_kind, record.entity_kind);
                }
            }
            HostMessage::ObserveOutputSource(request) => {
                request.validate().unwrap();
                insert_wire(&mut output_source_availability, request.availability);
            }
            HostMessage::MaterializeOutputPage(page) => {
                page.validate().unwrap();
                insert_wire(&mut output_source_disposition, &page.disposition);
                for observation in &page.observations {
                    insert_wire(&mut output_observation_kind, observation.kind);
                    insert_wire(&mut output_outcome, observation.outcome.outcome);
                }
            }
            HostMessage::Blame(request) => {
                request.validate().unwrap();
                blame_target_kind.insert(
                    match &request.target {
                        BlameTarget::File { .. } => "file",
                        BlameTarget::Commit { .. } => "commit",
                        BlameTarget::PullRequest { .. } => "pull_request",
                    }
                    .to_owned(),
                );
            }
            HostMessage::Hello(_)
            | HostMessage::PrepareGraphKeyDeletion(_)
            | HostMessage::ConfirmGraphKeyDeletion(_)
            | HostMessage::Status(_)
            | HostMessage::BeginOutputInventory(_)
            | HostMessage::FinishOutputInventory(_)
            | HostMessage::GetOutputProgress(_)
            | HostMessage::BeginSourceManifest(_)
            | HostMessage::BeginSourceManifestAdmission(_)
            | HostMessage::AdmitSourceManifestPage(_)
            | HostMessage::FinishSourceManifestAdmission(_)
            | HostMessage::PrepareSource(_)
            | HostMessage::MaterializeSourcePage(_)
            | HostMessage::DeleteSource(_)
            | HostMessage::FinishSourceManifest(_)
            | HostMessage::FinishAdmittedSourceManifest(_) => {
                panic!("{name} is not an operation-specific host fixture");
            }
        }
    }

    for (name, encoded) in helper_frames {
        let bytes = unhex(encoded.as_str().expect("helper operation frame hex"));
        let envelope = read_frame::<_, HelperEnvelope>(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(
            helper_operation_kind(&envelope.message),
            Some(name.as_str())
        );
        match &envelope.message {
            HelperMessage::Authorized(result) => {
                insert_wire(&mut entitlement_access_state, result.state);
                for capability in &result.capabilities {
                    insert_wire(&mut entitlement_capability, capability);
                }
            }
            HelperMessage::Status(result) => {
                insert_wire(&mut graph_state, result.state);
            }
            HelperMessage::OutputSourceObserved(result) => {
                insert_wire(&mut output_source_availability, result.availability);
            }
            HelperMessage::Blame(result) => {
                blame_target_kind.insert(
                    match &result.target {
                        ResolvedBlameTarget::File { .. } => "file",
                        ResolvedBlameTarget::Commit { .. } => "commit",
                        ResolvedBlameTarget::PullRequest { .. } => "pull_request",
                    }
                    .to_owned(),
                );
                if let Some(snapshot) = &result.git_snapshot {
                    insert_wire(&mut worktree_status, snapshot.worktree_status);
                }
                if let Some(next) = &result.next {
                    insert_wire(&mut continuation_reason, next.reason);
                }
                for evidence in &result.evidence {
                    if let Some(kind) = evidence.citation.observation_kind {
                        insert_wire(&mut observation_kind, kind);
                    }
                }
                for blame_match in &result.matches {
                    match blame_match {
                        BlameMatch::File(file) => {
                            blame_match_kind.insert("file".to_owned());
                            for attribution in &file.production {
                                insert_wire(&mut production_relationship, attribution.relationship);
                                insert_wire(&mut fact_confidence, attribution.confidence);
                                insert_wire(&mut fact_state, attribution.state);
                            }
                        }
                        BlameMatch::Commit(commit) => {
                            blame_match_kind.insert("commit".to_owned());
                            insert_wire(&mut commit_fact_type, commit.fact_type);
                            insert_wire(&mut commit_predicate, commit.predicate);
                            insert_wire(&mut fact_confidence, commit.confidence);
                            insert_wire(&mut fact_state, commit.state);
                        }
                        BlameMatch::PullRequest(pull_request) => {
                            blame_match_kind.insert("pull_request".to_owned());
                            match &pull_request.relationship {
                                PullRequestBlameRelationship::Activity(activity) => {
                                    pull_request_relationship_kind.insert("activity".to_owned());
                                    insert_wire(&mut pull_request_action, activity.action);
                                    insert_wire(&mut fact_confidence, activity.confidence);
                                    insert_wire(&mut fact_state, activity.state);
                                }
                                PullRequestBlameRelationship::Commit(commit) => {
                                    pull_request_relationship_kind.insert("commit".to_owned());
                                    insert_wire(
                                        &mut pull_request_commit_relationship,
                                        commit.relationship,
                                    );
                                    for attribution in &commit.production {
                                        insert_wire(
                                            &mut production_relationship,
                                            attribution.relationship,
                                        );
                                        insert_wire(&mut fact_confidence, attribution.confidence);
                                        insert_wire(&mut fact_state, attribution.state);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            HelperMessage::Hello(_)
            | HelperMessage::GraphKeyDeletionPrepared(_)
            | HelperMessage::GraphKeyDeleted(_)
            | HelperMessage::JournalSynced(_)
            | HelperMessage::OutputInventoryBegan(_)
            | HelperMessage::OutputPageMaterialized(_)
            | HelperMessage::OutputInventoryFinished(_)
            | HelperMessage::OutputProgress(_)
            | HelperMessage::SourceManifestBegan(_)
            | HelperMessage::SourceManifestAdmissionBegan(_)
            | HelperMessage::SourceManifestPageAdmitted(_)
            | HelperMessage::SourceManifestAdmitted(_)
            | HelperMessage::SourcePrepared(_)
            | HelperMessage::SourcePageMaterialized(_)
            | HelperMessage::SourceDeleted(_)
            | HelperMessage::SourceManifestFinished(_)
            | HelperMessage::Error(_) => {
                panic!("{name} is not an operation-specific helper fixture");
            }
        }
    }

    for (name, actual) in [
        ("entitlement_access_kind", entitlement_access_kind),
        ("entitlement_access_state", entitlement_access_state),
        ("entitlement_capability", entitlement_capability),
        ("graph_state", graph_state),
        ("journal_entity_kind", journal_entity_kind),
        ("journal_operation", journal_operation),
        ("journal_sync_mode", journal_sync_mode),
        ("observation_kind", observation_kind),
        ("output_observation_kind", output_observation_kind),
        ("output_outcome", output_outcome),
        ("output_source_availability", output_source_availability),
        ("output_source_disposition", output_source_disposition),
        ("blame_match_kind", blame_match_kind),
        ("blame_target_kind", blame_target_kind),
        ("commit_fact_type", commit_fact_type),
        ("commit_predicate", commit_predicate),
        ("continuation_reason", continuation_reason),
        ("fact_confidence", fact_confidence),
        ("fact_state", fact_state),
        ("production_relationship", production_relationship),
        ("pull_request_action", pull_request_action),
        (
            "pull_request_commit_relationship",
            pull_request_commit_relationship,
        ),
        (
            "pull_request_relationship_kind",
            pull_request_relationship_kind,
        ),
        ("worktree_status", worktree_status),
    ] {
        assert_eq!(
            actual,
            inventory_enum(&value, name),
            "{name} fixture coverage"
        );
    }
}
