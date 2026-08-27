use std::{
    env, fs,
    path::{Path, PathBuf},
};

const PRODUCTION_SOURCE_ROOTS: &[&str] = &[
    "crates/ctx-agent-application/src",
    "crates/ctx-agent-integrations/src",
    "crates/ctx-cli/src",
    "crates/ctx-cli-presentation/src",
    "crates/ctx-daemon-runtime/src",
    "crates/ctx-history-cli/src",
    "crates/ctx-managed-pair-engine/src",
    "crates/ctx-terminal/src",
    "crates/ctx-upgrade-engine/src",
];

const BUILD_SCRIPTS: &[&str] = &[
    "crates/ctx-cli/build.rs",
    "crates/ctx-semantic-model/build.rs",
    "crates/ctx-upgrade-engine/build.rs",
];

// These files are the existing raw delivery boundaries: measured terminal
// writers, already-framed machine protocols, process-stream finalization, and
// explicitly tested diagnostics or capability probes. Product code outside
// this list must use those contracts instead of opening stdout/stderr itself.
const DIRECT_OUTPUT_BOUNDARIES: &[(&str, &[(&str, usize)])] = &[
    ("crates/ctx-cli/build.rs", &[("println!", 4)]),
    (
        "crates/ctx-cli-presentation/src/skill/selection.rs",
        &[("stderr()", 1)],
    ),
    ("crates/ctx-cli/src/analytics.rs", &[("eprintln!", 1)]),
    (
        "crates/ctx-cli/src/commands/status/usage.rs",
        &[("eprintln!", 3)],
    ),
    ("crates/ctx-cli/src/companion.rs", &[("stderr()", 3)]),
    (
        "crates/ctx-cli/src/core_capability.rs",
        &[("eprintln!", 1), ("stdout()", 1)],
    ),
    (
        "crates/ctx-cli/src/core_capability/hosted_pair_install.rs",
        &[("stdout()", 1)],
    ),
    (
        "crates/ctx-cli/src/dispatch.rs",
        &[("eprintln!", 2), ("stdout()", 1), ("stderr()", 1)],
    ),
    ("crates/ctx-cli/src/mcp.rs", &[("stdout()", 1)]),
    (
        "crates/ctx-cli/src/release_build_identity.rs",
        &[("println!", 3)],
    ),
    // The only direct write is a diagnostic in a cfg(test)-only pidfd test.
    (
        "crates/ctx-daemon-runtime/src/handoff/termination.rs",
        &[("eprintln!", 2)],
    ),
    ("crates/ctx-semantic-model/build.rs", &[("println!", 2)]),
    (
        "crates/ctx-terminal/src/output.rs",
        &[("stdout()", 1), ("stderr()", 2)],
    ),
    (
        "crates/ctx-terminal/src/ui/writer.rs",
        &[("stdout()", 2), ("stderr()", 2)],
    ),
    ("crates/ctx-upgrade-engine/build.rs", &[("println!", 3)]),
    (
        "crates/ctx-upgrade-engine/src/upgrade/install/hosted_transaction.rs",
        &[("println!", 2)],
    ),
    (
        "crates/ctx-upgrade-engine/src/upgrade/install/transaction/windows/helper.rs",
        &[("stdout()", 1)],
    ),
];

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.parent().and_then(Path::parent) {
        if root.join("crates/ctx-cli/src").is_dir() {
            return root.to_path_buf();
        }
    }
    if let (Ok(source_dir), Ok(workspace)) = (env::var("TEST_SRCDIR"), env::var("TEST_WORKSPACE")) {
        let root = PathBuf::from(source_dir).join(workspace);
        if root.join("crates/ctx-cli/src").is_dir() {
            return root;
        }
    }
    panic!(
        "cannot resolve workspace source root from CARGO_MANIFEST_DIR={}",
        env!("CARGO_MANIFEST_DIR")
    );
}

fn visit_production_sources(directory: &Path, paths: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("read {} entry: {error}", directory.display()));
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                visit_production_sources(&path, paths);
            }
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            && !is_test_source(&path)
        {
            paths.push(path);
        }
    }
}

