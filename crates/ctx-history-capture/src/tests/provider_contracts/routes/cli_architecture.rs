use super::*;

const CLI_COLD_PATH: &str = "crates/ctx-cli/src/commands/import/cold.rs";
const CLI_EXPLICIT_PATH: &str = "crates/ctx-cli/src/commands/import/explicit.rs";
const CLI_PLUGIN_PATH: &str = "crates/ctx-cli/src/commands/import/requests.rs";
const FORBIDDEN_LEGACY_IDENTIFIERS: &[&str] =
    &["CaptureDb", "CapturedBatch", "capture_db", "captured_batch"];

fn function_body<'a>(tokens: &'a [Token], name: &str) -> &'a [Token] {
    let matches = functions(tokens)
        .into_iter()
        .filter(|function| function.name == name)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one function named {name}"
    );
    matches[0].body
}

fn let_initializer<'a>(body: &'a [Token], binding: &str) -> &'a [Token] {
    let starts = body
        .windows(3)
        .enumerate()
        .filter(|(_, window)| {
            window[0].text == "let" && window[1].text == binding && window[2].text == "="
        })
        .map(|(index, _)| index + 3)
        .collect::<Vec<_>>();
    assert_eq!(
        starts.len(),
        1,
        "expected exactly one `{binding}` initializer"
    );
    let start = starts[0];
    let end = let_statement_end(body, start).expect("initializer must end with a semicolon");
    &body[start..end]
}

fn capture_provider_variants(tokens: &[Token]) -> BTreeSet<String> {
    tokens
        .windows(3)
        .filter(|window| window[0].text == "CaptureProvider" && window[1].text == "::")
        .map(|window| window[2].text.clone())
        .collect()
}

fn assert_exact_architecture_call(path: &str, function: &str, expected: &str) {
    let calls = function_calls(&read_workspace_source(path), function);
    let mut known_routes = PROVIDER_ROUTES
        .iter()
        .flat_map(|contract| contract.routes)
        .map(|route| route.public_route)
        .collect::<BTreeSet<_>>();
    known_routes.extend([
        "import_custom_history_jsonl_v1",
        "import_custom_history_jsonl_v1_reader",
    ]);
    let architecture_calls = calls
        .iter()
        .filter(|call| known_routes.contains(call.as_str()) || call.contains("nativepath"))
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        architecture_calls,
        BTreeSet::from([expected]),
        "{function} must call only {expected}"
    );
}

#[test]
fn only_codex_is_cold_eligible_and_all_other_routes_remain_nativepath() {
    let cold_tokens = rust_tokens(&read_workspace_source(CLI_COLD_PATH));
    let cold_body = function_body(&cold_tokens, "try_codex_cold_cli_import");
    let eligibility = let_initializer(cold_body, "eligible_command");
    assert_eq!(
        capture_provider_variants(eligibility),
        BTreeSet::from(["Codex".to_owned()]),
        "fresh cold admission must name only Codex"
    );
    assert_eq!(
        called_identifiers(eligibility),
        BTreeSet::from(["capture_provider", "is_none", "is_some_and"]),
        "cold provider admission must remain a direct, pure CLI-provider comparison"
    );
    assert_eq!(
        capture_provider_variants(cold_body),
        BTreeSet::from(["Codex".to_owned()]),
        "the complete cold path must remain Codex-only"
    );
    assert!(
        called_identifiers(cold_body).contains("build_codex_cold_store"),
        "Codex cold admission must enter the canonical cold Store builder"
    );

    let (_, _, expected_variants) = expected_contract_sets();
    let expected_ordinary = expected_variants
        .iter()
        .filter(|variant| variant.as_str() != "Codex")
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        expected_ordinary.len(),
        40,
        "ordinary semantic provider count changed"
    );
    let dispatch = dispatch_arms(&read_workspace_source(CLI_DISPATCH_PATH));
    let actual_ordinary = dispatch
        .keys()
        .filter(|variant| variant.as_str() != "Codex")
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_ordinary, expected_ordinary,
        "a non-Codex provider left the ordinary NativePath dispatcher"
    );

    assert_exact_architecture_call(
        CLI_EXPLICIT_PATH,
        "run_explicit_format_import",
        "import_custom_history_jsonl_v1",
    );
    assert_exact_architecture_call(
        CLI_PLUGIN_PATH,
        "import_history_source_plugin",
        "import_custom_history_jsonl_v1_reader",
    );
}

fn path_is_nativepath_source(path: &Path) -> bool {
    let nativepath = path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("native_path" | "nativepath")
        )
    });
    let source = path
        .components()
        .any(|component| component.as_os_str() == "source")
        || path
            .file_stem()
            .is_some_and(|stem| stem == "source" || stem == "spool");
    nativepath && source
}

fn legacy_route_violations(source: &str, path: &Path) -> Vec<String> {
    let tokens = production_tokens(source);
    let nativepath_source = path_is_nativepath_source(path);
    let mut violations = Vec::new();
    for token in &tokens {
        let forbidden_type = FORBIDDEN_LEGACY_IDENTIFIERS.contains(&token.text.as_str());
        let legacy_dispatch_term =
            token.text == "projector" || (token.text == "spool" && !nativepath_source);
        if forbidden_type || legacy_dispatch_term {
            violations.push(format!(
                "{}:{} contains forbidden legacy route identifier `{}`",
                path.display(),
                token.line,
                token.text
            ));
        }
    }
    let forbidden_module = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem == "projector" || (stem == "spool" && !nativepath_source));
    if forbidden_module {
        violations.push(format!(
            "{} is a forbidden legacy route module",
            path.display()
        ));
    }
    violations
}

#[test]
fn production_routes_have_no_legacy_capture_or_dispatch_architecture() {
    let root = workspace_root();
    let mut sources = Vec::new();
    for relative_root in PROVIDER_SOURCE_ROOTS {
        collect_production_rust_sources(&root.join(relative_root), &mut sources);
    }
    let violations = sources
        .into_iter()
        .flat_map(|path| {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            legacy_route_violations(&source, &path)
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "legacy provider architecture became production-reachable:\n{}",
        violations.join("\n")
    );
}

#[test]
fn legacy_route_guard_rejects_alternatives_but_allows_nativepath_source_spooling() {
    let legacy = "struct CaptureDb; struct CapturedBatch; mod spool; mod projector;";
    assert_eq!(
        legacy_route_violations(legacy, Path::new("provider/legacy.rs")).len(),
        4
    );
    assert!(legacy_route_violations(
        "mod spool; struct ContinuePathSpool;",
        Path::new("provider/continue_cli/native_path/source.rs")
    )
    .is_empty());
    assert_eq!(
        legacy_route_violations(
            "mod spool; mod projector;",
            Path::new("provider/example/native_path/publication.rs")
        )
        .len(),
        2
    );
}
