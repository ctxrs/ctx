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
        owning_test: TestOwner::behavioral(
            "tests/raw_output_policy/self_tests.rs::exact_allowed_site_is_accepted",
            &["src/example.rs"],
            &["compare_policy"],
        ),
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
        owning_test: TestOwner::behavioral(
            "tests/raw_output_policy/self_tests.rs::stale_allowlist_entry_is_rejected",
            &["src/example.rs"],
            &["compare_policy"],
        ),
    };
    let diff = compare_policy(Vec::new(), &[entry]);
    assert_eq!(diff.stale.len(), 1);
    assert!(!diff.is_closed());
}

#[test]
fn build_script_add_remove_and_reorder_mutations_are_rejected() {
    const OWNER: TestOwner = TestOwner::behavioral(
        "tests/raw_output_policy/self_tests.rs::build_script_add_remove_and_reorder_mutations_are_rejected",
        &["build.rs"],
        &["compare_policy", "scan_source"],
    );
    let original = scan_source(
        "build.rs",
        r#"fn main() {
            println!("cargo:rustc-check-cfg=cfg(first)");
            println!("cargo:rustc-check-cfg=cfg(second)");
        }"#,
    );
    let allowed = original
        .iter()
        .map(|site| AllowEntry {
            path: "build.rs",
            fingerprint: Box::leak(site.key.fingerprint.clone().into_boxed_str()),
            primitive: Primitive::PrintMacro,
            class: OutputClass::MachineProtocol,
            rationale: "synthetic Cargo build-script directive",
            owning_test: OWNER,
        })
        .collect::<Vec<_>>();
    assert!(compare_policy(original, &allowed).is_closed());

    let added = compare_policy(
        scan_source(
            "build.rs",
            r#"fn main() {
                println!("cargo:rustc-check-cfg=cfg(first)");
                println!("cargo:rustc-check-cfg=cfg(second)");
                println!("cargo:rustc-check-cfg=cfg(third)");
            }"#,
        ),
        &allowed,
    );
    assert_eq!(added.unmatched.len(), 1);
    assert!(!added.is_closed());

    let removed = compare_policy(
        scan_source(
            "build.rs",
            r#"fn main() { println!("cargo:rustc-check-cfg=cfg(first)"); }"#,
        ),
        &allowed,
    );
    assert_eq!(removed.stale.len(), 1);
    assert!(!removed.is_closed());

    let reordered = compare_policy(
        scan_source(
            "build.rs",
            r#"fn main() {
                println!("cargo:rustc-check-cfg=cfg(second)");
                println!("cargo:rustc-check-cfg=cfg(first)");
            }"#,
        ),
        &allowed,
    );
    assert_eq!(reordered.unmatched.len(), 2);
    assert_eq!(reordered.stale.len(), 2);
    assert!(!reordered.is_closed());
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
        owning_test: TestOwner::behavioral(
            "tests/raw_output_policy/self_tests.rs::classified_violation_is_rejected",
            &["src/example.rs"],
            &["compare_policy"],
        ),
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

#[test]
fn scanner_covers_direct_macros_and_writer_methods_through_a_raw_wrapper() {
    let source = r#"
        fn output(ui: &mut Ui) -> RawOutput<&mut dyn io::Write> {
            RawOutput::new(ui.stdout_writer())
        }

        struct RawOutput<W> {
            destination: W,
        }

        impl<W: io::Write> RawOutput<W> {
            fn new(destination: W) -> Self {
                Self { destination }
            }

            fn emit(&mut self) {
                write!(self.destination, "one");
                writeln!(self.destination, "two");
                self.destination.write_all(b"three");
                let chunks = [io::IoSlice::new(b"four")];
                self.destination.write_vectored(&chunks);
                self.destination.write_all_vectored(&mut chunks.as_slice());
            }
        }
    "#;
    let sites = scan_source("src/example.rs", source);
    let direct_writes = sites
        .iter()
        .filter(|site| site.key.primitive == Primitive::DirectWrite)
        .collect::<Vec<_>>();
    assert_eq!(direct_writes.len(), 5, "{sites:#?}");

    let entries = direct_writes
        .into_iter()
        .map(|site| AllowEntry {
            path: "src/example.rs",
            fingerprint: Box::leak(site.key.fingerprint.clone().into_boxed_str()),
            primitive: Primitive::DirectWrite,
            class: OutputClass::Infrastructure,
            rationale: "synthetic specialized raw writer",
            owning_test: TestOwner::behavioral(
                "tests/raw_output_policy/self_tests.rs::scanner_covers_direct_macros_and_writer_methods_through_a_raw_wrapper",
                &["src/example.rs"],
                &["compare_policy"],
            ),
        })
        .collect::<Vec<_>>();
    let direct_sites = sites
        .into_iter()
        .filter(|site| site.key.primitive == Primitive::DirectWrite)
        .collect();
    assert!(compare_policy(direct_sites, &entries).is_closed());
}

