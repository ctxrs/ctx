use super::support;
use crate::conversation::{self, Artifact, ConversationManifest};
use crate::ProjectionBinding;
use ctx_history_core::{ActivityTextCapture, CoreRecord};
use serde_json::{json, Value};

fn project(kind: &str, payload: Value, artifact: Option<&[u8]>) -> CoreRecord {
    let source = support::source(1);
    conversation::project(ProjectionBinding { source: &source, native_session_id: "conversation" },
        &ConversationManifest { schema_version: 4, id: "conversation".into(), workspace_root: "/fixture".into(), subagent_child: false }, None,
        &serde_json::to_vec(&json!({"schema_version":2,"seq":1,"timestamp_ms":1700000000000_i64,"event":{kind:payload}})).unwrap(),
        &mut |_, _| Ok(match artifact { Some(bytes) => Artifact::Present(bytes.to_vec()), None => Artifact::Unavailable })).unwrap()
}

#[test]
fn all_conversation_variants_keep_meaningful_content_and_metadata() {
    for (kind, payload, needle) in [
        ("user", json!({"text":"user request"}), "user request"),
        ("steering", json!({"text":"steer now"}), "steer now"),
        (
            "assistant",
            json!({"text":"answer","provider_replay":{"source":{"provider":"gateway","model":"fixture"},"parts_json":"[{\"type\":\"reasoning\",\"text\":\"retained thought\"}]"}}),
            "retained thought",
        ),
        (
            "tool_call",
            json!({"call_id":"c","tool_name":"read","arguments_json":"{\"path\":\"file.txt\"}"}),
            "file.txt",
        ),
        (
            "tool_result",
            json!({"call_id":"c","tool_name":"read","status":"success","artifact_ref":"result.txt","preview":"short"}),
            "full retained result",
        ),
        (
            "interrupted",
            json!({"reason":"canceled","partial_text":"partial answer","files":[{"path":"interrupted.txt"}],"turn_summary":{"turn_duration_ms":123}}),
            "partial answer",
        ),
        (
            "turn_completed",
            json!({"files":[{"path":"completed.txt","evidence":"retained evidence"}],"turn_summary":{"turn_duration_ms":456}}),
            "retained evidence",
        ),
        (
            "context_checkpoint",
            json!({"covers_through_seq":1,"summary":"context summary"}),
            "context summary",
        ),
    ] {
        let record = project(kind, payload.clone(), Some(b"full retained result"));
        assert!(
            record
                .content
                .normalized_body
                .as_deref()
                .unwrap_or_default()
                .contains(needle),
            "{kind}: {:?}",
            record.content
        );
        let structured = record.content.structured_content.unwrap();
        if matches!(kind, "interrupted" | "turn_completed") {
            assert_eq!(
                structured["event"][kind]["turn_summary"],
                payload["turn_summary"]
            );
        }
    }
}

#[test]
fn repaired_artifact_keeps_native_identity_without_claiming_preview_is_full() {
    let payload = json!({"call_id":"c","tool_name":"read","status":"success","artifact_ref":"result.txt","preview":"preview"});
    let missing = project("tool_result", payload.clone(), None);
    let repaired = project("tool_result", payload, Some(b"complete repaired content"));
    assert_eq!(missing.event_id, repaired.event_id);
    assert_eq!(
        missing.content.activity.unwrap().result.unwrap().text,
        ActivityTextCapture::Unavailable
    );
    assert_eq!(
        repaired.content.activity.unwrap().result.unwrap().text,
        ActivityTextCapture::NormalizedBody
    );
    assert!(repaired
        .content
        .normalized_body
        .as_deref()
        .unwrap_or_default()
        .contains("complete repaired content"));
}

#[test]
fn command_replay_preserves_framing_binary_and_split_utf8() {
    let mut bytes = b"FXRPLY01".to_vec();
    for (stream, payload) in [
        (0, &b"hello \xe2"[..]),
        (1, &b"warning"[..]),
        (0, &b"\x82\xac"[..]),
    ] {
        bytes.push(stream);
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(payload);
    }
    let replay = crate::conversation_replay::decode(&bytes).unwrap();
    assert_eq!(std::str::from_utf8(&replay.stdout).unwrap(), "hello €");
    assert_eq!(std::str::from_utf8(&replay.stderr).unwrap(), "warning");
    assert_eq!(replay.frames.len(), 3);
    bytes.push(0);
    assert!(crate::conversation_replay::decode(&bytes).is_err());
    assert!(crate::conversation_replay::decode(b"not a text artifact").is_err());
}

