use super::*;

#[test]
fn rust_public_uses_requires_the_module_traversal_snapshot() {
    let declared = "mod child; pub fn caller() { crate::child::grand::work(); }";
    for changed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/child")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='demo'\nversion='0.1.0'\n",
        )
        .unwrap();
        let first = if changed {
            "pub fn caller() {}"
        } else {
            declared
        };
        std::fs::write(root.path().join("src/lib.rs"), first).unwrap();
        std::fs::write(root.path().join("src/child/grand.rs"), "pub fn work() {}").unwrap();
        let mut paths: Vec<String> = ["Cargo.toml", "src/lib.rs", "src/child/grand.rs"]
            .map(String::from)
            .into();
        if !changed {
            std::fs::write(root.path().join("src/child.rs"), "pub mod grand;").unwrap();
            paths.push("src/child.rs".into());
        }
        let inventory = Inventory::new(root.path(), &paths);
        let mut context = RustContext {
            crates: BTreeMap::from([(
                String::new(),
                RustCrate {
                    name: "demo".into(),
                    library: "demo".into(),
                    root: Some("src/lib.rs".into()),
                    dependencies: BTreeMap::new(),
                    modules: BTreeSet::new(),
                    public_modules: BTreeSet::new(),
                    unavailable_modules: BTreeSet::new(),
                },
            )]),
            owners: paths
                .iter()
                .filter(|path| path.ends_with(".rs"))
                .map(|path| (path.clone(), String::new()))
                .collect(),
            ..Default::default()
        };
        context
            .visit_module(&inventory, "", "src/lib.rs", "", true, 0)
            .unwrap();
        let traversal_hashes = context.source_hashes.clone();
        assert_eq!(
            traversal_hashes["src/lib.rs"],
            blake3::hash(first.as_bytes()).to_hex().as_str()
        );
        if changed {
            // The new declaration rejects a missing parent, but traversal
            // has already observed the earlier source without that module.
            std::fs::write(root.path().join("src/lib.rs"), declared).unwrap();
            let error = context.public_uses(&inventory).unwrap_err();
            assert!(error.to_string().contains(
                "source changed during Rust context discovery; retry indexing: src/lib.rs"
            ));
            assert!(context.facts.is_empty());
        } else {
            context.public_uses(&inventory).unwrap();
            assert_eq!(context.facts.len(), 3);
            assert!(
                context.facts["src/lib.rs"]
                    .references
                    .iter()
                    .any(|reference| {
                        reference.relation == "calls" && !reference.candidate_keys.is_empty()
                    })
            );
            assert!(
                context.facts["src/child/grand.rs"]
                    .nodes
                    .iter()
                    .any(|node| { node.label == "work" && node.kind == "function" })
            );
        }
        // Neither acceptance nor rejection may replace the traversal proof.
        assert_eq!(context.source_hashes, traversal_hashes);
    }
}

#[test]
fn rust_cached_tokens_include_final_impl_checks_and_preserve_source_guards() {
    let root = tempfile::tempdir().unwrap();
    let sources = [
        ("Cargo.toml", "[package]\nname='demo'\nversion='0.1.0'\n"),
        ("src/lib.rs", "mod model; mod provider;"),
        ("src/model.rs", "pub struct Register<A>(pub A);"),
        (
            "src/provider.rs",
            "use crate::model::Register; impl<A, B> Register<A, B> { pub fn work() {} } // a",
        ),
    ];
    for (path, source) in sources {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let mut paths: Vec<String> = sources.iter().map(|(path, _)| (*path).into()).collect();
    let mut context =
        ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new()).unwrap();
    paths.reverse();
    let reordered =
        ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new()).unwrap();
    let path = "src/provider.rs";
    let source = sources[3].1;
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let token = context.fingerprint(path);
    assert_eq!(token, reordered.fingerprint(path));
    let mut cached = context.take_cached_facts(path, &hash).unwrap().unwrap();
    assert!(
        cached
            .nodes
            .iter()
            .find(|node| node.label == "work")
            .unwrap()
            .binding_key
            .is_some()
    );
    assert_ne!(
        token,
        outcome_fingerprint("rust-output-v1", cached.clone()).unwrap()
    );
    context.apply(&mut cached);
    assert!(
        cached
            .nodes
            .iter()
            .find(|node| node.label == "work")
            .unwrap()
            .binding_key
            .is_none()
    );
    assert_eq!(
        token,
        outcome_fingerprint("rust-output-v1", cached).unwrap()
    );
    assert!(context.take_cached_facts(path, &hash).unwrap().is_none());
    assert_eq!(token, context.fingerprint(path));

    // Same-size, comment-only edits still invalidate the discovery snapshot,
    // even after cached facts have been consumed.
    std::fs::write(root.path().join(path), source.replace("// a", "// b")).unwrap();
    let (changed_hash, _) = crate::index::read_source(
        &root.path().join(path),
        crate::parser::MAX_SOURCE_BYTES as u64,
    )
    .unwrap();
    assert!(context.validate_source(path, &changed_hash).is_err());
    assert!(context.take_cached_facts(path, &changed_hash).is_err());
    let edited =
        ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new()).unwrap();
    assert_eq!(token, edited.fingerprint(path));
    std::fs::write(root.path().join(path), source).unwrap();
    let (restored_hash, _) = crate::index::read_source(
        &root.path().join(path),
        crate::parser::MAX_SOURCE_BYTES as u64,
    )
    .unwrap();
    assert_eq!(restored_hash, hash);
    assert!(context.validate_source(path, &restored_hash).is_ok());
    assert!(edited.validate_source(path, &restored_hash).is_err());
}

