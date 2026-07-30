use std::collections::BTreeSet;

use super::*;

#[test]
fn exact_allowed_site_is_accepted() {
    let sites = scan_source(
        "src/example.rs",
        "fn emit(value: &str) { println!(\"{value}\"); }",
    );
    assert_eq!(sites.len(), 1);
    let site = &sites[0];
    let entry = AllowEntry {
        path: "src/example.rs",
        fingerprint: Box::leak(site.key.fingerprint.clone().into_boxed_str()),
        primitive: Primitive::PrintMacro,
        class: OutputClass::MachineProtocol,
        rationale: "synthetic JSON protocol",
        owning_test: "raw_output_policy::exact_allowed_site_is_accepted",
    };
    assert!(compare_policy(sites, &[entry]).is_closed());
}

#[test]
fn new_unmatched_site_is_rejected() {
    let sites = scan_source("src/example.rs", "fn emit() { eprintln!(\"new output\"); }");
    let diff = compare_policy(sites, &[]);
    assert_eq!(diff.unmatched.len(), 1);
    assert!(!diff.is_closed());
}

#[test]
fn stale_allowlist_entry_is_rejected() {
    let entry = AllowEntry {
        path: "src/example.rs",
        fingerprint: "emit#1@0000000000000000",
        primitive: Primitive::PrintMacro,
        class: OutputClass::JustifiedPlainHuman,
        rationale: "synthetic fallback",
        owning_test: "raw_output_policy::stale_allowlist_entry_is_rejected",
    };
    let diff = compare_policy(Vec::new(), &[entry]);
    assert_eq!(diff.stale.len(), 1);
    assert!(!diff.is_closed());
}

#[test]
fn classified_violation_is_rejected() {
    let sites = scan_source(
        "src/example.rs",
        "fn emit(value: &str) { println!(\"{value}\"); }",
    );
    assert_eq!(sites.len(), 1);
    let site = &sites[0];
    let entry = AllowEntry {
        path: "src/example.rs",
        fingerprint: Box::leak(site.key.fingerprint.clone().into_boxed_str()),
        primitive: Primitive::PrintMacro,
        class: OutputClass::Violation,
        rationale: "synthetic policy violation",
        owning_test: "raw_output_policy::classified_violation_is_rejected",
    };
    let diff = compare_policy(sites, &[entry]);
    assert_eq!(diff.violations.len(), 1);
    assert!(!diff.is_closed());
}

#[test]
fn scanner_excludes_only_definitely_test_only_regions() {
    let source = r#"
        fn production() { println!("production"); }
        #[cfg(test)]
        fn cfg_test() { println!("test"); }
        #[cfg(all(test, unix))]
        mod nested_test { fn emit() { eprintln!("test"); } }
        #[test]
        fn test_attribute() { print!("test"); }
        #[cfg(not(test))]
        fn non_test() { eprintln!("non-test"); }
        #[cfg(any(test, feature = "qualification"))]
        fn possible_non_test() { println!("qualification"); }
        // println!("comment");
        const TEXT: &str = "eprintln!(\"string\")";
    "#;
    let sites = scan_source("src/example.rs", source);
    assert_eq!(sites.len(), 3, "{sites:#?}");
    let owners = sites
        .iter()
        .map(|site| site.key.fingerprint.split('#').next().unwrap_or(""))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        owners,
        BTreeSet::from(["non_test", "possible_non_test", "production"])
    );
}

#[test]
fn normalized_fingerprint_ignores_whitespace_and_comments() {
    let compact = scan_source("src/example.rs", "fn emit(){println!(\"stable\");}");
    let spaced = scan_source(
        "src/example.rs",
        "fn emit() {\n  // explanation\n  println! ( \"stable\" ) ;\n}",
    );
    assert_eq!(compact[0].key, spaced[0].key);
}

#[test]
fn scanner_covers_raw_accessors_document_render_and_clap_exit() {
    let source = r#"
        fn sinks(ui: &mut Ui, document: &Document) {
            let _ = dbg!("diagnostic");
            let _ = io::stdout();
            let _ = std::io::stderr();
            let _ = crate::output::stdout_writer();
            let _ = ui.stderr_writer();
            let _ = Ui::with_writers(a, b, c, d);
            let _ = document.render_plain();
            let _ = document.render(&context);
            let _ = Cli::parse();
        }
    "#;
    let primitives = scan_source("src/example.rs", source)
        .into_iter()
        .map(|site| site.key.primitive)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        primitives,
        BTreeSet::from([
            Primitive::PrintMacro,
            Primitive::StdoutConstructor,
            Primitive::StderrConstructor,
            Primitive::OutputRawHelper,
            Primitive::UiRawWriter,
            Primitive::UiWriterInjection,
            Primitive::DocumentRender,
            Primitive::ClapParse,
        ])
    );
}
