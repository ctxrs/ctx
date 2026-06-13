use super::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Map, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use crate::crp::policy::parse_crp_slash_command;
use crate::crp::protocol::CrpEventEnvelope;

const ITERATIONS: usize = 200;
const MAX_DEPTH: u8 = 3;

fn random_string(rng: &mut StdRng, max_len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_- /.:";
    let len = rng.gen_range(1..=max_len.max(1));
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn random_value(rng: &mut StdRng, depth: u8) -> Value {
    if depth == 0 {
        return match rng.gen_range(0..5) {
            0 => Value::String(random_string(rng, 16)),
            1 => Value::Number((rng.gen_range(0..=9999_u64)).into()),
            2 => Value::Bool(rng.gen_bool(0.5)),
            3 => Value::Null,
            _ => json!({"k": "v"}),
        };
    }

    match rng.gen_range(0..6) {
        0 => Value::String(random_string(rng, 24)),
        1 => Value::Array(
            (0..rng.gen_range(0..=4))
                .map(|_| random_value(rng, depth - 1))
                .collect(),
        ),
        2 => {
            let mut map = Map::new();
            for _ in 0..rng.gen_range(0..=4) {
                map.insert(random_string(rng, 12), random_value(rng, depth - 1));
            }
            Value::Object(map)
        }
        3 => Value::Bool(rng.gen_bool(0.5)),
        4 => Value::Number((rng.gen_range(0..=9999_u64)).into()),
        _ => Value::Null,
    }
}

fn random_event_type(rng: &mut StdRng) -> &'static str {
    const TYPES: &[&str] = &[
        "session.opened",
        "turn.started",
        "message.delta",
        "message.final",
        "reasoning.summary",
        "reasoning.trace",
        "reasoning.trace.final",
        "tool.started",
        "tool.output_delta",
        "tool.completed",
        "turn.completed",
        "session.notice",
        "session.gap",
    ];
    TYPES[rng.gen_range(0..TYPES.len())]
}

fn valid_message_delta(seq: u64) -> String {
    json!({
        "v": 1,
        "seq": seq,
        "channel": "control",
        "type": "message.delta",
        "session_id": "s",
        "turn_id": "t",
        "message_id": "m",
        "delta": "hello",
    })
    .to_string()
}

fn random_envelope_json(rng: &mut StdRng) -> String {
    if rng.gen_bool(0.15) {
        return random_value(rng, MAX_DEPTH).to_string();
    }

    let mut obj = Map::new();
    obj.insert(
        "seq".to_string(),
        Value::Number((rng.gen_range(0..=10_000_u64)).into()),
    );
    obj.insert(
        "channel".to_string(),
        Value::String(if rng.gen_bool(0.5) {
            "control".to_string()
        } else {
            "data".to_string()
        }),
    );
    obj.insert(
        "type".to_string(),
        Value::String(random_event_type(rng).to_string()),
    );
    obj.insert(
        "session_id".to_string(),
        Value::String(random_string(rng, 8)),
    );
    obj.insert("turn_id".to_string(), Value::String(random_string(rng, 8)));
    obj.insert(
        "message_id".to_string(),
        Value::String(random_string(rng, 8)),
    );
    obj.insert("delta".to_string(), Value::String(random_string(rng, 12)));
    obj.insert("content".to_string(), Value::String(random_string(rng, 16)));
    obj.insert(
        "summary_index".to_string(),
        Value::Number((rng.gen_range(0..=8_u64)).into()),
    );
    obj.insert("text".to_string(), Value::String(random_string(rng, 18)));
    obj.insert("chunk".to_string(), Value::String(random_string(rng, 18)));
    obj.insert(
        "tool_call_id".to_string(),
        Value::String(random_string(rng, 8)),
    );
    obj.insert(
        "tool_name".to_string(),
        Value::String(random_string(rng, 10)),
    );
    obj.insert(
        "status".to_string(),
        Value::String(if rng.gen_bool(0.5) {
            "success".to_string()
        } else {
            "error".to_string()
        }),
    );
    obj.insert("reason".to_string(), Value::String(random_string(rng, 12)));
    if rng.gen_bool(0.5) {
        obj.insert("details".to_string(), random_value(rng, MAX_DEPTH - 1));
    }
    Value::Object(obj).to_string()
}

fn try_parse_and_map(line: &str) {
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        serde_json::from_str::<CrpEventEnvelope>(line)
    }));
    assert!(parsed.is_ok(), "panic while parsing CRP envelope");
    if let Ok(env) = parsed.expect("parse panic already checked") {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();
        let mapped = catch_unwind(AssertUnwindSafe(|| {
            map_crp_event(
                env.event,
                env.channel,
                env.seq,
                &mut tool_output_cache,
                &mut tool_input_cache,
            )
        }));
        assert!(mapped.is_ok(), "panic while mapping CRP event");
    }
}

#[test]
fn fuzz_crp_envelope_parsing_and_mapping_do_not_panic() {
    let mut rng = StdRng::seed_from_u64(0xAC1F_2026);

    for idx in 0..ITERATIONS {
        let line = if idx % 10 == 0 {
            valid_message_delta((idx as u64) + 1)
        } else {
            random_envelope_json(&mut rng)
        };
        try_parse_and_map(&line);

        let random_slash = if rng.gen_bool(0.5) {
            format!(
                "/{} {}",
                random_string(&mut rng, 10),
                random_string(&mut rng, 16)
            )
        } else {
            random_string(&mut rng, 24)
        };
        let slash = catch_unwind(AssertUnwindSafe(|| parse_crp_slash_command(&random_slash)));
        assert!(slash.is_ok(), "panic while parsing slash command");
    }
}

#[test]
fn fuzz_acp_corpus_replay_does_not_panic() {
    let corpus_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/acp");
    assert!(
        corpus_dir.is_dir(),
        "missing corpus dir: {}",
        corpus_dir.display()
    );

    let mut files = std::fs::read_dir(&corpus_dir)
        .expect("read_dir failed")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    files.sort();
    assert!(!files.is_empty(), "expected at least one corpus file");

    for file in files {
        let body = std::fs::read_to_string(&file).expect("read corpus file");
        for raw in body.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            try_parse_and_map(line);
        }
    }
}
