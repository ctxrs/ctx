use std::{fs, path::Path};

use super::{
    is_ident_continue, is_ident_start, is_path_ident, lex, matching_delimiter, package_root,
    AllowEntry, Token,
};

const POLICY_TEST_SOURCE: &str = include_str!("../raw_output_policy.rs");
const POLICY_SELF_TEST_SOURCE: &str = include_str!("self_tests.rs");

pub(super) fn validate(entry: &AllowEntry) -> Result<(), String> {
    let owner = entry.owning_test;
    let (path, symbol) = parse_identity(owner.identity)?;
    if !owner
        .covered_paths
        .iter()
        .any(|coverage| path_is_covered(entry.path, coverage))
    {
        return Err(format!(
            "owning test `{}` has no behavioral coverage contract for {}",
            owner.identity, entry.path
        ));
    }
    if owner.evidence.is_empty() {
        return Err(format!(
            "owning test `{}` declares no behavioral evidence",
            owner.identity
        ));
    }

    let source = read_test_source(&path)?;
    let matches = runnable_test_functions(&source)
        .into_iter()
        .filter(|test| test.name == symbol)
        .collect::<Vec<_>>();
    let test = match matches.as_slice() {
        [test] => test,
        [] => {
            return Err(format!(
                "owning test `{symbol}` is not one runnable #[test] in {path}"
            ));
        }
        tests => {
            return Err(format!(
                "owning test `{symbol}` is ambiguous ({} definitions in {path})",
                tests.len()
            ));
        }
    };
    if !test.has_assertion {
        return Err(format!(
            "owning test `{}` has no behavioral assertion",
            owner.identity
        ));
    }
    let missing = owner
        .evidence
        .iter()
        .filter(|evidence| !test.tokens.iter().any(|token| token.text == ***evidence))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "owning test `{}` is missing behavioral evidence: {}",
            owner.identity,
            missing.join(", ")
        ));
    }
    Ok(())
}

fn parse_identity(identity: &str) -> Result<(String, String), String> {
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
    Ok((path, symbol.to_owned()))
}

fn read_test_source(path: &str) -> Result<String, String> {
    match path {
        "tests/raw_output_policy.rs" => Ok(POLICY_TEST_SOURCE.to_owned()),
        "tests/raw_output_policy/self_tests.rs" => Ok(POLICY_SELF_TEST_SOURCE.to_owned()),
        _ => fs::read_to_string(package_root().join(path))
            .map_err(|error| format!("cannot read owning test source {path}: {error}")),
    }
}

fn path_is_covered(path: &str, coverage: &str) -> bool {
    if coverage.ends_with('/') {
        path.starts_with(coverage)
    } else {
        path == coverage
    }
}

#[derive(Debug)]
struct RunnableTest {
    name: String,
    tokens: Vec<Token>,
    has_assertion: bool,
}

fn runnable_test_functions(source: &str) -> Vec<RunnableTest> {
    let tokens = lex(source);
    let mut tests = Vec::new();
    for index in 0..tokens.len().saturating_sub(1) {
        if tokens[index].text != "fn"
            || !has_exact_test_attribute(&tokens, index)
            || has_ignore_attribute(&tokens, index)
            || !tokens
                .get(index + 1)
                .is_some_and(|token| is_path_ident(&token.text))
        {
            continue;
        }
        let Some(open) = (index + 2..tokens.len()).find(|cursor| tokens[*cursor].text == "{")
        else {
            continue;
        };
        let Some(close) = matching_delimiter(&tokens, open, "{", "}") else {
            continue;
        };
        let body = tokens[open + 1..close].to_vec();
        let has_assertion = body.windows(2).any(|pair| {
            matches!(
                pair[0].text.as_str(),
                "assert" | "assert_eq" | "assert_ne" | "debug_assert" | "debug_assert_eq"
            ) && pair[1].text == "!"
        });
        tests.push(RunnableTest {
            name: tokens[index + 1].text.clone(),
            tokens: body,
            has_assertion,
        });
    }
    tests
}

pub(super) fn runnable_test_function_names(source: &str) -> Vec<String> {
    runnable_test_functions(source)
        .into_iter()
        .map(|test| test.name)
        .collect()
}

fn has_exact_test_attribute(tokens: &[Token], fn_index: usize) -> bool {
    attributes_before(tokens, fn_index)
        .into_iter()
        .any(|attribute| attribute.len() == 1 && attribute[0].text == "test")
}

fn has_ignore_attribute(tokens: &[Token], fn_index: usize) -> bool {
    attributes_before(tokens, fn_index)
        .into_iter()
        .any(|attribute| {
            attribute
                .first()
                .is_some_and(|token| token.text == "ignore")
        })
}

fn attributes_before(tokens: &[Token], fn_index: usize) -> Vec<&[Token]> {
    let mut cursor = fn_index;
    if cursor > 0 && tokens[cursor - 1].text == "async" {
        cursor -= 1;
    }
    let mut attributes = Vec::new();
    while cursor > 0 && tokens[cursor - 1].text == "]" {
        let Some(open) = super::reverse_matching_delimiter(tokens, cursor - 1, "[", "]") else {
            break;
        };
        if open == 0 || tokens[open - 1].text != "#" {
            break;
        }
        attributes.push(&tokens[open + 1..cursor - 1]);
        cursor = open - 1;
    }
    attributes
}
