use serde_json::json;
#[test]
fn hermes_unknown_cost_defaults_do_not_replace_recorded_estimates() {
    let mut row = json!({"api_call_count":1,"estimated_cost_usd":0.0,"actual_cost_usd":0.0,"cost_status":"unknown"});
    assert!(
        super::common::summary(&row, false, "session_model_usage:example")
            .unwrap()
            .get("cost")
            .is_none()
    );
    row["estimated_cost_usd"] = json!(1.25);
    row["cost_status"] = json!("estimated");
    let estimated = super::common::summary(&row, false, "session_model_usage:example").unwrap();
    assert_eq!(estimated["cost"]["amount"], 1.25);
    assert_eq!(estimated["cost"]["source"], "harness_estimated");
    row["cost_status"] = json!("actual");
    row["cost_source"] = json!("provider_cost_api");
    let actual = super::common::summary(&row, false, "session_model_usage:example").unwrap();
    assert_eq!(actual["cost"]["amount"], 0.0);
    assert_eq!(actual["cost"]["source"], "provider_reported");
}
