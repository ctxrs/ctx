// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use std::io::{Read, Write};
use std::process::{Output, Stdio};
use std::sync::OnceLock;

use serde_json::{Value, json};
use sift::{Compactor, Encoding, restore};

fn compactor() -> &'static Compactor {
    static COMPACTOR: OnceLock<Compactor> = OnceLock::new();
    COMPACTOR.get_or_init(|| Compactor::new().unwrap())
}

fn cli(args: &[&str], input: &[u8]) -> Output {
    let mut child = crate::test_support::command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child
        .wait_with_output()
        .map(crate::test_support::clean)
        .unwrap();
    // Usage errors can close stdin without consuming it.
    let _ = writer.join().unwrap();
    output
}

#[test]
fn exact_counts_never_expand_and_text_restores() {
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    let mut cases = vec![
        String::new(),
        "failure: missing input\n".into(),
        "<|endoftext|> <|fim_prefix|>".into(),
        "event completed: task=7\r\n".repeat(200),
        format!("{}tail", "\"quoted\"\\🦀\n".repeat(100)),
    ];
    // Exercise the tokenizer's long-piece path and Unicode handling without
    // imposing machine-specific timing assertions or changing token boundaries.
    cases.push("a".repeat(200_000));
    cases.push("🦀".repeat(10_000));
    cases.push(format!("{}0{}", "[".repeat(512), "]".repeat(512)));
    for input in cases {
        let result = compactor().compact(&input);
        assert_eq!(result.input_tokens, tokenizer.encode_ordinary(&input).len());
        assert_eq!(
            result.output_tokens,
            tokenizer.encode_ordinary(&result.text).len()
        );
        assert!(result.output_tokens <= result.input_tokens);
        assert_eq!(restore(result.encoding, &result.text).unwrap(), input);
    }
    let result = compactor().compact(&"repeat this complete diagnostic\n".repeat(200));
    assert_eq!(result.encoding, Encoding::TextRunsV1);
}