#[test]
fn javascript_provider_scratch_preserves_raw_facts_and_caller_outcomes() {
    let root = tempfile::tempdir().unwrap();
    let caller = "import {Shape, ordinary} from './provider'; function use(value: Shape): Shape { Shape(); ordinary(); return value; }";
    std::fs::write(root.path().join("main.ts"), caller).unwrap();
    let paths = ["provider.ts".into(), "main.ts".into()];
    let mut previous = None;
    for private_count in [0, 16, 0] {
        let mut provider = String::from(
            "export interface Shape {}\nexport const Shape = factory();\nexport function ordinary() {}\nfunction privateScope() {\nfunction local() {} local();\n",
        );
        for i in 0..private_count {
            provider.push_str(&format!("function helper_{i}() {{}} helper_{i}();\n"));
        }
        provider.push_str("}\n");
        std::fs::write(root.path().join("provider.ts"), &provider).unwrap();
        let mut context =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        let provider_hash = blake3::hash(provider.as_bytes()).to_hex().to_string();
        let raw = crate::languages::parse("provider.ts", &provider, &provider_hash)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&raw).unwrap(),
            serde_json::to_value(&context.javascript.raw_facts["provider.ts"]).unwrap()
        );
        let caller_hash = blake3::hash(caller.as_bytes()).to_hex().to_string();
        let mut applied = context
            .take_cached_facts("main.ts", &caller_hash)
            .unwrap()
            .unwrap();
        context.apply(&mut applied);
        let outcome = (
            context.javascript.imported_callees.clone(),
            context.fingerprint("main.ts"),
            serde_json::to_value(applied).unwrap(),
        );
        if let Some(previous) = previous {
            assert_eq!(previous, outcome);
        }
        previous = Some(outcome);
    }
}

