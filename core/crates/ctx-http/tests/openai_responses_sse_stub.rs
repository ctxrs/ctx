use std::path::PathBuf;

mod common;

use serde_json::json;

use common::openai_responses_stub::{
    load_fixture, parse_sse_events, spawn_openai_responses_sse_stub,
};

fn fixture_dir() -> PathBuf {
    common::resolve_manifest_dir().join("tests/fixtures")
}

#[tokio::test]
async fn responses_sse_fixture_tool_flow() {
    let tool_call_fixture = load_fixture(fixture_dir().join("openai_responses_tool_call.sse"));
    let final_fixture = load_fixture(fixture_dir().join("openai_responses_final.sse"));

    let stub = spawn_openai_responses_sse_stub(vec![tool_call_fixture, final_fixture]).await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/responses", stub.base_url);

    let prompt = "What's the weather in Boston?";
    let request1 = json!({
        "model": "gpt-test",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": prompt}]
        }]
    });

    let resp1 = client.post(&url).json(&request1).send().await.unwrap();
    assert!(resp1.status().is_success());
    let body1 = resp1.text().await.unwrap();
    let events1 = parse_sse_events(&body1);
    let call_event = events1
        .iter()
        .find(|ev| {
            ev["type"] == "response.output_item.done" && ev["item"]["type"] == "function_call"
        })
        .expect("missing function_call event");
    let call_id = call_event["item"]["call_id"]
        .as_str()
        .expect("missing call_id");

    let tool_output = "72F and sunny";
    let request2 = json!({
        "model": "gpt-test",
        "input": [{
            "type": "function_call_output",
            "call_id": call_id,
            "output": tool_output
        }]
    });

    let resp2 = client.post(&url).json(&request2).send().await.unwrap();
    assert!(resp2.status().is_success());
    let body2 = resp2.text().await.unwrap();
    let events2 = parse_sse_events(&body2);
    let final_event = events2
        .iter()
        .find(|ev| ev["type"] == "response.output_item.done" && ev["item"]["type"] == "message")
        .expect("missing assistant message event");
    assert_eq!(
        final_event["item"]["content"][0]["text"],
        "It is 72F and sunny."
    );

    let requests = stub.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["input"][0]["type"], "message");
    assert_eq!(requests[0]["input"][0]["role"], "user");
    assert_eq!(requests[0]["input"][0]["content"][0]["text"], prompt);
    assert_eq!(requests[1]["input"][0]["type"], "function_call_output");
    assert_eq!(requests[1]["input"][0]["call_id"], call_id);
    assert_eq!(requests[1]["input"][0]["output"], tool_output);
}