fn is_test_source(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.starts_with("test_support")
        || path
            .components()
            .any(|component| component.as_os_str() == "tests")
}

fn production_source_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = BUILD_SCRIPTS
        .iter()
        .map(|relative| root.join(relative))
        .collect::<Vec<_>>();
    for relative in PRODUCTION_SOURCE_ROOTS {
        visit_production_sources(&root.join(relative), &mut paths);
    }
    paths.sort();
    paths
}

fn direct_output_counts(source: &str) -> Vec<(&'static str, usize)> {
    let mut counts = Vec::new();

    for (name, marker) in [
        ("print", "print!"),
        ("println", "println!"),
        ("eprint", "eprint!"),
        ("eprintln", "eprintln!"),
        ("dbg", "dbg!"),
    ] {
        let count = count_direct_references(source, name, "!");
        if count > 0 {
            counts.push((marker, count));
        }
    }
    for (name, marker) in [("stdout", "stdout()"), ("stderr", "stderr()")] {
        let count =
            count_direct_references(source, name, "(") + usize::from(imports_io_glob(source));
        if count > 0 {
            counts.push((marker, count));
        }
    }

    counts
}

fn count_direct_references(source: &str, name: &str, suffix: &str) -> usize {
    source
        .match_indices(name)
        .filter(|(index, matched)| {
            let name_end = *index + matched.len();
            source[..*index].chars().next_back().is_none_or(|previous| {
                !previous.is_ascii_alphanumeric() && previous != '_' && previous != '.'
            }) && source[name_end..]
                .chars()
                .next()
                .is_none_or(|next| !next.is_ascii_alphanumeric() && next != '_')
                && (source[skip_trivia(source, name_end)..].starts_with(suffix)
                    || preceded_by_path_separator(source, *index)
                    || is_in_use_statement(source, *index))
        })
        .count()
}

fn preceded_by_path_separator(source: &str, index: usize) -> bool {
    let path_end = source[..index].strip_suffix("r#").map_or(index, str::len);
    source[..path_end]
        .rmatch_indices("::")
        .any(|(separator, _)| skip_trivia(source, separator + 2) == path_end)
}

fn imports_io_glob(source: &str) -> bool {
    source.match_indices("io").any(|(index, matched)| {
        if !identifier_at(source, index, matched.len()) {
            return false;
        }
        let mut next = skip_trivia(source, index + matched.len());
        if !source[next..].starts_with("::") {
            return false;
        }
        next = skip_trivia(source, next + 2);
        if source[next..].starts_with('*') {
            return true;
        }
        if !source[next..].starts_with('{') {
            return false;
        }
        source[next + 1..]
            .split_once('}')
            .is_some_and(|(members, _)| members.contains('*'))
    })
}

fn imports_any_glob(source: &str) -> bool {
    source.match_indices('*').any(|(index, _)| {
        preceded_by_path_separator(source, index) || is_in_use_statement(source, index)
    })
}

fn is_in_use_statement(source: &str, index: usize) -> bool {
    source[..index]
        .match_indices("use")
        .filter(|(start, matched)| identifier_at(source, *start, matched.len()))
        .any(|(start, matched)| {
            let mut cursor = start + matched.len();
            while cursor < index {
                cursor = skip_trivia(source, cursor);
                if cursor >= index {
                    return true;
                }
                if source.as_bytes()[cursor] == b';' {
                    return false;
                }
                cursor += source[cursor..].chars().next().map_or(1, char::len_utf8);
            }
            true
        })
}

fn skip_trivia(source: &str, mut index: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            index += 1;
        }
        if bytes.get(index..index + 2) == Some(b"//") {
            index += 2;
            while bytes.get(index).is_some_and(|byte| *byte != b'\n') {
                index += 1;
            }
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            let mut depth = 1usize;
            while depth > 0 {
                match bytes.get(index..index + 2) {
                    Some(b"/*") => {
                        depth += 1;
                        index += 2;
                    }
                    Some(b"*/") => {
                        depth -= 1;
                        index += 2;
                    }
                    Some(_) => index += 1,
                    None => return bytes.len(),
                }
            }
            continue;
        }
        return index;
    }
}

