use super::*;

fn receipt() -> crate::ProviderUsage {
    crate::ProviderUsage {
        provider: crate::Provider::OpenAi,
        requested_model: "private-model-sentinel".into(),
        reported_model: Some("private-reported-model-sentinel".into()),
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        reasoning_tokens: None,
        cost_usd: None,
    }
}

#[test]
fn missing_usage_is_not_zero_and_reservations_are_not_receipts() {
    let mut facts = GraphSemanticFacts::default();
    facts.record(None, None);
    assert_eq!(facts.receipts, None);
    assert_eq!(facts.input.known_sum, None);
    let mut known = receipt();
    known.input_tokens = Some(0);
    known.output_tokens = Some(3);
    known.cost_usd = Some(0.0);
    facts.record(
        Some(crate::SemanticUsage {
            calls: 4,
            reserved_output_tokens: 400,
        }),
        Some(&[receipt(), known]),
    );
    assert_eq!(facts.reserved_generations, Some(4));
    assert_eq!(facts.receipts, Some(2));
    assert_eq!(
        facts.input,
        GraphTokenUsage {
            known_sum: Some(0),
            reporting_receipts: 1
        }
    );
    assert_eq!(facts.output.known_sum, Some(3));
    assert_eq!(facts.total.known_sum, None);
    assert_eq!(facts.known_cost_usd, Some(0.0));
    assert_eq!(facts.cost_reporting_receipts, 1);
    assert!(!format!("{facts:?}").contains("sentinel"));
    facts.record(None, Some(&[]));
    assert_eq!(facts.receipts, Some(0));
    assert_eq!(facts.input.known_sum, None);
    assert_eq!(facts.known_cost_usd, None);
}

#[test]
fn overflow_and_invalid_cost_remain_unknown() {
    let mut first = receipt();
    first.input_tokens = Some(u64::MAX);
    first.cost_usd = Some(f64::NAN);
    let mut second = receipt();
    second.input_tokens = Some(1);
    second.cost_usd = Some(-1.0);
    let mut facts = GraphSemanticFacts::default();
    facts.record(None, Some(&[first, second]));
    assert_eq!(facts.input.known_sum, None);
    assert_eq!(facts.input.reporting_receipts, 2);
    assert_eq!(facts.known_cost_usd, None);
    assert_eq!(facts.cost_reporting_receipts, 0);
    facts.record(None, None);
    assert_eq!(facts.input.reporting_receipts, 0);
    assert_eq!(facts.receipts, None);
}

#[test]
fn new_invocation_does_not_claim_work_or_delivery() {
    let facts = GraphObservation::new(GraphOperation::Index, GraphInvocation::Cli);
    assert_eq!(facts.duration, None);
    assert_eq!(facts.index, None);
    assert_eq!(facts.nodes, None);
    assert_eq!(facts.parsed_files, None);
    assert_eq!(facts.execution_succeeded, None);
    assert_eq!(facts.output_served, None);
    assert_eq!(facts.output_boundary, GraphOutputBoundary::Unobserved);
}