#[test]
fn scanner_tracks_raw_writers_through_free_function_parameters() {
    let source = r#"
        fn run(ui: &mut Ui) {
            let destination = ui.stderr_writer();
            emit(destination);
        }

        fn emit(target: &mut impl io::Write) {
            writeln!(target, "error");
        }
    "#;
    let sites = scan_source("src/example.rs", source);
    assert_eq!(
        sites
            .iter()
            .filter(|site| site.key.primitive == Primitive::DirectWrite)
            .count(),
        1,
        "{sites:#?}"
    );
}

#[test]
fn scanner_covers_document_render_regardless_of_binding_names() {
    let source = r#"
        fn emit(page: &Document, palette: RenderContext) {
            let _ = page.render(&palette);
        }
    "#;
    let sites = scan_source("src/example.rs", source);
    assert_eq!(sites.len(), 1, "{sites:#?}");
    assert_eq!(sites[0].key.primitive, Primitive::DocumentRender);
}

#[test]
fn scanner_ignores_format_buffers_and_non_document_renderers() {
    let source = r#"
        fn format_only(
            text: &mut String,
            formatter: &mut std::fmt::Formatter<'_>,
            glyph: Glyph,
            context: RenderContext,
        ) {
            write!(text, "buffered");
            writeln!(formatter, "display");
            let marker = Glyph::Success;
            let _ = glyph.render(&context);
            let _ = marker.render(&context);
            let template = Template::new();
            let _ = template.render(&context);
        }

        struct Template;
        impl Template {
            fn new() -> Self { Self }
            fn render(&self, context: &RenderContext) -> String {
                context.to_string()
            }
        }
    "#;
    assert!(scan_source("src/example.rs", source).is_empty());
}

#[test]
fn scanner_resolves_imported_output_helpers_and_io_aliases() {
    let source = r#"
        use crate::output::{self as raw_output, write_stdout, write_stdout_line as emit_line};
        use std::io::{self as terminal_io, stderr as diagnostics, stdout as terminal, Write};

        fn emit() {
            write_stdout(format_args!("one"));
            emit_line(format_args!("two"));
            raw_output::write_stderr_line(format_args!("three"));
            terminal().write_all(b"four");
            diagnostics().write_fmt(format_args!("five"));
            terminal_io::stdout().write(b"six");
        }
    "#;
    let sites = scan_source("src/example.rs", source);
    assert_eq!(
        sites
            .iter()
            .filter(|site| site.key.primitive == Primitive::OutputRawHelper)
            .count(),
        3,
        "{sites:#?}"
    );
    assert_eq!(
        sites
            .iter()
            .filter(|site| matches!(
                site.key.primitive,
                Primitive::StdoutConstructor | Primitive::StderrConstructor
            ))
            .count(),
        3,
        "{sites:#?}"
    );
    assert_eq!(
        sites
            .iter()
            .filter(|site| site.key.primitive == Primitive::DirectWrite)
            .count(),
        3,
        "{sites:#?}"
    );
}

#[test]
fn scanner_covers_every_byte_writing_write_method_and_ufcs_alias() {
    let source = r#"
        use std::io::{self, Write as ByteSink};

        fn run(ui: &mut Ui) {
            let destination = ui.stdout_writer();
            destination.write(b"one");
            destination.write_vectored(&[io::IoSlice::new(b"two")]);
            destination.write_all(b"three");
            destination.write_all_vectored(&mut [io::IoSlice::new(b"four")].as_slice());
            destination.write_fmt(format_args!("five"));
            ByteSink::write_vectored(destination, &[io::IoSlice::new(b"six")]);
        }
    "#;
    let sites = scan_source("src/example.rs", source);
    assert_eq!(
        sites
            .iter()
            .filter(|site| site.key.primitive == Primitive::DirectWrite)
            .count(),
        6,
        "{sites:#?}"
    );
}

