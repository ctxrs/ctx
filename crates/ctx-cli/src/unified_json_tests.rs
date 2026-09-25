//! Keep these canaries in the final executable: engine dependencies participate
//! in Cargo feature unification even when a history-only unit target passes.

#[test]
fn history_json_preserves_released_value_bytes() {
    // Frozen serde_json 1.0.151 default-parser behavior: sorted object keys,
    // f64 decimals/exponents, and floating negative zero. 10^20 exceeds u64
    // but is exactly representable as f64; its published formatter emits 1e+20.
    // Expected bytes must not be regenerated with the currently linked parser.
    let cases: &[(&str, &[u8])] = &[
        (r#"{"z":0,"a":1}"#, br#"{"a":1,"z":0}"#),
        (
            r#"{"z":{"z":0,"a":1},"a":[{"z":2,"a":3}]}"#,
            br#"{"a":[{"a":3,"z":2}],"z":{"a":1,"z":0}}"#,
        ),
        ("1.00", b"1.0"),
        ("1e0", b"1.0"),
        ("-0", b"-0.0"),
        ("100000000000000000000", b"1e+20"),
        (
            r#"{"a":[0,1,true,null],"z":"unchanged"}"#,
            br#"{"a":[0,1,true,null],"z":"unchanged"}"#,
        ),
    ];
    for (input, expected) in cases {
        let value: serde_json::Value = serde_json::from_str(input).unwrap();
        assert_eq!(
            serde_json::to_vec(&value).unwrap(),
            *expected,
            "history JSON compatibility changed for {input}",
        );
    }
}

#[test]
fn history_payload_hash_keeps_the_released_identity_witness() {
    let value: serde_json::Value = serde_json::from_str(r#"{"z":0,"a":1}"#).unwrap();
    assert_eq!(
        ctx_history_core::compute_payload_hash(&value).unwrap(),
        "fnv1a64:864eb0392469e43d",
    );
}

#[test]
fn embedded_sift_preserves_numeric_lexemes_and_field_order() {
    // Deliberate repetition makes a complete non-raw representation cheaper.
    // Keep the input literal: parsing it through history JSON would erase the
    // numeric spellings and object order this half of the boundary preserves.
    const ROW: &str = r#"{"z_decimal":1.00,"y_exponent":1e0,"x_negative_zero":-0,"w_large_integer":100000000000000000000,"v_precise_decimal":12345678901234567890.1234567890123456789,"a_nested":{"z":2,"a":1}}"#;
    let input = format!("[{}]", [ROW; 128].join(","));
    let compactor = sift::Compactor::new().unwrap();
    let compacted = compactor.compact(&input);
    assert_ne!(
        compacted.encoding,
        sift::Encoding::Raw,
        "the fixture must exercise an actual compaction codec",
    );
    assert!(compacted.output_tokens < compacted.input_tokens);
    let restored = sift::restore(compacted.encoding, &compacted.text).unwrap();
    assert_eq!(restored.as_bytes(), input.as_bytes());
}