#[test]
fn javascript_imported_declarations_require_unique_complete_provider_proof() {
    let parse = |path: &str, source: &str| {
        crate::languages::parse(path, source, "raw")
            .unwrap()
            .unwrap()
    };
    let provider = parse(
        "provider.ts",
        "export interface Shape {} export const Shape = factory(); export function ordinary() {}",
    );
    let other = parse("other.ts", "export const Shape = factory();");
    let caller = parse(
        "main.ts",
        "import {Shape as Value} from './provider'; function use(v: Value): Value { Value(); return v; }",
    );
    for variation in [
        "valid",
        "missing marker",
        "false marker",
        "wrong kind",
        "wrong key",
        "competing owner",
        "ordinary candidate",
        "other declaration",
        "unproven candidate",
    ] {
        let mut raw = provider.clone();
        let constant = raw.nodes.iter().position(|n| n.kind == "constant").unwrap();
        match variation {
            "missing marker" => {
                raw.nodes[constant]
                    .metadata
                    .as_object_mut()
                    .unwrap()
                    .remove("declared_callee_binding");
            }
            "false marker" => {
                raw.nodes[constant].metadata["declared_callee_binding"] = false.into()
            }
            "wrong kind" => raw.nodes[constant].kind = "function".into(),
            "wrong key" => {
                raw.nodes[constant].binding_key = Some("javascript:provider:Shape".into())
            }
            "competing owner" => {
                let mut competitor = raw.nodes[constant].clone();
                competitor.id.push_str(":competitor");
                competitor.kind = "function".into();
                competitor.metadata["declared_callee_binding"] = false.into();
                raw.nodes.push(competitor);
            }
            _ => (),
        }
        let mut context = JavascriptContext {
            files: ["provider.ts", "other.ts", "main.ts"]
                .map(String::from)
                .into(),
            raw_facts: [
                ("provider.ts".into(), raw),
                ("other.ts".into(), other.clone()),
            ]
            .into(),
            ..Default::default()
        };
        context.exports();
        let mut facts = caller.clone();
        context.apply_paths(&mut facts);
        let call = facts
            .references
            .iter_mut()
            .find(|r| r.relation == "calls")
            .unwrap();
        match variation {
            "ordinary candidate" => call
                .candidate_keys
                .push("javascript:file:provider.ts:ordinary".into()),
            "other declaration" => call
                .candidate_keys
                .push("javascript:file:other.ts:Shape".into()),
            "unproven candidate" => call
                .candidate_keys
                .push("javascript:file:absent.ts:Shape".into()),
            _ => (),
        }
        let original = call.clone();
        let types: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.relation != "calls")
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        context.apply_imported_callees(&mut facts);
        let calls: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.relation == "calls")
            .collect();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].candidate_keys.is_empty(), "{variation}");
        let mut runtime = original.clone();
        runtime.candidate_keys.clear();
        assert_eq!(
            serde_json::to_value(calls[0]).unwrap(),
            serde_json::to_value(runtime).unwrap()
        );
        let siblings: Vec<_> = facts
            .references
            .iter()
            .filter(|r| r.relation == "declared_callee")
            .collect();
        assert_eq!(
            siblings.len(),
            usize::from(variation == "valid"),
            "{variation}"
        );
        if let Some(sibling) = siblings.first() {
            assert_eq!(sibling.id, format!("{}:declared_callee", original.id));
            assert_eq!(
                (&sibling.source, &sibling.label, &sibling.file, sibling.line),
                (
                    &original.source,
                    &original.label,
                    &original.file,
                    original.line
                )
            );
            assert_eq!(
                sibling.candidate_keys,
                ["javascript:file:provider.ts:Shape#declared_callee"]
            );
            assert_eq!(
                sibling.reason,
                "written immutable callee binding; factory result and runtime dispatch are unresolved"
            );
        }
        assert_eq!(
            types,
            facts
                .references
                .iter()
                .filter(|r| !matches!(r.relation.as_str(), "calls" | "declared_callee"))
                .map(|r| serde_json::to_value(r).unwrap())
                .collect::<Vec<_>>()
        );
        let once = serde_json::to_value(&facts).unwrap();
        context.apply_imported_callees(&mut facts);
        assert_eq!(once, serde_json::to_value(facts).unwrap());
    }
}

#[test]
fn javascript_cache_returns_raw_facts_once_and_retains_output_tokens() {
    let root = tempfile::tempdir().unwrap();
    let sources = [
        ("lib.cjs", "exports.work = function work() {};"),
        (
            "main.cjs",
            "const lib = require('./lib.cjs'); function use() { lib.work(); }",
        ),
        (
            "typed.ts",
            "import {work} from './lib.cjs'; function use() { work(); }",
        ),
        ("broken.ts", "export function ("),
    ];
    for (path, source) in sources {
        std::fs::write(root.path().join(path), source).unwrap();
    }
    let mut paths: Vec<String> = sources.iter().map(|(p, _)| (*p).into()).collect();
    let mut context =
        ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new()).unwrap();
    paths.reverse();
    let reordered =
        ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new()).unwrap();
    for (path, source) in sources {
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let raw = crate::languages::parse(path, source, &hash)
            .unwrap()
            .unwrap();
        let token = context.fingerprint(path);
        assert_eq!(token, reordered.fingerprint(path));
        let mut cached = context.take_cached_facts(path, &hash).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&cached).unwrap(),
            serde_json::to_value(&raw).unwrap()
        );
        let mut expected = raw;
        context.apply(&mut expected);
        context.apply(&mut cached);
        assert_eq!(
            serde_json::to_value(&cached).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(
            token,
            JavascriptContext::outcome_fingerprint(cached).unwrap()
        );
        assert!(context.take_cached_facts(path, &hash).unwrap().is_none());
        assert_eq!(token, context.fingerprint(path));
        assert!(context.validate_source(path, "different bytes").is_err());
    }
}