fn identifier_at(source: &str, index: usize, length: usize) -> bool {
    let previous = source[..index].chars().next_back();
    let next = source[index + length..].chars().next();
    previous.is_none_or(|character| !character.is_alphanumeric() && character != '_')
        && next.is_none_or(|character| !character.is_alphanumeric() && character != '_')
}

fn contains_identifier(source: &str, name: &str) -> bool {
    source
        .match_indices(name)
        .any(|(index, matched)| identifier_at(source, index, matched.len()))
}

fn direct_clap_exit_route(source: &str) -> bool {
    has_associated_exit_parse(source, "Cli")
        || ((contains_identifier(source, "Parser")
            || (contains_identifier(source, "clap") && imports_any_glob(source)))
            && has_any_associated_exit_parse(source))
}

fn has_associated_exit_parse(source: &str, receiver: &str) -> bool {
    source.match_indices(receiver).any(|(index, matched)| {
        if !identifier_at(source, index, matched.len()) {
            return false;
        }
        let mut next = skip_trivia(source, index + matched.len());
        if !source[next..].starts_with("::") {
            return false;
        }
        next = skip_trivia(source, next + 2);
        is_exit_parse_name(source, next)
    })
}

fn has_any_associated_exit_parse(source: &str) -> bool {
    source
        .match_indices("::")
        .any(|(separator, _)| is_exit_parse_name(source, skip_trivia(source, separator + 2)))
}

fn is_exit_parse_name(source: &str, mut index: usize) -> bool {
    if source[index..].starts_with("r#") {
        index += 2;
    }
    ["parse_from", "parse"]
        .into_iter()
        .any(|name| source[index..].starts_with(name) && identifier_at(source, index, name.len()))
}

fn approved_direct_output_counts(path: &str) -> &'static [(&'static str, usize)] {
    DIRECT_OUTPUT_BOUNDARIES
        .iter()
        .find_map(|(candidate, counts)| (*candidate == path).then_some(*counts))
        .unwrap_or_default()
}

#[test]
fn production_direct_output_is_confined_to_explicit_boundaries() {
    let root = workspace_root();
    let mut findings = Vec::new();
    let mut visited = Vec::new();

    for path in production_source_paths(&root) {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        visited.push(relative.clone());
        if relative.starts_with("crates/ctx-terminal/src/") {
            let print_macros = ["print", "println", "eprint", "eprintln", "dbg"]
                .into_iter()
                .filter(|name| contains_identifier(&source, name))
                .collect::<Vec<_>>();
            if !print_macros.is_empty() {
                findings.push(format!(
                    "  {relative}: ctx-terminal print macro reference ({})",
                    print_macros.join(", ")
                ));
            }
        }
        if direct_clap_exit_route(&source) {
            findings.push(format!(
                "  {relative}: direct Cli::parse route bypasses selected-stream errors"
            ));
        }
        let actual = direct_output_counts(&source);
        let expected = approved_direct_output_counts(&relative);
        if actual != expected {
            findings.push(format!(
                "  {relative}: expected {expected:?}, found {actual:?}"
            ));
        }
    }
    for (path, _) in DIRECT_OUTPUT_BOUNDARIES {
        if !visited.iter().any(|visited| visited == path) {
            findings.push(format!("  {path}: stale direct-output boundary"));
        }
    }

    assert!(
        findings.is_empty(),
        "production code must use measured writers or an existing framed protocol; direct raw output found:\n{}",
        findings.join("\n")
    );
}

#[test]
fn cargo_build_script_output_counts_are_exact() {
    let root = workspace_root();
    for (relative, expected) in [
        ("crates/ctx-cli/build.rs", 4),
        ("crates/ctx-semantic-model/build.rs", 2),
        ("crates/ctx-upgrade-engine/build.rs", 3),
    ] {
        let source = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        assert_eq!(
            direct_output_counts(&source),
            vec![("println!", expected)],
            "{relative} must emit exactly {expected} Cargo println directives"
        );
    }
}

