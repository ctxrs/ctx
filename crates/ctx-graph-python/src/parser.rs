use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tree_sitter::{Node as Syntax, Parser};
use unicode_normalization::UnicodeNormalization;

use crate::model::*;

pub use ctx_graph_types::MAX_SOURCE_BYTES;

pub fn empty_facts(path: &str, hash: &str) -> FileFacts {
    let module = path.strip_prefix("src/").unwrap_or(path);
    let module = module.strip_suffix(".py").unwrap_or(module);
    let module = module.strip_suffix("/__init__").unwrap_or(module);
    let module = if module == "__init__" { "" } else { module };
    FileFacts {
        path: path.into(),
        hash: hash.into(),
        module: module.replace('/', "."),
        nodes: vec![],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    }
}

#[derive(Clone)]
enum Binding {
    Definition {
        key: String,
        id: String,
        start: usize,
    },
    Symbol {
        key: String,
        start: usize,
    },
    Module {
        module: String,
        prefix: String,
        start: usize,
    },
    Receiver {
        class: usize,
        method: usize,
    },
    Unknown,
}

#[derive(Clone, Copy, PartialEq)]
enum ScopeKind {
    Module,
    Class,
    Function,
    Opaque,
}

struct Scope {
    parent: Option<usize>,
    kind: ScopeKind,
    owner: String,
    qualified: String,
    bindings: HashMap<String, Binding>,
    uncertain: bool,
}

struct PendingCall {
    scope: usize,
    start: usize,
    end: usize,
    line: u32,
    parts: Vec<String>,
}

struct PendingReference {
    site: PendingCall,
    owner: String,
    relation: &'static str,
    context: &'static str,
}

struct CallableWrite {
    start: usize,
    end: usize,
    name: String,
    value: Option<String>,
}

#[derive(Default)]
struct CallableFlow {
    writes: Vec<CallableWrite>,
    calls: HashSet<usize>,
    blocked: HashSet<String>,
    opaque: bool,
}

struct Extractor<'a> {
    source: &'a str,
    facts: FileFacts,
    scopes: Vec<Scope>,
    calls: Vec<PendingCall>,
    evidence: Vec<PendingReference>,
    builtin_methods: Vec<(usize, String, String)>,
    stars: Vec<(String, usize, u32)>,
    all: Value,
    receiver_writes: BTreeMap<usize, BTreeSet<String>>,
    callable_flows: HashMap<usize, CallableFlow>,
}

/// Extract static Python facts without executing source or guessing dynamic targets.
///
/// Binding identifiers use Python's NFKC normalization. Display labels and native
/// IDs retain source spelling; module paths retain literal filesystem spelling.
/// Annotation expressions are conservatively omitted from call extraction across
/// eager, lazy, and postponed annotation modes; evaluated defaults are retained.
/// This is not a complete runtime call graph. Class-private names and implicit
/// `__class__` stay unresolved. Module stars defer definition keys and aliases
/// until `PythonContext::apply` can verify the binding against the inventory.
/// Member candidates are lookup requests, not inferred runtime receiver types.
/// Implicit method receivers provide class-qualified declaration navigation;
/// recorded writes, dynamic lookup hooks, and conflicting visible overrides
/// suppress that evidence. External subclasses and runtime monkey patches are
/// not modeled. Descriptors with arbitrary decorators remain opaque.
/// Validation covers Tree-sitter syntax and duplicate parameters, not all Python
/// compiler constraints.
pub fn parse_python(path: &str, source: &str, hash: &str) -> Result<FileFacts> {
    parse_python_with_source_root(path, source, hash, None)
}