#[test]
fn fingerprints_include_enclosing_machine_eligibility_guards() {
    let machine = scan_source(
        "src/example.rs",
        r#"fn emit(json: bool) { if json { println!("{}", payload()); } }"#,
    );
    let human = scan_source(
        "src/example.rs",
        r#"fn emit(json: bool) { if !json { println!("{}", payload()); } }"#,
    );
    assert_eq!(machine.len(), 1);
    assert_eq!(human.len(), 1);
    assert_ne!(machine[0].key, human[0].key);

    let owner = TestOwner::behavioral(
        "tests/raw_output_policy/self_tests.rs::fingerprints_include_enclosing_machine_eligibility_guards",
        &["src/example.rs"],
        &["compare_policy"],
    );
    let allowlist = [AllowEntry {
        path: "src/example.rs",
        fingerprint: Box::leak(machine[0].key.fingerprint.clone().into_boxed_str()),
        primitive: Primitive::PrintMacro,
        class: OutputClass::MachineProtocol,
        rationale: "synthetic guarded protocol",
        owning_test: owner,
    }];
    let diff = compare_policy(human, &allowlist);
    assert_eq!(diff.unmatched.len(), 1);
    assert_eq!(diff.stale.len(), 1);
}

#[test]
fn owning_test_requires_exact_test_attribute_and_behavioral_contract() {
    let names = owning_test::runnable_test_function_names(
        r#"
            #[cfg(test)]
            fn helper_only() { assert!(true); }
            #[test]
            fn runnable() { assert!(true); }
            #[test]
            #[ignore]
            fn ignored() { assert!(true); }
        "#,
    );
    assert_eq!(names, vec!["runnable"]);

    let sites = scan_source("src/example.rs", "fn emit() { println!(\"value\"); }");
    let unrelated = AllowEntry {
        path: "src/example.rs",
        fingerprint: Box::leak(sites[0].key.fingerprint.clone().into_boxed_str()),
        primitive: Primitive::PrintMacro,
        class: OutputClass::MachineProtocol,
        rationale: "synthetic protocol",
        owning_test: TestOwner::behavioral(
            "tests/raw_output_policy/self_tests.rs::normalized_fingerprint_ignores_whitespace_and_comments",
            &["src/example.rs"],
            &["protocol_receipt"],
        ),
    };
    let diff = compare_policy(sites, &[unrelated]);
    assert_eq!(diff.invalid_metadata.len(), 1);
    assert!(diff.invalid_metadata[0]
        .1
        .contains("missing behavioral evidence"));
}

#[test]
fn owning_test_requires_a_resolvable_source_identity() {
    let sites = scan_source("src/example.rs", "fn emit() { println!(\"value\"); }");
    let fingerprint = Box::leak(sites[0].key.fingerprint.clone().into_boxed_str());
    let arbitrary = AllowEntry {
        path: "src/example.rs",
        fingerprint,
        primitive: Primitive::PrintMacro,
        class: OutputClass::MachineProtocol,
        rationale: "synthetic protocol",
        owning_test: TestOwner::behavioral(
            "some nonempty prose",
            &["src/example.rs"],
            &["compare_policy"],
        ),
    };
    let missing = AllowEntry {
        owning_test: TestOwner::behavioral(
            "tests/raw_output_policy/self_tests.rs::not_a_real_test",
            &["src/example.rs"],
            &["compare_policy"],
        ),
        ..arbitrary
    };
    let arbitrary_diff = compare_policy(sites.clone(), &[arbitrary]);
    assert_eq!(arbitrary_diff.invalid_metadata.len(), 1);
    assert!(!arbitrary_diff.is_closed());
    let missing_diff = compare_policy(sites, &[missing]);
    assert_eq!(missing_diff.invalid_metadata.len(), 1);
    assert!(!missing_diff.is_closed());
}