fn replay_bytes(byte: u8, frames: usize) -> Vec<u8> {
    let mut bytes = b"FXRPLY01".to_vec();
    for _ in 0..frames {
        bytes.push(0);
        bytes.extend_from_slice(&(1024_u64 * 1024).to_le_bytes());
        bytes.resize(bytes.len() + 1024 * 1024, byte);
    }
    bytes
}

#[test]
fn replay_frame_limits_match_native_decoder_and_ranges_preserve_split_utf8() {
    assert!(crate::conversation_replay::decode(&replay_bytes(b'a', 1)).is_ok());
    let mut invalid = b"FXRPLY01".to_vec();
    invalid.push(0);
    invalid.extend_from_slice(&0_u64.to_le_bytes());
    assert!(crate::conversation_replay::decode(&invalid).is_err());
    invalid[9..17].copy_from_slice(&(1024_u64 * 1024 + 1).to_le_bytes());
    invalid.resize(17 + 1024 * 1024 + 1, b'a');
    assert!(crate::conversation_replay::decode(&invalid).is_err());
    let mut bytes = b"FXRPLY01".to_vec();
    for (stream, value) in [
        (0, &b"\xe2"[..]),
        (1, &b"warning"[..]),
        (0, &b"\x82\xac"[..]),
    ] {
        bytes.push(stream);
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value);
    }
    let mut body = "existing".to_owned();
    let capture = crate::conversation_replay::capture(&bytes, &mut body).unwrap();
    assert_eq!(body, "existing\n€\nwarning");
    assert_eq!(
        capture["frames"],
        json!([["stdout", 0, 1], ["stderr", 0, 7], ["stdout", 1, 2]])
    );
    assert_eq!(capture["streams"]["stdout"]["byte_start"], 9);
    assert_eq!(capture["streams"]["stdout"]["byte_length"], 3);
}

#[test]
fn aggregate_oversize_replay_is_omitted_without_losing_full_tool_result() {
    let source = support::source(1);
    let manifest = ConversationManifest {
        schema_version: 4,
        id: "conversation".into(),
        workspace_root: "/fixture".into(),
        subagent_child: false,
    };
    for (byte, expected_status) in [(b'a', "present"), (0xff, "omitted")] {
        let replay = replay_bytes(byte, 7);
        let result = vec![
            b'r';
            if byte == 0xff {
                8 * 1024 * 1024
            } else {
                1024 * 1024
            }
        ];
        let record = conversation::project(ProjectionBinding { source:&source,native_session_id:&manifest.id }, &manifest, None,
            &serde_json::to_vec(&json!({"schema_version":2,"seq":1,"timestamp_ms":1,"event":{"tool_result":{"call_id":"c","tool_name":"command","status":"success","artifact_ref":"result.txt","command_replay_ref":"replay.bin"}}})).unwrap(),
            &mut |_, name| Ok(Artifact::Present(if name == "replay.bin" { replay.clone() } else { result.clone() }))).unwrap();
        record.validate_contract().unwrap();
        assert!(record
            .content
            .normalized_body
            .as_ref()
            .unwrap()
            .starts_with(&String::from_utf8(result).unwrap()));
        assert_eq!(
            record.content.structured_content.as_ref().unwrap()["event"]["tool_result"]
                ["command_replay_capture"]["capture_status"],
            expected_status
        );
        if byte == 0xff {
            assert_eq!(
                record.content.normalized_body.as_ref().unwrap().len(),
                8 * 1024 * 1024
            );
        }
    }
}

#[test]
fn malformed_command_replay_and_empty_result_preserve_event_and_capture_status() {
    let record = project(
        "tool_result",
        json!({"call_id":"c","tool_name":"command","status":"success","artifact_ref":"result.txt","command_replay_ref":"replay.bin"}),
        Some(b""),
    );
    assert_eq!(
        record.content.activity.unwrap().result.unwrap().text,
        ActivityTextCapture::Present {
            value: String::new()
        }
    );
    assert_eq!(
        record.content.structured_content.unwrap()["event"]["tool_result"]
            ["command_replay_capture"]["capture_status"],
        "omitted"
    );
}

#[test]
fn large_tool_arguments_are_not_triplicated_into_a_rejected_record() {
    let argument = "a".repeat(6 * 1024 * 1024);
    let raw = serde_json::to_string(&json!({"text":argument})).unwrap();
    let record = project(
        "tool_call",
        json!({"call_id":"c","tool_name":"command","arguments_json":raw}),
        None,
    );
    record.validate_contract().unwrap();
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(raw.as_str())
    );
}