#[test]
fn javascript_context_rejects_changed_bytes_and_configuration_observations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lib.ts");
    let old = "export function old() {}";
    let new = "export function new() {}";
    std::fs::write(&path, new).unwrap();
    std::fs::write(root.path().join("bad.js"), [255]).unwrap();
    std::fs::write(
        root.path().join("large.js"),
        vec![b' '; crate::parser::MAX_SOURCE_BYTES + 1],
    )
    .unwrap();
    std::fs::write(root.path().join("tool"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(
        root.path().join("tsconfig.json"),
        r#"{"extends":"./base.json"}"#,
    )
    .unwrap();
    std::fs::write(root.path().join("base.json"), "{}").unwrap();
    let paths = [
        "lib.ts",
        "bad.js",
        "large.js",
        "tool",
        "tsconfig.json",
        "base.json",
    ]
    .map(String::from);
    let mut context =
        ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new()).unwrap();
    assert!(!context.javascript.files.contains("tool"));
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, old).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    for (name, bytes) in [
        ("lib.ts", old.as_bytes()),
        ("bad.js", b"x".as_slice()),
        ("large.js", b"export {};".as_slice()),
        ("tool", b"#!/usr/bin/env node\nrun();".as_slice()),
    ] {
        if name != "lib.ts" {
            std::fs::write(root.path().join(name), bytes).unwrap();
        }
        let (hash, _) = crate::index::read_source(
            &root.path().join(name),
            crate::parser::MAX_SOURCE_BYTES as u64,
        )
        .unwrap();
        assert!(context.validate_source(name, &hash).is_err(), "{name}");
        assert!(context.take_cached_facts(name, &hash).is_err(), "{name}");
    }
    assert!(context.javascript.raw_facts.contains_key("lib.ts"));
    let inventory = Inventory::new(root.path(), &paths);
    context.javascript.validate_configs(&inventory).unwrap();
    for (name, contents) in [
        ("base.json", "{ }"),
        ("package.json", "{}"),
        ("base.json", "{}"),
    ] {
        std::fs::write(root.path().join(name), contents).unwrap();
        if name == "base.json" && contents == "{}" {
            std::fs::remove_file(root.path().join("package.json")).unwrap();
            context.javascript.validate_configs(&inventory).unwrap();
        } else {
            assert!(context.javascript.validate_configs(&inventory).is_err());
            assert!(
                context
                    .validate_source(name, blake3::hash(contents.as_bytes()).to_hex().as_ref())
                    .is_err()
            );
        }
    }
    std::fs::remove_file(root.path().join("base.json")).unwrap();
    assert!(context.javascript.validate_configs(&inventory).is_err());
}

#[test]
fn javascript_output_token_hashes_complete_facts_except_the_source_stamp() {
    let raw = crate::languages::parse(
        "main.ts",
        "import {work} from './lib'; function use() { work(); }",
        "raw",
    )
    .unwrap()
    .unwrap();
    let token = JavascriptContext::outcome_fingerprint(raw.clone()).unwrap();
    let mut reordered = raw.clone();
    reordered.hash = "final-stamp".into();
    reordered.nodes.reverse();
    reordered.edges.reverse();
    reordered.references.reverse();
    assert_eq!(
        token,
        JavascriptContext::outcome_fingerprint(reordered).unwrap()
    );
    let call = raw
        .references
        .iter()
        .position(|r| r.relation == "calls")
        .unwrap();
    for field in ["candidate", "reason", "line", "metadata", "diagnostic"] {
        let mut changed = raw.clone();
        match field {
            "candidate" => changed.references[call]
                .candidate_keys
                .push("another:target".into()),
            "reason" => changed.references[call].reason.push('!'),
            "line" => changed.references[call].line += 1,
            "metadata" => changed.nodes[0].metadata["proof"] = true.into(),
            _ => changed.diagnostics.push(crate::model::Diagnostic {
                file: "main.ts".into(),
                line: Some(1),
                message: "unsupported".into(),
            }),
        }
        assert_ne!(
            token,
            JavascriptContext::outcome_fingerprint(changed).unwrap(),
            "{field}"
        );
    }
    let mut ordered = raw;
    ordered.references[call].candidate_keys = vec!["first".into(), "second".into()];
    let first = JavascriptContext::outcome_fingerprint(ordered.clone()).unwrap();
    ordered.references[call].candidate_keys.reverse();
    assert_ne!(
        first,
        JavascriptContext::outcome_fingerprint(ordered).unwrap()
    );
}

