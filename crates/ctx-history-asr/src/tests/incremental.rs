use super::*;
use std::io::Write;
#[test]
fn folded_growth_keeps_usage_and_tool_record_ids() {
    let (_dir, options) = fixture(
        Provider::Claude,
        &[
            json!({"type":"user","uuid":"u","sessionId":"test","message":{"content":"hi"}}),
            json!({"type":"assistant","uuid":"a","parentUuid":"u","sessionId":"test","message":{"id":"response","content":[{"type":"text","text":"first"}],"usage":{"output_tokens":1}}}),
        ],
    );
    export(&options).unwrap();
    let before = records(&options);
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&options.inputs[0].path)
        .unwrap();
    writeln!(f,"{}",json!({"type":"assistant","uuid":"b","parentUuid":"a","sessionId":"test","message":{"id":"response","content":[{"type":"tool_use","id":"call","name":"Read","input":{"file_path":"a"}}],"usage":{"output_tokens":2}}})).unwrap();
    let result = export(&options).unwrap();
    assert_eq!(result.snapshots, 0);
    let after = records(&options);
    assert_eq!(&after[..before.len()], &before);
    assert!(after
        .iter()
        .any(|r| r["type"] == "message" && r["supersedes"].is_string()));
    assert!(after
        .iter()
        .any(|r| r["type"] == "inference" && r["supersedes"].is_string()));
    assert_eq!(export(&options).unwrap().unchanged, 1);
    for r in before
        .iter()
        .filter(|r| r["type"] == "inference" || r["type"] == "message")
    {
        let key = if r["type"] == "inference" {
            "inferenceId"
        } else {
            "messageId"
        };
        let new = after
            .iter()
            .find(|n| n["type"] == r["type"] && n[key] == r[key])
            .unwrap();
        assert_eq!(r["id"], new["id"]);
        assert_eq!(r["seq"], new["seq"]);
    }
    assert_eq!(after.iter().filter(|r| r["type"] == "tool_call").count(), 1);
}
#[test]
fn torn_output_tail_is_truncated_before_resuming() {
    let (_dir, options) = fixture(
        Provider::Codex,
        &[json!({"type":"session_meta","payload":{"id":"test","source":"cli"}})],
    );
    export(&options).unwrap();
    let before = records(&options);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(options.out.join("bundle.json")).unwrap()).unwrap();
    let p = options
        .out
        .join(manifest["sessions"][0]["path"].as_str().unwrap());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"{\"torn")
        .unwrap();
    export(&options).unwrap();
    assert_eq!(records(&options), before);
    assert!(std::fs::read(&p).unwrap().ends_with(b"\n"));
}
#[test]
fn revert_segments_retain_abandoned_suffix_and_append_stable_ids() {
    let owner = "00000000-0000-4000-8000-000000000001";
    let segment = "00000000-0000-4000-8000-000000000002";
    let (dir, mut options) = fixture(
        Provider::Codex,
        &[
            json!({"type":"session_meta","ordinal":0,"payload":{"id":owner,"source":"cli","timestamp":"2026-01-01T00:00:00Z"}}),
            json!({"type":"response_item","ordinal":1,"payload":{"type":"message","id":"shared","role":"user","content":[{"type":"input_text","text":"shared"}]}}),
            json!({"type":"response_item","ordinal":2,"payload":{"type":"message","id":"abandoned","role":"assistant","content":[{"type":"output_text","text":"abandoned"}]}}),
        ],
    );
    export(&options).unwrap();
    let before = records(&options);
    let next = dir.path().join(format!(
        "rollout-2026-01-02T00-00-00-{owner}_{segment}.jsonl"
    ));
    std::fs::write(&next,format!("{}\n{}\n",json!({"type":"session_meta","ordinal":2,"payload":{"id":owner,"source":"cli","timestamp":"2026-01-02T00:00:00Z","history_base":{"thread_id":owner,"end_ordinal_exclusive":2}}}),json!({"type":"response_item","ordinal":3,"payload":{"type":"message","id":"replacement","role":"assistant","content":[{"type":"output_text","text":"replacement"}]}}))).unwrap();
    options.inputs.push(Input {
        provider: Provider::Codex,
        path: next,
    });
    export(&options).unwrap();
    let after = records(&options);
    assert_eq!(&after[..before.len()], &before);
    let shared = after.iter().find(|r| r["messageId"] == "shared").unwrap();
    let rewind = after.iter().find(|r| r["type"] == "rewind").unwrap();
    assert_eq!(rewind["to"], shared["id"]);
    assert!(after.iter().any(|r| r["messageId"] == "abandoned"));
    assert!(after.iter().any(|r| r["messageId"] == "replacement"));
    assert_eq!(export(&options).unwrap().unchanged, 1);
}
#[test]
fn large_fold_spills_content_but_keeps_late_tool_calls() {
    let mut rows =
        vec![json!({"type":"user","uuid":"u","sessionId":"test","message":{"content":"hi"}})];
    for n in 0..5 {
        rows.push(json!({"type":"assistant","uuid":format!("a{n}"),"sessionId":"test","message":{"id":"response","content":[{"type":"text","text":"x".repeat(500)}]}}));
    }
    rows.push(json!({"type":"assistant","uuid":"tool","sessionId":"test","message":{"id":"response","content":[{"type":"tool_use","id":"call","name":"Read","input":{}}]}}));
    let (_dir, mut options) = fixture(Provider::Claude, &rows);
    options.max_record_bytes = 1024;
    export(&options).unwrap();
    let result = records(&options);
    let message = result
        .iter()
        .find(|r| r["messageId"] == "response")
        .unwrap();
    assert_eq!(message["capture"]["content"]["state"], "external");
    let digest = message["capture"]["content"]["blob"]["blob"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let blob: Value = serde_json::from_slice(
        &std::fs::read(options.out.join("blobs/sha256").join(digest)).unwrap(),
    )
    .unwrap();
    assert_eq!(blob.as_array().unwrap().len(), 6);
    assert!(result
        .iter()
        .any(|r| r["type"] == "tool_call" && r["callId"] == "call"));
}

#[cfg(unix)]
#[test]
fn transcript_file_aliases_reuse_the_target_and_report_missing_targets() {
    let (dir, mut options) = fixture(
        Provider::Codex,
        &[json!({"type":"session_meta","payload":{"id":"test","source":"cli"}})],
    );
    let aliases = dir.path().join("aliases");
    std::fs::create_dir(&aliases).unwrap();
    std::os::unix::fs::symlink(&options.inputs[0].path, aliases.join("alias.jsonl")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("missing"), aliases.join("missing.jsonl")).unwrap();
    options.inputs.push(Input {
        provider: Provider::Codex,
        path: aliases,
    });
    let summary = export(&options).unwrap();
    assert_eq!(summary.sessions, 1);
    assert_eq!(summary.unidentified_sources, 1);
    assert_eq!(
        records(&options)
            .iter()
            .filter(|r| r["type"] == "com.openai.codex/session_meta")
            .count(),
        1
    );
    assert_eq!(export(&options).unwrap().unchanged, 1);
}