/// Parse with an explicit repository-relative source root. An empty root means
/// repository root; `None` preserves the conventional `src/` layout.
/// The caller selects the root from repository configuration; no filesystem is read.
pub fn parse_python_with_source_root(
    path: &str,
    source: &str,
    hash: &str,
    source_root: Option<&str>,
) -> Result<FileFacts> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || !(path.ends_with(".py")
            || (std::path::Path::new(path).extension().is_none()
                && ctx_graph_types::shebang_language(source) == Some("python")))
    {
        bail!(
            "Python source path must be a normalized relative POSIX .py path or an extensionless Python script"
        );
    }
    let mut facts = empty_facts(path, hash);
    if let Some(root) = source_root {
        if !root.is_empty()
            && (root.contains('\\')
                || root
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == ".."))
        {
            bail!("Python source root must be a normalized relative POSIX directory");
        }
        let relative = if root.is_empty() {
            path
        } else {
            path.strip_prefix(&format!("{root}/"))
                .context("Python path is outside its source root")?
        };
        let module = relative.strip_suffix(".py").unwrap_or(relative);
        let module = module.strip_suffix("/__init__").unwrap_or(module);
        facts.module = if module == "__init__" {
            String::new()
        } else {
            module.replace('/', ".")
        };
    }
    if source.len() > MAX_SOURCE_BYTES {
        facts.diagnostics.push(Diagnostic {
            file: path.into(),
            line: None,
            message: "Python source exceeds the 4 MiB limit".into(),
        });
        return Ok(facts);
    }
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .context("Python parser did not return a tree")?;
    let root = tree.root_node();
    // Bound our traversal as well as input size; generated deeply nested syntax is not useful here.
    let mut pending = vec![(root, 0)];
    while let Some((node, depth)) = pending.pop() {
        if node.is_error() || node.is_missing() || depth > 256 {
            facts.diagnostics.push(Diagnostic {
                file: path.into(),
                line: Some(line(node)),
                message: if depth > 256 {
                    "Python syntax nesting exceeds the indexing limit"
                } else {
                    "Python syntax error; no facts indexed for this file"
                }
                .into(),
            });
            return Ok(facts);
        }
        if matches!(node.kind(), "parameters" | "lambda_parameters") {
            let mut names = HashSet::new();
            for parameter in parameter_names(node) {
                let name = &source[parameter.byte_range()];
                if !names.insert(identifier(name)) {
                    facts.diagnostics.push(Diagnostic {
                        file: path.into(),
                        line: Some(line(parameter)),
                        message: format!(
                            "Duplicate Python parameter '{name}'; no facts indexed for this file"
                        ),
                    });
                    return Ok(facts);
                }
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor).map(|n| (n, depth + 1)));
    }
    let owner = format!("python:{path}:module");
    facts.nodes.push(Node {
        id: owner.clone(),
        label: facts.module.clone(),
        kind: "module".into(),
        file: path.into(),
        line: Some(1),
        end_line: Some(line_end(root)),
        qualified_name: Some(facts.module.clone()),
        binding_key: Some(format!("module:{}", facts.module)),
        metadata: Value::Null,
    });
    let mut extractor = Extractor {
        source,
        facts,
        calls: vec![],
        evidence: vec![],
        builtin_methods: vec![],
        stars: vec![],
        all: static_python_all(root, source),
        receiver_writes: BTreeMap::new(),
        callable_flows: HashMap::new(),
        scopes: vec![Scope {
            parent: None,
            kind: ScopeKind::Module,
            owner,
            qualified: String::new(),
            bindings: HashMap::new(),
            uncertain: false,
        }],
    };
    if !extractor.generated_module(root) {
        extractor.docstring(root, 0);
    }
    extractor.visit(root, 0, false);
    extractor.finish();
    Ok(extractor.facts)
}

