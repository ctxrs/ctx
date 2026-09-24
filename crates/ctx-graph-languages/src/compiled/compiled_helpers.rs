use super::*;

pub(super) fn recover_string_tests(
    source: &str,
    normalized: &mut Option<DialectSource>,
) -> Vec<StringTest> {
    let tokens = dialect_tokens(source);
    let mut tests = vec![];
    let mut edits = vec![];
    let mut bodies = vec![];
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate() {
        if t.opaque {
            continue;
        }
        if t.text == "{" {
            depth += 1;
        }
        if t.text == "}" {
            depth = depth.saturating_sub(1);
        }
        let top = depth == 0 && matches!(t.text, "TEST_CASE" | "TEST_CASE_TEMPLATE" | "SCENARIO");
        let subcase = t.text == "SUBCASE"
            && bodies
                .iter()
                .any(|(start, end)| t.start > *start && t.end < *end);
        if !(top || subcase) || tokens.get(i + 1).is_none_or(|t| t.text != "(") {
            continue;
        }
        let Some(label) = tokens
            .get(i + 2)
            .filter(|t| t.opaque && t.text.starts_with('"') && t.text.ends_with('"'))
        else {
            continue;
        };
        let Some(close) = matching_token(&tokens, i + 1, "(", ")") else {
            continue;
        };
        if tokens.get(close + 1).is_none_or(|t| t.text != "{") {
            continue;
        }
        let Some(end) = matching_token(&tokens, close + 1, "{", "}") else {
            continue;
        };
        edits.push((
            t.start,
            tokens[close].end,
            if top { "void t()" } else { "if(1)" },
        ));
        if top {
            tests.push(StringTest {
                start: t.start,
                label: label.text.into(),
                macro_name: t.text.into(),
            });
            bodies.push((tokens[close + 1].start, tokens[end].end));
        }
    }
    if edits.is_empty() {
        return tests;
    }
    let parsed = normalized.get_or_insert_with(|| DialectSource {
        source: source.into(),
        dialect: "cpp",
        spans: vec![],
    });
    let mut bytes = parsed.source.as_bytes().to_vec();
    for (start, end, replacement) in edits {
        for b in &mut bytes[start..end] {
            if !matches!(*b, b'\n' | b'\r') {
                *b = b' ';
            }
        }
        bytes[start..start + replacement.len()].copy_from_slice(replacement.as_bytes());
        parsed
            .spans
            .push(json!({"kind":"cpp_string_test","start_byte":start,"end_byte":end}));
    }
    parsed.source = String::from_utf8(bytes).unwrap();
    tests
}

pub(super) fn context_key(path: &str, key: &str) -> String {
    // The parent supplies the assembly boundary before unnamespaced C# names
    // can participate. Without it the inventory's unit remains file-local.
    key.strip_prefix(&format!("csharp:symbol:@{path}."))
        .map_or_else(
            || key.to_owned(),
            |symbol| format!("csharp:symbol:{symbol}"),
        )
}

pub(super) fn type_node(n: &crate::model::Node) -> bool {
    matches!(
        n.kind.as_str(),
        "class" | "struct" | "union" | "enum" | "interface" | "type"
    )
}

pub(super) fn node_keys(n: &crate::model::Node) -> Vec<String> {
    n.binding_key
        .iter()
        .cloned()
        .chain(
            n.metadata["binding_aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned)),
        )
        .collect()
}
