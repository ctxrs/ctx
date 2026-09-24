//! Grammar-backed C, C++, Java, C#, Kotlin, and Swift facts.
//!
//! Resolution is deliberately syntactic: explicit packages/namespaces and imports,
//! lexical definitions, and nonvirtual members of explicitly typed receivers. No
//! preprocessing, build-system module inference, overload selection, or execution.
//! CUDA uses its C++-derived grammar. Metal and C++/CLI accept a documented
//! declaration subset through byte-preserving normalization, never preprocessing.
use super::common::{Extractor, children, module_path, relative_path, tree};
use crate::model::FileFacts;
use anyhow::{Result, bail};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::{Language, Node as Syntax};

pub fn supports(path: &str) -> bool {
    language(path).is_some()
}

pub fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    let Some((mut lang, mut grammar)) = language(path) else {
        return Ok(None);
    };
    if path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        bail!("source path must be a normalized relative POSIX path");
    }
    let mut normalized = if (lang == "cpp" || path.ends_with(".h"))
        && !path.ends_with(".cu")
        && !path.ends_with(".cuh")
        && source.len() <= crate::parser::MAX_SOURCE_BYTES
    {
        normalize_dialect(source, path.ends_with(".metal"))
    } else {
        None
    };
    if lang == "kotlin" && source.len() <= crate::parser::MAX_SOURCE_BYTES {
        normalized = compact_kotlin(source);
    }
    let tests = if lang == "cpp" && source.len() <= crate::parser::MAX_SOURCE_BYTES {
        recover_string_tests(source, &mut normalized)
    } else {
        vec![]
    };
    let parse_source = normalized.as_ref().map_or(source, |n| n.source.as_str());
    if path.ends_with(".h")
        && source.len() <= crate::parser::MAX_SOURCE_BYTES
        && cpp_header(parse_source)?
    {
        lang = "cpp";
        grammar = tree_sitter_cpp::LANGUAGE.into();
    }
    let mut e = Extractor::new(path, source, hash, lang, module_path(path));
    let Some(tree) = tree(grammar, parse_source, &mut e.facts)? else {
        return Ok(Some(e.facts));
    };
    let root = tree.root_node();
    e.root(root, format!("{lang}:file:{path}"));
    e.facts.nodes[0].metadata["cross_project_public"] = json!(false);
    if let Some(normalized) = &normalized {
        e.facts.nodes[0].metadata["dialect"] = json!(normalized.dialect);
        e.facts.nodes[0].metadata["normalization"] = json!(normalized.spans);
    } else if path.ends_with(".cu") || path.ends_with(".cuh") {
        e.facts.nodes[0].metadata["dialect"] = json!("cuda");
    }
    if lang == "cpp" && path.ends_with(".h") {
        e.facts.nodes[0].metadata["binding_aliases"] = json!([format!("c:file:{path}")]);
        e.facts.nodes[0].metadata["header_grammar"] = json!("C++ syntax markers");
    }
    let mut x = Compiled {
        e,
        scopes: HashMap::new(),
        pending: vec![],
        includes: vec![],
        tests,
        swift_callables: SwiftCallables::default(),
    };
    let package = children(root)
        .into_iter()
        .find(|n| {
            matches!(
                n.kind(),
                "package_declaration" | "package_header" | "file_scoped_namespace_declaration"
            )
        })
        .and_then(|n| {
            n.child_by_field_name("name").or_else(|| {
                children(n).into_iter().find(|c| {
                    matches!(
                        c.kind(),
                        "identifier" | "scoped_identifier" | "qualified_identifier"
                    )
                })
            })
        })
        .and_then(|n| x.name(n))
        .unwrap_or_default();
    // A file path is an explicit unit identity; directory names are not modules.
    let prefix = if package.is_empty() {
        format!("@{path}")
    } else {
        package.clone()
    };
    x.scopes.insert(
        0,
        Context {
            namespace: prefix.clone(),
            prefix,
            class: None,
            abstract_members: false,
            local: false,
            bindings: HashMap::new(),
            uncertain: false,
        },
    );
    x.e.facts.nodes[0].metadata["package"] = json!(package);
    x.e.facts.nodes[0].metadata["module_context_required"] = json!(lang == "swift");
    x.e.facts.nodes[0].metadata["unit"] = json!(module_path(path));
    x.e.facts.nodes[0].metadata["imports"] = json!([]);
    if lang == "swift" {
        x.e.facts.nodes[0].metadata["swift_imports"] = json!([]);
    }
    for child in children(root) {
        x.visit(child, 0);
    }
    x.finish();
    Ok(Some(x.e.facts))
}