#[test]
fn json_values_numbers_and_rows_survive_selection() {
    let input = format!("[{}]", (0..100).map(|i| format!(r#"{{ "record_identifier": {i}, "exact_decimal": 123456789012345678901234567890.123456789, "status_description": "ready" }}"#)).collect::<Vec<_>>().join(",\n"));
    let result = compactor().compact(&input);
    let restored = restore(result.encoding, &result.text).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&restored).unwrap(),
        serde_json::from_str::<Value>(&input).unwrap()
    );
    assert!(result.output_tokens < result.input_tokens);
}

#[test]
fn inline_comma_repetition_uses_exact_lossless_symbols() {
    let input = format!(
        "diagnostic: {}\"quoted,a,b\" 雪🦀\r\nfinal\0",
        (0..80)
            .map(|i| format!("source/generated/modules/component_{i}.rs,"))
            .collect::<String>()
    );
    let result = compactor().compact(&input);
    assert_eq!(result.encoding, Encoding::TextSymbolsV1);
    let frame = result
        .text
        .strip_prefix("sift:symbols-v1 substitute each character using this JSON dictionary:\n")
        .unwrap();
    let (dictionary, body) = frame.split_once('\n').unwrap();
    let dictionary: std::collections::HashMap<String, String> =
        serde_json::from_str(dictionary).unwrap();
    assert!(dictionary.len() <= 32);
    assert!(dictionary.keys().all(|key| key.chars().count() == 1));
    // Expand the written grammar independently, visiting original characters only.
    let decoded: String = body
        .chars()
        .map(|c| {
            dictionary
                .get(&c.to_string())
                .cloned()
                .unwrap_or_else(|| c.to_string())
        })
        .collect();
    assert_eq!(decoded.as_bytes(), input.as_bytes());
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(result.input_tokens, tokenizer.encode_ordinary(&input).len());
    assert_eq!(
        result.output_tokens,
        tokenizer.encode_ordinary(&result.text).len()
    );
    assert!(result.output_tokens < result.input_tokens);
    let restored = cli(
        &["restore", "--encoding", "text-symbols-v1"],
        result.text.as_bytes(),
    );
    assert!(restored.status.success(), "{:?}", restored.stderr);
    assert_eq!(restored.stdout, input.as_bytes());
}

#[test]
fn selector_chooses_the_cheapest_complete_candidate() {
    let row = r#"{ "long_field_name": 7, "status": "ready" }"#;
    let input = format!("[\n{}{row}\n]\n", format!("{row},\n").repeat(99));
    // Independently spell out the competing JSON/RLE representations and framing.
    let minified = format!(
        "JSON v1 (all values):\n[{}]",
        vec![r#"{"long_field_name":7,"status":"ready"}"#; 100].join(",")
    );
    let table = format!(
        "JSON rows v1 (each row maps to the columns in order):\n{{\"columns\":[\"long_field_name\",\"status\"],\"rows\":[{}]}}",
        vec![r#"[7,"ready"]"#; 100].join(",")
    );
    let columns = concat!(
        "JSON columns v1: arrays are columns; scalars repeat for all rows\n",
        r#"{"rows":100,"columns":{"long_field_name":7,"status":"ready"}}"#
    )
    .to_owned();
    let plain_json = format!(
        "[{}]",
        vec![r#"{"long_field_name":7,"status":"ready"}"#; 100].join(",")
    );
    let runs = format!(
        "sift:text-runs-v1 counts repeat exact JSON strings; concatenate\n{}",
        json!([
            [1, "[\n"],
            [99, format!("{row},\n")],
            [1, format!("{row}\n]\n")]
        ])
    );
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    let candidates = [
        (Encoding::Raw, &input),
        (Encoding::JsonV1, &minified),
        (Encoding::JsonMinV1, &plain_json),
        (Encoding::JsonRowsV1, &table),
        (Encoding::JsonColumnsV1, &columns),
        (Encoding::TextRunsV1, &runs),
    ];
    let (encoding, text) = candidates
        .into_iter()
        .min_by_key(|(_, text)| tokenizer.encode_ordinary(text).len())
        .unwrap();
    let result = compactor().compact(&input);
    assert_eq!(result.encoding, encoding);
    assert_eq!(&result.text, text);
}

#[test]
fn earlier_line_format_keeps_priority_when_an_early_symbol_frame_ties() {
    let prefix = "source/generated/components/Widget";
    let input: String = (0..64)
        .map(|i| format!("{prefix}{i}.rs details\n"))
        .collect();
    let lines = format!(
        "sift:lines-v1 [N,prefix] then N lines; prepend prefix\n[64,\"{prefix}\"]\n{}",
        (0..64)
            .map(|i| format!("{i}.rs details\n"))
            .collect::<String>()
    );
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(tokenizer.encode_ordinary(&lines).len(), 283);
    // The multi-entry symbol dictionary also costs 283, and is counted first.
    // Its exact contents are an encoder choice; the earlier format must win.
    let result = compactor().compact(&input);
    assert_eq!(result.encoding, Encoding::TextLinesV1);
    assert_eq!(result.text, lines);
    assert_eq!(result.output_tokens, 283);
    assert_eq!(restore(result.encoding, &result.text).unwrap(), input);
}

#[test]
fn plain_bytes_and_explicit_raw_restore() {
    for input in [
        &b"\xff\0\r\n\x80"[..],
        b"failure: bad input\r\n",
        b"sift:text-runs-v1 counts repeat exact JSON strings; concatenate\n[[2,\"x\"]]",
    ] {
        for args in [&["compact"][..], &["restore", "--encoding=raw"][..]] {
            let output = cli(args, input);
            assert!(output.status.success(), "{:?}", output.stderr);
            assert_eq!(output.stdout, input);
        }
    }
    let output = cli(&["restore", "--encoding=text-runs-v1"], b"not encoded");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn prefix_protocol_and_cli_restore_match_independent_expansion() {
    let input: String = (0..100)
        .map(|i| format!("packages/synthetic/components/Widget{i}.rs\r\n"))
        .collect();
    let request = json!({"version": 1, "text": input});
    let response = cli(
        &["compact", "--protocol=json-v1"],
        format!("{request}\n").as_bytes(),
    );
    assert!(response.status.success());
    let result: Value = serde_json::from_slice(&response.stdout).unwrap();
    let header = "sift:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate\n";
    let suffixes: Vec<String> = (0..100).map(|i| format!("{i}.rs\r\n")).collect();
    let expected = format!(
        "{header}{}",
        json!([["packages/synthetic/components/Widget", suffixes]])
    );
    let selected = result["text"].as_str().unwrap();
    let encoding = serde_json::from_value(result["encoding"].clone()).unwrap();
    assert_eq!(restore(encoding, selected).unwrap(), input);
    let expanded: String = suffixes
        .iter()
        .map(|suffix| format!("packages/synthetic/components/Widget{suffix}"))
        .collect();
    assert_eq!(expanded, input);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        result["input_tokens"],
        tokenizer.encode_ordinary(&input).len()
    );
    assert_eq!(
        result["output_tokens"],
        tokenizer.encode_ordinary(selected).len()
    );
    assert!(
        tokenizer.encode_ordinary(selected).len() <= tokenizer.encode_ordinary(&expected).len()
    );
    assert!(tokenizer.encode_ordinary(&expected).len() < tokenizer.encode_ordinary(&input).len());
    let restored = cli(
        &["restore", "--encoding", "text-prefixes-v1"],
        expected.as_bytes(),
    );
    assert!(restored.status.success());
    assert_eq!(restored.stdout, input.as_bytes());
}

#[test]
fn literal_lines_protocol_matches_independent_expansion_and_counts() {
    let prefix = "~/项目/🦀/shared/packages/very-long-component/";
    let suffixes: String = (0..100).map(|i| format!("{i}\r\n")).collect();
    let input: String = (0..100).map(|i| format!("{prefix}{i}\r\n")).collect();
    let expected = format!(
        "sift:lines-v1 [N,prefix] then N lines; prepend prefix\n{}\n{suffixes}",
        json!([100, prefix])
    );
    let response = cli(
        &["compact", "--protocol=json-v1"],
        format!("{}\n", json!({"version":1,"text":input})).as_bytes(),
    );
    assert!(response.status.success());
    let result: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(result["encoding"], "text-lines-v1");
    assert_eq!(result["text"], expected);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        result["input_tokens"],
        tokenizer.encode_ordinary(&input).len()
    );
    assert_eq!(
        result["output_tokens"],
        tokenizer.encode_ordinary(&expected).len()
    );
    assert!(result["output_tokens"].as_u64().unwrap() < result["input_tokens"].as_u64().unwrap());
    let restored = cli(
        &["restore", "--encoding", "text-lines-v1"],
        expected.as_bytes(),
    );
    assert!(restored.status.success());
    assert_eq!(restored.stdout, input.as_bytes());
}

#[test]
fn protocol_recovers_from_request_errors_and_preserves_flags() {
    let text = "failed: missing artifact\r\n".repeat(100);
    let request =
        json!({"version":1,"text":text,"is_error":true,"complete":false,"tokenizer":"o200k_base"});
    let input = format!(
        "not json\n{}\n{}\n{}\n{}\n",
        json!({"version":2,"text":"x"}),
        json!({"version":1,"text":"x","tokenizer":"unknown"}),
        request,
        json!({"version":1,"text":"last"})
    );
    let output = cli(&["compact", "--protocol=json-v1"], input.as_bytes());
    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 5);
    for response in &responses[..3] {
        assert!(response["error"].is_string());
        assert!(response.get("text").is_none());
    }
    let result = &responses[3];
    let encoding = serde_json::from_value(result["encoding"].clone()).unwrap();
    assert_eq!(
        restore(encoding, result["text"].as_str().unwrap()).unwrap(),
        text
    );
    assert_eq!(responses[4]["text"], "last");
}

#[test]
fn symbol_protocol_and_legacy_refs_restore_match_independent_expansion() {
    let repeated = "warning: packages/synthetic/components/navigation/LongComponentName.rs: required configuration value unavailable; supply the complete configuration before retrying the operation\r\n";
    let between = "another event\n";
    let last = "separate event\n";
    let input = format!("{repeated}{between}{repeated}{last}{repeated}");
    let request = json!({"version": 1, "text": input});
    let response = cli(
        &["compact", "--protocol=json-v1"],
        format!("{request}\n").as_bytes(),
    );
    assert!(response.status.success());
    let result: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(result["encoding"], "text-symbols-v1");
    let legacy = format!(
        "sift:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n{}",
        json!([repeated, between, 0, last, 0])
    );
    let expected = format!(
        "sift:symbols-v1 substitute each character using this JSON dictionary:\n{}\n§{between}§{last}§",
        json!({"§": repeated})
    );
    assert_eq!(result["text"], expected);
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        result["input_tokens"],
        tokenizer.encode_ordinary(&input).len()
    );
    assert_eq!(
        result["output_tokens"],
        tokenizer.encode_ordinary(&expected).len()
    );
    assert!(tokenizer.encode_ordinary(&expected).len() < tokenizer.encode_ordinary(&legacy).len());
    for (encoding, encoded) in [("text-symbols-v1", expected), ("text-refs-v1", legacy)] {
        let restored = cli(&["restore", "--encoding", encoding], encoded.as_bytes());
        assert!(restored.status.success());
        assert_eq!(restored.stdout, input.as_bytes());
    }
}

#[test]
fn nonadjacent_fragment_candidates_preserve_both_separator_forms() {
    let first = "packages/synthetic/generated/components/navigation/widgets/Widget";
    let second = "assets/synthetic/generated/icons/toolbar/vector/Icon";
    let tail = "final byte: 🦀";
    let tokenizer = tiktoken_rs::o200k_base().unwrap();
    for ending in ["\r\n", "\\r\\n"] {
        let mut input = String::new();
        let mut expected_entries = Vec::new();
        for i in 0..8 {
            let first_suffix = format!("{i}.rs{ending}");
            let second_suffix = format!("{i}.svg{ending}");
            input.push_str(&format!("{first}{first_suffix}{second}{second_suffix}"));
            // Specify the literal-anchor positions independently of the encoder.
            expected_entries.extend([
                if i == 0 { json!(first) } else { json!(0) },
                json!(first_suffix),
                if i == 0 { json!(second) } else { json!(2) },
                json!(if i == 7 {
                    format!("{second_suffix}{tail}")
                } else {
                    second_suffix
                }),
            ]);
        }
        input.push_str(tail);
        let expected = format!(
            "sift:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n{}",
            serde_json::to_string(&expected_entries).unwrap()
        );
        let symbol_body: String = (0..8)
            .map(|i| format!("§{i}.rs{ending}¶{i}.svg{ending}"))
            .collect();
        let symbols = format!(
            "sift:symbols-v1 substitute each character using this JSON dictionary:\n{{\"§\":{},\"¶\":{}}}\n{symbol_body}{tail}",
            serde_json::to_string(first).unwrap(),
            serde_json::to_string(second).unwrap()
        );
        let result = compactor().compact(&input);
        assert_eq!(result.encoding, Encoding::TextSymbolsV1);
        assert_eq!(result.text, symbols);
        assert_eq!(result.input_tokens, tokenizer.encode_ordinary(&input).len());
        assert_eq!(
            result.output_tokens,
            tokenizer.encode_ordinary(&symbols).len()
        );
        assert!(result.output_tokens < result.input_tokens);
        assert!(result.output_tokens < tokenizer.encode_ordinary(&expected).len());
        assert_eq!(
            restore(result.encoding, &symbols).unwrap().as_bytes(),
            input.as_bytes()
        );
        assert_eq!(
            restore(Encoding::TextRefsV1, &expected).unwrap().as_bytes(),
            input.as_bytes()
        );
    }
    let diagnostic = "error: input missing\n";
    let result = compactor().compact(diagnostic);
    assert_eq!(result.encoding, Encoding::Raw);
    assert_eq!(result.text, diagnostic);
}

#[test]
fn protocol_flushes_before_stdin_closes() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;
    let mut child = crate::test_support::command(["compact", "--protocol=json-v1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        crate::test_support::skip_header(&mut reader);
        reader.read_line(&mut line).unwrap();
        let _ = tx.send(line);
    });
    stdin
        .write_all(b"{\"version\":1,\"text\":\"ready\"}\n")
        .unwrap();
    let response = rx.recv_timeout(Duration::from_secs(30));
    if response.is_err() {
        let _ = child.kill();
    }
    drop(stdin);
    let status = child.wait().unwrap();
    reader.join().unwrap();
    assert!(status.success());
    assert_eq!(
        serde_json::from_str::<Value>(&response.unwrap()).unwrap()["text"],
        "ready"
    );
}