// A single literal list/tuple assignment is the entire supported __all__ language.
// Any additional use (including mutation, aliasing, or a conditional assignment)
// makes star enumeration opaque, without affecting explicit named imports.
fn static_python_all(root: Syntax<'_>, source: &str) -> Value {
    let mut pending = vec![root];
    let mut mentions = 0;
    while let Some(node) = pending.pop() {
        if node.kind() == "identifier" && identifier(&source[node.byte_range()]) == "__all__" {
            mentions += 1;
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    if mentions == 0 {
        return Value::Null;
    }
    if mentions != 1 {
        return json!(false);
    }
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let Some(assignment) = statement
            .named_child(0)
            .filter(|n| statement.kind() == "expression_statement" && n.kind() == "assignment")
        else {
            continue;
        };
        if !assignment.child_by_field_name("left").is_some_and(|n| {
            n.kind() == "identifier" && identifier(&source[n.byte_range()]) == "__all__"
        }) {
            continue;
        }
        let Some(value) = assignment.child_by_field_name("right") else {
            break;
        };
        if !matches!(value.kind(), "list" | "tuple") {
            break;
        }
        let mut names = BTreeSet::new();
        let mut cursor = value.walk();
        for item in value
            .named_children(&mut cursor)
            .filter(|n| n.kind() != "comment")
        {
            let text = &source[item.byte_range()];
            if item.kind() != "string"
                || text.len() < 2
                || !matches!(text.as_bytes()[0], b'\'' | b'"')
                || text.as_bytes().last() != text.as_bytes().first()
            {
                return json!(false);
            }
            let name = &text[1..text.len() - 1];
            if name.is_empty()
                || !name.chars().all(|c| c == '_' || c.is_alphanumeric())
                || name.chars().next().is_some_and(char::is_numeric)
            {
                return json!(false);
            }
            names.insert(name.to_owned());
        }
        return json!(names);
    }
    json!(false)
}

fn line(node: Syntax<'_>) -> u32 {
    node.start_position().row as u32 + 1
}
fn line_end(node: Syntax<'_>) -> u32 {
    let end = node.end_position();
    (end.row + usize::from(end.column != 0 || end.row == 0)) as u32
}

fn parameter_names(node: Syntax<'_>) -> Vec<Syntax<'_>> {
    let mut names = vec![];
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        match node.kind() {
            "identifier" | "keyword_identifier" => names.push(node),
            "default_parameter" | "typed_default_parameter" => {
                pending.extend(node.child_by_field_name("name"));
            }
            "typed_parameter" => pending.extend(node.named_child(0)),
            "parameters"
            | "lambda_parameters"
            | "list_splat_pattern"
            | "dictionary_splat_pattern"
            | "tuple_pattern" => {
                let mut cursor = node.walk();
                pending.extend(node.named_children(&mut cursor));
            }
            _ => {}
        }
    }
    names
}

fn private_name(name: &str) -> bool {
    name.starts_with("__") && !name.ends_with("__")
}

fn identifier(name: &str) -> String {
    name.nfkc().collect()
}

fn dotted_text(node: Syntax<'_>, source: &str) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![source[node.byte_range()].into()]),
        "attribute" => {
            let mut parts = dotted_text(node.child_by_field_name("object")?, source)?;
            parts.push(source[node.child_by_field_name("attribute")?.byte_range()].into());
            Some(parts)
        }
        "parenthesized_expression" if node.named_child_count() == 1 => {
            dotted_text(node.named_child(0)?, source)
        }
        _ => None,
    }
}

fn python_definition_key(node: &Node) -> Option<&str> {
    node.binding_key
        .as_deref()
        .or_else(|| node.metadata["python_pending_binding"]["key"].as_str())
}

/// Explicit Python import forwarding over a complete, visible parser inventory.
/// Rebuild this context after inventory changes and include `fingerprint()` in
/// Python file stamps before calling `apply` on parsed files. No filesystem or
/// Python execution is performed. Only literal export lists, verified public
/// bindings, and literal class bases participate; runtime aliases stay opaque.
#[derive(Default)]
pub struct PythonContext {
    modules: BTreeMap<String, Vec<PythonModule>>,
    bindings: BTreeSet<String>,
    methods: BTreeSet<String>,
    classes: BTreeMap<String, Vec<PythonClass>>,
    children: BTreeMap<String, BTreeSet<String>>,
    fingerprint: String,
}

type PythonExports = BTreeMap<String, Option<String>>;

struct PythonModule {
    exports: BTreeMap<String, String>,
    definitions: BTreeMap<String, String>,
    stars: Vec<String>,
    all: Value,
    blocked: BTreeSet<String>,
    uncertain: bool,
}

struct PythonClass {
    bases: Vec<Option<String>>,
    members: BTreeMap<String, Option<String>>,
    uncertain: bool,
    receiver_writes: BTreeSet<String>,
}

mod extractor_collect_callable_flow;
mod extractor_finish;
mod extractor_text;
mod pythoncontext_from_facts;
mod pythoncontext_module_member;
