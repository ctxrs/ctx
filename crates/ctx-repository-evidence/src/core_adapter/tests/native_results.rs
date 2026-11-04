use super::*;

#[test]
fn native_error_evidence_controls_real_commit_outcomes() {
    let (_temporary, repository, _) = initialize_repository();
    let output = run_git(
        &repository,
        &["commit", "--allow-empty", "-m", "native result"],
    );
    let command = "git commit --allow-empty -m 'native result'";
    let workdir = repository.to_string_lossy();
    for provider in ["pi", "claude"] {
        let error_field = if provider == "pi" {
            "isError"
        } else {
            "is_error"
        };
        let mut envelope = serde_json::json!({"content":output});
        envelope[error_field] = Value::Bool(false);
        let core = serde_json::json!({"entry":{"message":envelope}});
        let present = |value| ActivityJsonCapture::Present { value };
        let mut failed = envelope.clone();
        failed[error_field] = Value::Bool(true);
        let mut malformed = envelope.clone();
        malformed[error_field] = Value::String("false".to_owned());
        for (name, capture, retained, expected) in [
            ("complete", present(envelope.clone()), None, true),
            (
                "absent_error_field",
                present(serde_json::json!({"content":output})),
                None,
                true,
            ),
            ("explicit_error", present(failed.clone()), None, false),
            ("malformed_error", present(malformed), None, false),
            (
                "absent_activity",
                ActivityJsonCapture::Absent,
                Some(core.clone()),
                provider == "pi",
            ),
            (
                "native_error",
                ActivityJsonCapture::Absent,
                Some(serde_json::json!({"entry":{"message":failed}})),
                false,
            ),
            (
                "native_no_error_field",
                ActivityJsonCapture::Absent,
                Some(serde_json::json!({"entry":{"message":{"content":output}}})),
                provider == "pi",
            ),
            (
                "missing_native_envelope",
                ActivityJsonCapture::Absent,
                None,
                false,
            ),
            (
                "unavailable",
                ActivityJsonCapture::Unavailable,
                Some(core.clone()),
                false,
            ),
            (
                "omitted_9_mib",
                ActivityJsonCapture::Omitted {
                    reason: "size_limit".to_owned(),
                    observed_encoded_bytes: Some(9 * 1024 * 1024),
                },
                Some(core.clone()),
                false,
            ),
        ] {
            let invocation = ActivityInvocation {
                protocol: None,
                server: None,
                tool: "bash".to_owned(),
                arguments: present(serde_json::json!({"command":command})),
                started_at_unix_ms: None,
            };
            let facts = vec![
                fact(LiteralFactKind::SessionCwd, &workdir),
                fact(LiteralFactKind::Command, command),
            ];
            let mut call = codex_record(1, "call", Some(invocation), None, facts.clone());
            let mut result = codex_record(
                2,
                "call",
                None,
                Some(ActivityResult {
                    // The native envelope must still veto an optimistic status.
                    status: Some("success".to_owned()),
                    completed_at_unix_ms: Some(1_787_126_404_000),
                    duration_ns: None,
                    text: ActivityTextCapture::Present {
                        value: output.clone(),
                    },
                    structured_content: capture,
                }),
                facts,
            );
            let source = SourceKey::derive_provider_native(
                provider,
                "native-results",
                "test-v1",
                1,
                "session",
                TypedKey::Utf8("primary".to_owned()),
            )
            .unwrap();
            call.source = source.clone();
            result.source = source;
            result.content.structured_content = retained;
            for (call_admitted, result_admitted) in
                [(true, true), (false, true), (true, false), (false, false)]
            {
                for reversed in [false, true] {
                    let mut adapter = CoreRepositoryEvidenceAdapter::default();
                    adapter.begin_source();
                    let records = if reversed {
                        [&result, &call]
                    } else {
                        [&call, &result]
                    };
                    let outcomes = records
                        .into_iter()
                        .enumerate()
                        .flat_map(|(index, record)| {
                            adapter
                                .evaluate_record_with_outcome_admission(
                                    record,
                                    &format!("{:064x}", index + 1),
                                    if record
                                        .content
                                        .activity
                                        .as_ref()
                                        .unwrap()
                                        .invocation
                                        .is_some()
                                    {
                                        call_admitted
                                    } else {
                                        result_admitted
                                    },
                                )
                                .repository_vcs_observations
                        })
                        .filter(|observation| {
                            matches!(observation.kind, RepositoryVcsObservationKind::Outcome(_))
                        })
                        .count();
                    assert_eq!(
                        outcomes,
                        usize::from(expected && call_admitted && result_admitted),
                        "{provider} {name} reversed={reversed} call={call_admitted} result={result_admitted}"
                    );
                }
            }
        }
    }
}
