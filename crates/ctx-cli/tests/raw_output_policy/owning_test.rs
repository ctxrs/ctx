use std::{fs, path::Path};

use super::{is_ident_continue, is_ident_start, is_path_ident, lex, package_root, Token};

const POLICY_TEST_SOURCE: &str = include_str!("../raw_output_policy.rs");
const POLICY_SELF_TEST_SOURCE: &str = include_str!("self_tests.rs");

pub(super) fn validate(identity: &str) -> Result<(), String> {
    let Some((path, symbol)) = identity.split_once(".rs::") else {
        return Err(
            "owning test must be an exact `<source>.rs::<test_function>` identity".to_owned(),
        );
    };
    let path = format!("{path}.rs");
    if symbol.is_empty()
        || symbol.contains("::")
        || !symbol.bytes().next().is_some_and(is_ident_start)
        || !symbol.bytes().all(is_ident_continue)
    {
        return Err("owning test function is not one exact Rust identifier".to_owned());
    }
    if path != "tests/raw_output_policy.rs"
        && path != "tests/raw_output_policy/self_tests.rs"
        && !path.starts_with("src/")
    {
        return Err("owning test source is outside the source-checked test roots".to_owned());
    }
    if Path::new(&path)
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("owning test source path is not normalized".to_owned());
    }

    let source = match path.as_str() {
        "tests/raw_output_policy.rs" => POLICY_TEST_SOURCE.to_owned(),
        "tests/raw_output_policy/self_tests.rs" => POLICY_SELF_TEST_SOURCE.to_owned(),
        _ => fs::read_to_string(package_root().join(&path))
            .map_err(|error| format!("cannot read owning test source {path}: {error}"))?,
    };
    let matches = test_function_names(&source)
        .into_iter()
        .filter(|candidate| candidate == symbol)
        .count();
    match matches {
        1 => Ok(()),
        0 => Err(format!(
            "owning test `{symbol}` is not a source-resolvable #[test] in {path}"
        )),
        count => Err(format!(
            "owning test `{symbol}` is ambiguous ({count} definitions in {path})"
        )),
    }
}

fn test_function_names(source: &str) -> Vec<String> {
    let tokens = lex(source);
    let mut names = Vec::new();
    for index in 0..tokens.len().saturating_sub(1) {
        if tokens[index].text == "fn"
            && has_test_attribute(&tokens, index)
            && tokens
                .get(index + 1)
                .is_some_and(|token| is_path_ident(&token.text))
        {
            names.push(tokens[index + 1].text.clone());
        }
    }
    names
}

fn has_test_attribute(tokens: &[Token], fn_index: usize) -> bool {
    let mut cursor = fn_index;
    if cursor > 0 && tokens[cursor - 1].text == "async" {
        cursor -= 1;
    }
    let mut is_test = false;
    while cursor > 0 && tokens[cursor - 1].text == "]" {
        let Some(open) = reverse_matching_delimiter(tokens, cursor - 1, "[", "]") else {
            return false;
        };
        if open == 0 || tokens[open - 1].text != "#" {
            return false;
        }
        is_test |= tokens[open + 1..cursor - 1]
            .iter()
            .any(|token| token.text == "test");
        cursor = open - 1;
    }
    is_test
}

fn reverse_matching_delimiter(
    tokens: &[Token],
    close_index: usize,
    open: &str,
    close: &str,
) -> Option<usize> {
    let mut depth = 0usize;
    for index in (0..=close_index).rev() {
        if tokens[index].text == close {
            depth += 1;
        } else if tokens[index].text == open {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}