#[test]
fn prior_product_headers_remain_explicitly_restorable() {
    let product = String::from_utf8(vec![114, 101, 116, 111, 107]).unwrap();
    let cases = [
        (
            Encoding::TextRunsV1,
            format!(
                "{product}:text-runs-v1 counts repeat exact JSON strings; concatenate\n[[2,\"x\"]]"
            ),
            "xx",
        ),
        (
            Encoding::TextPrefixesV1,
            format!(
                "{product}:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate\n[\"x\"]"
            ),
            "x",
        ),
        (
            Encoding::TextRefsV1,
            format!(
                "{product}:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n[\"x\",0]"
            ),
            "xx",
        ),
        (
            Encoding::TextLinesV1,
            format!("{product}:lines-v1 [N,prefix] then N lines; prepend prefix\n[1,\"\"]\nx"),
            "x",
        ),
        (
            Encoding::TextSymbolsV1,
            format!(
                "{product}:symbols-v1 substitute each character using this JSON dictionary:\n{{\"§\":\"x\"}}\n§"
            ),
            "x",
        ),
    ];
    for (encoding, frame, expected) in cases {
        assert_eq!(restore(encoding, &frame).unwrap(), expected);
    }
}

#[test]
fn closed_output_is_quiet_and_successful() {
    let mut child = crate::test_support::command(["restore", "--encoding", "raw"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    crate::test_support::close_stdout(&mut child);
    let _ = child.stdin.take().unwrap().write_all(b"a complete message");
    assert!(child.wait().unwrap().success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.is_empty());
}
