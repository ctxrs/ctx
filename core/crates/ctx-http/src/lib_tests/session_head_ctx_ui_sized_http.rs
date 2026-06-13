use std::time::{Duration, Instant};

use super::*;
use fixtures::CtxUiSizedHeadFixture;

mod fixtures;

#[tokio::test]
async fn ctx_ui_sized_active_session_head_recovery_is_bounded() {
    const TURN_COUNT: i64 = 68;
    const HEAD_LIMIT: i64 = 60;
    const TOOL_COUNT: i64 = 16_320;
    const MESSAGE_COUNT: i64 = 5_880;
    const EVENT_COUNT: i64 = 65_600;
    const TOOL_OUTPUT_BYTES: usize = 4 * 1024;
    const TOOL_SUMMARY_LIMIT: usize = 96;
    const HEAD_BYTE_LIMIT: usize = 256_000;
    let head_recovery_budget = Duration::from_secs(2);
    let step_timeout = Duration::from_secs(180);

    let fixture = CtxUiSizedHeadFixture::new().await;
    let (workspace, task, session) = fixture.create_default_session().await;

    let stats = fixture
        .daemon()
        .seed_ctx_ui_sized_session_head_fixture_for_test(
            workspace.id,
            session.id,
            task.id,
            CtxUiSizedHeadSeedSpec {
                turn_count: TURN_COUNT,
                message_count: MESSAGE_COUNT,
                tool_count: TOOL_COUNT,
                event_count: EVENT_COUNT,
                tool_output_bytes: TOOL_OUTPUT_BYTES,
            },
            step_timeout,
        )
        .await
        .unwrap();
    assert_eq!(stats.event_count, EVENT_COUNT);
    assert_eq!(stats.tool_count, TOOL_COUNT);
    assert_eq!(stats.message_count, MESSAGE_COUNT);

    let probe = fixture
        .daemon()
        .ctx_ui_sized_recent_tool_summary_probe_for_test(session.id, HEAD_LIMIT, TOOL_SUMMARY_LIMIT)
        .await
        .unwrap();
    assert_eq!(
        probe.bounded_tool_count,
        TOOL_SUMMARY_LIMIT + 1,
        "tool-summary recovery must fetch exactly one sentinel row past the visible limit"
    );
    assert!(
        probe.oldest_loaded_order_seq >= TOOL_COUNT - (TOOL_SUMMARY_LIMIT as i64 + 1),
        "tool-summary recovery must seek into the latest hot rows, not load the long tail"
    );

    let started = Instant::now();
    let (head_status, head_body): (StatusCode, serde_json::Value) = tokio::time::timeout(
        step_timeout,
        fixture.json_request(
            Method::GET,
            format!(
                "/api/sessions/{}/head?limit={HEAD_LIMIT}&include_events=true",
                session.id.0,
            ),
            None,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out requesting ctx-ui sized active session head"));
    let elapsed = started.elapsed();

    assert_eq!(head_status, StatusCode::OK, "{head_body:#?}");
    eprintln!(
        "ctx-ui-sized-head elapsed_ms={} events={} tools={} messages={} response_bytes={}",
        elapsed.as_millis(),
        stats.event_count,
        stats.tool_count,
        stats.message_count,
        serde_json::to_vec(&head_body).unwrap().len(),
    );
    assert!(
        elapsed <= head_recovery_budget,
        "ctx-ui-sized /head recovery took {}ms, over {}ms budget",
        elapsed.as_millis(),
        head_recovery_budget.as_millis()
    );
    assert!(head_body["has_more_turns"].as_bool().unwrap_or(false));
    assert!(
        head_body["turns"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default()
            <= HEAD_LIMIT as usize,
        "head turns must stay bounded"
    );
    assert!(
        head_body["tool_summaries"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default()
            <= TOOL_SUMMARY_LIMIT,
        "head tool summaries must stay bounded"
    );
    assert_eq!(
        head_body["head_window"]["truncated"],
        json!(true),
        "ctx-ui sized head should report that long-tail state was intentionally truncated"
    );
    assert!(
        head_body["head_window"]["bytes"]
            .as_i64()
            .unwrap_or(i64::MAX)
            <= HEAD_BYTE_LIMIT as i64,
        "head window bytes must stay within the bounded recovery policy"
    );
    let latest_visible_tool_count = head_body["tool_summaries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|tool| {
            tool["turn_id"].as_str() == Some(probe.latest_turn_id.0.to_string().as_str())
        })
        .count();
    assert!(
        latest_visible_tool_count > 0,
        "head must rebuild missing tool summaries for the latest visible running turn"
    );
}