#[test]
fn swiftpm_literal_ast_comments_and_raw_snapshot_validation() {
    let source = r#"// swift-tools-version: 6.0
import PackageDescription
let /* binding */ package = Package(
    name: "Example", platforms: [.macOS(.v13)],
    products: [.library(name: "Core", targets: ["Core"])],
    dependencies: [.package(url: "https://example.invalid/library", from: "1.0.0")],
    targets: [/* .target(name: "Fake") */ .target(/* argument */ name: "Core"),]
)
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let targets =
        swift_package(source, "").unwrap_or_else(|| panic!("{}", tree.root_node().to_sexp()));
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].root, "Sources/Core");
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Package.swift"), source).unwrap();
    let paths = vec!["Package.swift".into()];
    let inventory = Inventory::new(root.path(), &paths);
    let swift = SwiftContext::discover(&inventory, &BTreeMap::new()).unwrap();
    let changed = format!("{source}// only a comment changed\n");
    let hash = |s: &str| blake3::hash(s.as_bytes()).to_hex().to_string();
    swift
        .validate_source("Package.swift", &hash(source))
        .unwrap();
    assert!(
        swift
            .validate_source("Package.swift", &hash(&changed))
            .is_err()
    );
    std::fs::write(root.path().join("Package.swift"), &changed).unwrap();
    let mut context = ProjectContext {
        swift,
        ..Default::default()
    };
    assert!(
        context
            .compiled_inventory(&inventory)
            .unwrap_err()
            .to_string()
            .contains("Swift manifest")
    );
    let refreshed = SwiftContext::discover(&inventory, &BTreeMap::new()).unwrap();
    assert_ne!(context.swift.fingerprint, refreshed.fingerprint);
    // Uninventoried manifests are never probed, even though one exists on disk.
    let inventory = Inventory::new(root.path(), &[]);
    assert!(
        SwiftContext::discover(&inventory, &BTreeMap::new())
            .unwrap()
            .source_hashes
            .is_empty()
    );
}

#[test]
fn compiled_and_template_proofs_reject_changed_source_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let public = "namespace api; public class Base { public void work() {} }";
    let private = "namespace api; public class Base { private void work() {} }";
    std::fs::write(root.path().join("App.csproj"), "<Project/>").unwrap();
    std::fs::write(root.path().join("Base.cs"), public).unwrap();
    let pascal = "unit Proof; interface implementation end.";
    std::fs::write(root.path().join("Proof.pas"), pascal).unwrap();
    let paths = vec!["App.csproj".into(), "Base.cs".into(), "Proof.pas".into()];
    let database = tempfile::tempdir().unwrap();
    let db = database.path().join("graph.db");
    crate::index::run_with_options(
        root.path(),
        &db,
        &crate::index::IndexOptions {
            code_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    let snapshot = || {
        serde_json::to_value(crate::store::Store::open(&db).unwrap().snapshot().unwrap()).unwrap()
    };
    let previous_graph = snapshot();
    let mut context =
        ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new()).unwrap();
    let hash = |source: &str| blake3::hash(source.as_bytes()).to_hex().to_string();
    context.validate_source("Base.cs", &hash(public)).unwrap();
    context.validate_source("Proof.pas", &hash(pascal)).unwrap();
    assert!(
        context
            .validate_source(
                "Proof.pas",
                &hash("unit Changed; interface implementation end.")
            )
            .is_err()
    );
    std::fs::write(root.path().join("Base.cs"), private).unwrap();
    let (changed, _) = crate::index::read_source(
        &root.path().join("Base.cs"),
        crate::parser::MAX_SOURCE_BYTES as u64,
    )
    .unwrap();
    assert!(
        context
            .validate_source("Base.cs", &changed)
            .unwrap_err()
            .to_string()
            .contains("retry indexing")
    );
    // A later compiled read must also reject a stale earlier template inventory.
    let inventory = Inventory::new(root.path(), &paths);
    assert!(
        context
            .compiled_inventory(&inventory)
            .unwrap_err()
            .to_string()
            .contains("template context")
    );
    assert!(
        context
            .validate_source("App.csproj", &hash("<Project><PropertyGroup/></Project>"))
            .is_err()
    );
    context
        .validate_source("Uninventoried.cs", &hash(private))
        .unwrap();
    assert_eq!(
        snapshot(),
        previous_graph,
        "rejected discovery must preserve the stored graph"
    );
}
