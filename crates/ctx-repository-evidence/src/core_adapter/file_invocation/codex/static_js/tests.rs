use std::process::Command;

use super::*;

const ISOLATED_CASE_ENV: &str = "CTX_REPOSITORY_STATIC_JS_ISOLATED_CASE";

#[test]
fn shared_aliases_cannot_amplify_past_expanded_value_bound() {
    let seed =
        serde_json::to_string(&"x".repeat(MAX_STATIC_VALUE_WEIGHT / 32)).expect("seed literal");
    let source = format!(
        "const level0 = {seed};\n\
         const level1 = [level0, level0];\n\
         const level2 = [level1, level1];\n\
         const level3 = [level2, level2];\n\
         const level4 = [level3, level3];\n\
         const level5 = [level4, level4];"
    );
    assert!(StaticJsParser::new(&source).parse_program().is_none());
}

#[test]
fn prototype_sensitive_and_noncanonical_members_abstain() {
    let cases = [
        "const inherited = {cmd: 'git status'};\
         const args = {__proto__: inherited};\
         await tools.exec_command(args);",
        "const values = ['ignored', '*** Begin Patch'];\
         await tools.apply_patch(values['01']);",
        "const args = {cmd: 'git status'};\
         await tools.exec_command(args.constructor);",
        "const args = {prototype: {cmd: 'git status'}};\
         await tools.exec_command(args.prototype);",
    ];
    for source in cases {
        assert!(
            StaticJsParser::new(source).parse_program().is_none(),
            "unexpectedly accepted {source}"
        );
    }
}

#[test]
fn json_stringify_cannot_supply_tool_arguments() {
    let cases = [
        "await tools.exec_command({cmd: JSON.stringify({b: 1, a: 2})});",
        "const encoded = JSON.stringify({b: 1, a: 2}, null, 2);\
         await tools.apply_patch(encoded);",
    ];
    for source in cases {
        assert!(
            StaticJsParser::new(source).parse_program().is_none(),
            "unexpectedly accepted {source}"
        );
    }
}

#[test]
fn semantic_errors_and_preparse_complexity_excess_abstain() {
    let semantic_error = "const eval = '*** Begin Patch'; await tools.apply_patch(eval);";
    assert!(
        StaticJsParser::new(semantic_error)
            .parse_program()
            .is_none()
    );

    let deeply_nested = format!(
        "{}0{}",
        "(".repeat(MAX_STATIC_RAW_COMPLEXITY_ITEMS + 1),
        ")".repeat(MAX_STATIC_RAW_COMPLEXITY_ITEMS + 1)
    );
    assert!(!has_bounded_parser_complexity(&deeply_nested));
    assert!(
        StaticJsParser::new(&deeply_nested)
            .parse_program()
            .is_none()
    );
}

#[test]
fn raw_preparse_guard_keeps_representative_patch_content_usable() {
    let patch = "*** Begin Patch\n\
                 *** Add File: generated.js\n\
                 +export function summarize(values) {\n\
                 +  const total = values.reduce((sum, value) => sum + value, 0);\n\
                 +  return {\n\
                 +    total,\n\
                 +    average: values.length === 0 ? null : total / values.length,\n\
                 +    matches: values.filter((value) => /[{}]/.test(String(value))),\n\
                 +  };\n\
                 +}\n\
                 +\n\
                 +export const enabled = true;\n\
                 *** End Patch"
        .to_owned();
    let encoded = serde_json::to_string(&patch).expect("patch literal");
    let source = format!("const patch = {encoded}; await tools.apply_patch(patch);");
    assert!(has_bounded_parser_complexity(&source));
    let calls = StaticJsParser::new(&source)
        .parse_program()
        .expect("representative static patch");
    assert_eq!(calls, vec![StaticNestedToolCall::ApplyPatch { patch }]);
}

#[test]
fn malicious_recursive_inputs_abstain_in_process_isolation() {
    if let Some(case) = std::env::var_os(ISOLATED_CASE_ENV) {
        let source = malicious_source(case.to_str().expect("ASCII isolated case"));
        assert!(source.len() <= MAX_STATIC_JS_BYTES);
        if matches!(case.to_str(), Some("exponentiation" | "member-chain")) {
            assert!(source.len() > MAX_STATIC_JS_BYTES - 8);
        }
        assert!(!has_bounded_parser_complexity(&source));
        assert!(StaticJsParser::new(&source).parse_program().is_none());
        return;
    }

    let executable = std::env::current_exe().expect("current test executable");
    for case in ["exponentiation", "member-chain", "threshold-unary"] {
        let output = Command::new(&executable)
            .arg("malicious_recursive_inputs_abstain_in_process_isolation")
            .arg("--nocapture")
            .env(ISOLATED_CASE_ENV, case)
            .output()
            .expect("run isolated malicious-input regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "isolated {case} regression failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            stdout,
            stderr
        );
    }
}

#[test]
fn raw_complexity_bounds_flat_and_word_operator_shapes() {
    let flat_statements = "0;\n".repeat(MAX_STATIC_RAW_COMPLEXITY_ITEMS / 2 + 1);
    let word_operator = format!(
        "value{}",
        " instanceof value".repeat(MAX_STATIC_RAW_COMPLEXITY_ITEMS / 2 + 1)
    );
    for source in [flat_statements, word_operator] {
        assert!(source.len() <= MAX_STATIC_JS_BYTES);
        assert!(!has_bounded_parser_complexity(&source));
        assert!(StaticJsParser::new(&source).parse_program().is_none());
    }
}

fn malicious_source(case: &str) -> String {
    match case {
        "exponentiation" => {
            let mut source = "1**".repeat((MAX_STATIC_JS_BYTES - 1) / 3);
            source.push('1');
            source
        }
        "member-chain" => {
            let mut source = String::from("a");
            source.push_str(&".a".repeat((MAX_STATIC_JS_BYTES - 1) / 2));
            source
        }
        "threshold-unary" => {
            let mut source = "!".repeat(MAX_STATIC_RAW_COMPLEXITY_ITEMS);
            source.push('0');
            source
        }
        _ => panic!("unknown isolated case {case}"),
    }
}
