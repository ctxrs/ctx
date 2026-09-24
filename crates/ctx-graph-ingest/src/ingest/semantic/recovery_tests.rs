use super::*;

#[test]
fn expired_recovery_deadline_never_reserves_or_starts_a_call() {
    let shared = Arc::new(SemanticBudget::new(Some(3), None));
    let recorder = Arc::new(SemanticUsageRecorder::default());
    let s = SemanticOptions {
        provider: Provider::Cli,
        command: Some(CommandAdapter {
            program: "must-not-be-started".into(),
            ..Default::default()
        }),
        runtime_budget: Some(shared.clone()),
        runtime_usage: Some(recorder.clone()),
        ..Default::default()
    };
    let mut budget = RequestBudget {
        calls: 3,
        output: 8192,
        deadline: Instant::now(),
        claude_schema: None,
    };
    let error = request(&s, "Alpha", None, &mut budget).unwrap_err();
    assert!(error.to_string().contains("deadline"));
    assert_eq!(shared.usage().unwrap().calls, 0);
    assert!(recorder.snapshot().unwrap().is_empty());
}

#[test]
fn timeout_recovery_uses_error_types_not_messages() {
    let typed = command_error(super::super::convert::CommandTimeout.into());
    assert!(matches!(
        typed.downcast_ref::<Recovery>(),
        Some(Recovery::Timeout)
    ));
    let prose = command_error(anyhow::anyhow!("converter/provider timed out"));
    assert!(!prose.is::<Recovery>());
    assert!(io_timeout(&std::io::Error::from(
        std::io::ErrorKind::TimedOut
    )));
    assert!(!io_timeout(&std::io::Error::other("timed out")));
}
#[test]
fn aggregate_cli_usage_does_not_claim_a_single_model_or_sum_cache_counts() {
    let s = SemanticOptions {
        provider: Provider::ClaudeCli,
        ..Default::default()
    };
    let mut receipt = unknown_usage(&s);
    read_usage(
        &mut receipt,
        &json!({
            "model":"first-model",
            "modelUsage":{"first-model":{},"second-model":{}},
            "usage":{"input_tokens":0,"output_tokens":2,"cache_read_input_tokens":7},
            "total_cost_usd":-1
        }),
    );
    assert!(receipt.reported_model.is_none());
    assert_eq!(receipt.input_tokens, Some(0));
    assert_eq!(receipt.cache_read_input_tokens, Some(7));
    assert_eq!(receipt.total_tokens, None);
    assert_eq!(receipt.cost_usd, None);
}

#[test]
fn multi_turn_reservations_are_atomic_across_file_and_shared_limits() {
    for (file_calls, file_output, shared_calls, shared_output) in [
        (2, 6144, 3, 6144),
        (3, 4096, 3, 6144),
        (3, 6144, 2, 6144),
        (3, 6144, 3, 4096),
    ] {
        let shared = Arc::new(SemanticBudget::new(Some(shared_calls), Some(shared_output)));
        let s = SemanticOptions {
            runtime_budget: Some(shared.clone()),
            ..Default::default()
        };
        let mut budget = RequestBudget {
            calls: file_calls,
            output: file_output,
            deadline: Instant::now() + Duration::from_secs(60),
            claude_schema: None,
        };
        assert!(reserve_calls(&s, &mut budget, 3).is_err());
        assert_eq!((budget.calls, budget.output), (file_calls, file_output));
        assert_eq!(shared.usage().unwrap().calls, 0);
        assert_eq!(shared.usage().unwrap().reserved_output_tokens, 0);
        // Nearest ordinary operation is still admitted after rejection.
        reserve_calls(&s, &mut budget, 1).unwrap();
        assert_eq!(
            (budget.calls, budget.output),
            (file_calls - 1, file_output - 2048)
        );
        assert_eq!(shared.usage().unwrap().calls, 1);
        assert_eq!(shared.usage().unwrap().reserved_output_tokens, 2048);
    }
    let shared = Arc::new(SemanticBudget::new(Some(3), Some(6144)));
    let s = SemanticOptions {
        runtime_budget: Some(shared.clone()),
        ..Default::default()
    };
    let mut budget = RequestBudget {
        calls: 3,
        output: 6144,
        deadline: Instant::now() + Duration::from_secs(60),
        claude_schema: None,
    };
    reserve_calls(&s, &mut budget, 3).unwrap();
    assert_eq!((budget.calls, budget.output), (0, 0));
    assert!(reserve_calls(&s, &mut budget, 1).is_err());
    assert_eq!((budget.calls, budget.output), (0, 0));
    assert_eq!(shared.usage().unwrap().calls, 3);
    assert_eq!(shared.usage().unwrap().reserved_output_tokens, 6144);
}