#[test]
fn forbidden_direct_output_mutations_are_detected() {
    for (source, expected) in [
        ("print!(\"human\");", "print!"),
        ("println!(\"human\");", "println!"),
        ("eprint!(\"diagnostic\");", "eprint!"),
        ("std::eprintln!(\"diagnostic\");", "eprintln!"),
        ("std::io::stdout().write_all(bytes)?;", "stdout()"),
        ("use std::io::stderr as raw; raw().flush()?;", "stderr()"),
        ("use std::println as raw; raw!(\"hidden\");", "println!"),
        (
            "use ::std::io::stdout as raw; raw().write_all(bytes)?;",
            "stdout()",
        ),
        (
            "std::io::stdout /* gap */ ().write_all(bytes)?;",
            "stdout()",
        ),
        (
            "let raw = (std::io::stdout); raw().write_all(bytes)?;",
            "stdout()",
        ),
        (
            "let raw = std::io:: /* gap :: nested */ stdout; raw().write_all(bytes)?;",
            "stdout()",
        ),
        (
            "let raw = std::io::r#stdout; raw().write_all(bytes)?;",
            "stdout()",
        ),
        ("use std::io::{stdout as raw};", "stdout()"),
        ("use std::io::{/* ; */ stdout as raw, Write};", "stdout()"),
        ("use std::io::{/* ; */ stdout, Write};", "stdout()"),
        ("use std::io::*; let raw = stdout; raw();", "stdout()"),
        ("use std::{io::{self, *}};", "stderr()"),
        ("std::println /* gap */ !(\"hidden\");", "println!"),
        ("dbg!(value);", "dbg!"),
    ] {
        assert!(
            direct_output_counts(source)
                .iter()
                .any(|(marker, _)| marker == &expected),
            "mutation escaped direct-output policy: {source}"
        );
    }
    assert!(direct_clap_exit_route("let cli = Cli :: parse();"));
    assert!(direct_clap_exit_route(
        "let cli = Cli::parse_from(arguments);"
    ));
    assert!(direct_clap_exit_route(
        "let cli = Cli /* gap */ :: parse();"
    ));
    assert!(direct_clap_exit_route("let parse = Cli::parse; parse();"));
    assert!(direct_clap_exit_route("let cli = Cli::r#parse();"));
    assert!(direct_clap_exit_route(
        "let cli = <Cli as clap::Parser>::parse();"
    ));
    assert!(direct_clap_exit_route(
        "use clap::Parser as ParseCli; let cli: Cli = ParseCli::parse();"
    ));
    assert!(direct_clap_exit_route(
        "use clap::*; type Args = Cli; let _: Cli = Args::parse();"
    ));
    assert!(contains_identifier(
        "pub use standard::println as emit;",
        "println"
    ));
    assert!(
        !approved_direct_output_counts("crates/ctx-terminal/src/output.rs")
            .iter()
            .any(|(marker, _)| marker == &"println!")
    );
    assert_eq!(
        direct_output_counts("std::io::stdout();"),
        approved_direct_output_counts("crates/ctx-cli/src/core_capability/hosted_pair_install.rs")
    );
    assert_ne!(
        direct_output_counts("std::io::stdout(); std::io::stdout();"),
        approved_direct_output_counts("crates/ctx-cli/src/core_capability/hosted_pair_install.rs"),
        "an additional call inside an approved path must be rejected"
    );
}

#[test]
fn approved_writer_contracts_are_not_misclassified() {
    for source in [
        "output::write_stdout(format_args!(\"human\"));",
        "ui.write_stdout(&document)?;",
        "ui.write_stderr_bytes(encoded.as_bytes())?;",
        "ui.stdout_writer().write_all(bytes)?;",
        "writer.write_all(frame)?;",
        "command.stdout(Stdio::null()).stderr(Stdio::piped());",
        "let bytes = child_output.stdout();",
    ] {
        assert!(
            direct_output_counts(source).is_empty(),
            "approved writer contract was misclassified: {source}"
        );
    }
}