#[derive(Clone)]
enum Binding {
    Symbol(String),
    Type(String),
    Value(Option<String>),
    Module(String),
    TypeParameter,
    Unknown,
}
#[derive(Clone)]
struct Context {
    prefix: String,
    namespace: String,
    class: Option<(String, bool)>, // qualified owner, closed dispatch
    abstract_members: bool,
    local: bool,
    bindings: HashMap<String, Binding>,
    uncertain: bool,
}
struct Pending<'t> {
    node: Syntax<'t>,
    scope: usize,
    label: String,
    relation: &'static str,
    target: Option<String>,
    constructor: bool,
    context: Option<&'static str>,
}
#[derive(Clone)]
struct SwiftFunctionValue {
    scope: usize,
    name: String,
    key: String,
    node: usize,
}
struct SwiftCallableLocal {
    declaration: (usize, usize),
    assignment: (usize, usize),
    value: Option<SwiftFunctionValue>,
    mutable: bool,
    blocked: bool,
}
struct SwiftCallableCall {
    binding: (usize, String),
    assignment: (usize, usize),
    value: Option<SwiftFunctionValue>,
}
#[derive(Default)]
struct SwiftCallables<'t> {
    functions: HashMap<String, Option<usize>>,
    locals: HashMap<(usize, String), SwiftCallableLocal>,
    calls: HashMap<(usize, usize), SwiftCallableCall>,
    uses: Vec<(Syntax<'t>, usize)>,
    unbound_writes: Vec<(usize, String)>,
    escaped: HashSet<(usize, String)>,
    conditional_depth: HashMap<usize, usize>,
    conditional_owners: HashSet<String>,
}
struct Compiled<'s, 't> {
    e: Extractor<'s>,
    scopes: HashMap<usize, Context>,
    pending: Vec<Pending<'t>>,
    includes: Vec<String>,
    tests: Vec<StringTest>,
    swift_callables: SwiftCallables<'t>,
}

/// Apply exact module identities supplied by project configuration. This helper
/// neither discovers directories nor executes Package.swift. `imports` maps the
/// spelling of explicitly imported modules to unique project-qualified identities.
/// Internal declarations use same-module keys; only explicitly public/open
/// declarations expose foreign-module aliases. File-private/local keys and
/// uncertain references remain untouched. Extension methods are module-local
/// because this helper cannot prove the extended type's exported visibility.
pub fn apply_swift_context(facts: &mut FileFacts, module: &str, imports: &[(String, String)]) {
    if module.is_empty()
        || facts
            .nodes
            .first()
            .is_none_or(|n| n.metadata["language"] != "swift")
    {
        return;
    }
    let imported: HashSet<String> = facts.nodes[0].metadata["swift_imports"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect();
    let mut mappings: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (name, identity) in imports {
        if imported.contains(name) && !identity.is_empty() {
            mappings.entry(name).or_default().insert(identity);
        }
    }
    let unique: HashMap<&str, &str> = mappings
        .iter()
        .filter(|(_, ids)| ids.len() == 1)
        .map(|(name, ids)| (*name, *ids.iter().next().unwrap()))
        .collect();
    let own = format!("@{}.", facts.path);
    let callable_values: HashSet<String> = facts
        .nodes
        .iter()
        .flat_map(|node| {
            node.metadata["swift_callable_values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|proof| proof["reference_id"].as_str().map(str::to_owned))
        })
        .collect();
    let remap = |key: &str| -> Option<String> {
        let rest = key.strip_prefix("swift:")?;
        let (kind, symbol) = rest.split_once(':')?;
        if let Some(symbol) = symbol.strip_prefix(&own) {
            return Some(swift_module_key(kind, module, symbol));
        }
        let (name, symbol) = symbol.strip_prefix('!')?.split_once('.')?;
        Some(swift_export_key(kind, unique.get(name)?, symbol))
    };
    for node in &mut facts.nodes {
        let mut exported = vec![];
        if node.metadata["swift_exported"] == true {
            for key in node.binding_key.iter().map(String::as_str).chain(
                node.metadata["binding_aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str()),
            ) {
                if let Some((kind, symbol)) =
                    key.strip_prefix("swift:").and_then(|k| k.split_once(':'))
                    && let Some(symbol) = symbol.strip_prefix(&own)
                {
                    exported.push(json!(swift_export_key(kind, module, symbol)));
                }
            }
        }
        if let Some(key) = node.binding_key.as_mut()
            && let Some(mapped) = remap(key)
        {
            *key = mapped;
        }
        if let Some(aliases) = node
            .metadata
            .get_mut("binding_aliases")
            .and_then(serde_json::Value::as_array_mut)
        {
            for alias in aliases {
                if let Some(mapped) = alias.as_str().and_then(&remap) {
                    *alias = json!(mapped);
                }
            }
        }
        if !exported.is_empty() {
            if !node.metadata["binding_aliases"].is_array() {
                node.metadata["binding_aliases"] = json!([]);
            }
            node.metadata["binding_aliases"]
                .as_array_mut()
                .unwrap()
                .extend(exported);
        }
        node.metadata["swift_module"] = json!(module);
    }
    for reference in &mut facts.references {
        let mut keys = vec![];
        for key in &reference.candidate_keys {
            keys.push(remap(key).unwrap_or_else(|| key.clone()));
            // One imported module is an explicit fallback. Multiple imports are
            // an unordered search space and cannot be Store's priority list.
            if imported.len() == 1
                && unique.len() == 1
                // A copied local function already names an exact declaration;
                // importing a same-named function cannot replace that identity.
                && !callable_values.contains(&reference.id)
                && let Some((kind, symbol)) =
                    key.strip_prefix("swift:").and_then(|s| s.split_once(':'))
                && let Some(symbol) = symbol.strip_prefix(&own)
            {
                let imported_module = unique.values().next().unwrap();
                if *imported_module != module {
                    keys.push(swift_export_key(kind, imported_module, symbol));
                }
            }
        }
        let mut seen = HashSet::new();
        keys.retain(|key| seen.insert(key.clone()));
        reference.candidate_keys = keys;
    }
    let candidates: HashMap<_, _> = facts
        .references
        .iter()
        .map(|r| (r.id.as_str(), &r.candidate_keys))
        .collect();
    for node in &mut facts.nodes {
        if let Some(types) = node
            .metadata
            .get_mut("type_references")
            .and_then(serde_json::Value::as_array_mut)
        {
            for evidence in types {
                if let Some(keys) = evidence["reference_id"]
                    .as_str()
                    .and_then(|id| candidates.get(id))
                {
                    evidence["candidate_keys"] = json!(keys);
                }
            }
        }
    }
    facts.nodes[0].metadata["module_context_required"] = json!(false);
    facts.nodes[0].metadata["swift_module_imports"] = json!(unique);
}

struct DialectSource {
    source: String,
    dialect: &'static str,
    spans: Vec<serde_json::Value>,
}
#[derive(Clone, Copy)]
struct DialectToken<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    opaque: bool,
}

// A lexical pass only: never interpret comments, literals or macro bodies as
// dialect syntax. The grammar still validates the complete resulting source.

struct StringTest {
    start: usize,
    label: String,
    macro_name: String,
}

// Repair only the omitted separator after an abstract interface signature. A
// same-line space becomes ';'; original byte offsets and all line breaks survive.

/// Inventory-derived navigation. Call after exact Swift context has been applied.
/// Omitted unit identities permit only facts within the same file. Rebuild this
/// inventory when its fingerprint changes; apply to fresh (not persisted) facts.
#[derive(Default)]
pub struct CompiledContext {
    fingerprint: String,
    aliases: BTreeMap<String, Vec<String>>,
    candidates: BTreeMap<String, Vec<String>>,
    relations: BTreeMap<String, String>,
    references: BTreeMap<String, Vec<crate::model::Reference>>,
}
struct CompiledInventory {
    nodes: BTreeMap<String, crate::model::Node>,
    units: BTreeMap<String, String>,
    parents: BTreeMap<String, String>,
    types: BTreeMap<(String, String), Vec<String>>,
    bases: BTreeMap<String, Option<Vec<String>>>,
}

mod compiled_name;
mod compiled_prototype;
mod compiled_visit;
mod compiledcontext_new;
mod compiledinventory_unit;

mod language;
use language::*;
mod compiled_helpers;
use compiled_helpers::*;
