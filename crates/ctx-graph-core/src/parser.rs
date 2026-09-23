use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tree_sitter::{Node as Syntax, Parser};
use unicode_normalization::UnicodeNormalization;

use crate::model::*;

pub(crate) const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn empty_facts(path: &str, hash: &str) -> FileFacts {
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
                && crate::languages::scripted::shebang_language(source) == Some("python")))
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

impl Extractor<'_> {
    fn text(&self, node: Syntax<'_>) -> &str {
        &self.source[node.byte_range()]
    }

    fn reference(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        owner: &str,
        relation: &'static str,
        context: &'static str,
    ) {
        if let Some(parts) = self.dotted(node) {
            self.evidence.push(PendingReference {
                site: PendingCall {
                    scope,
                    start: node.start_byte(),
                    end: node.end_byte(),
                    line: line(node),
                    parts,
                },
                owner: owner.into(),
                relation,
                context,
            });
        }
    }

    fn type_references(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        owner: &str,
        context: &'static str,
    ) {
        if self.dotted(node).is_some() {
            self.reference(node, scope, owner, "references", context);
            return;
        }
        if node.kind() == "string" {
            // Forward references are syntax evidence only. Parse a plain quoted
            // name with the same grammar; escaped or computed strings stay opaque.
            if let Some(text) = self.string_text(node).filter(|s| !s.contains('\\')) {
                let mut parser = Parser::new();
                if parser
                    .set_language(&tree_sitter_python::LANGUAGE.into())
                    .is_ok()
                    && let Some(tree) = parser
                        .parse(&text, None)
                        .filter(|t| !t.root_node().has_error())
                {
                    let root = tree.root_node();
                    if root.named_child_count() == 1
                        && let Some(expression) = root.named_child(0).and_then(|n| n.named_child(0))
                        && let Some(parts) = dotted_text(expression, &text)
                    {
                        self.evidence.push(PendingReference {
                            site: PendingCall {
                                scope,
                                start: node.start_byte(),
                                end: node.end_byte(),
                                line: line(node),
                                parts,
                            },
                            owner: owner.into(),
                            relation: "references",
                            context,
                        });
                    }
                }
            }
            return;
        }
        // These expressions contain values or bind names; traversing them as types
        // would invent type dependencies (and annotations are never CALLS here).
        if matches!(
            node.kind(),
            "call"
                | "lambda"
                | "string"
                | "concatenated_string"
                | "list_comprehension"
                | "dictionary_comprehension"
                | "constrained_type"
        ) {
            return;
        }
        if matches!(node.kind(), "subscript" | "generic_type") {
            let head = node
                .child_by_field_name("value")
                .or_else(|| node.named_child(0));
            if let Some(head) = head {
                self.type_references(head, scope, owner, context);
                // Literal arguments are values; Annotated arguments after the
                // first are metadata. Avoid interpreting either as type names.
                let head_parts = self.dotted(head).unwrap_or_default();
                let special = head_parts.last().map(String::as_str);
                if special == Some("Literal") {
                    return;
                }
                let mut cursor = node.walk();
                let arguments: Vec<_> = node
                    .named_children(&mut cursor)
                    .filter(|n| n.id() != head.id())
                    .collect();
                for argument in arguments {
                    if argument.kind() == "type_parameter" {
                        let mut cursor = argument.walk();
                        for (index, item) in argument.named_children(&mut cursor).enumerate() {
                            if special == Some("Annotated") && index > 0 {
                                break;
                            }
                            self.type_references(item, scope, owner, "generic_arg");
                        }
                    } else {
                        self.type_references(argument, scope, owner, "generic_arg");
                        if special == Some("Annotated") {
                            break;
                        }
                    }
                }
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.type_references(child, scope, owner, context);
        }
    }

    fn string_text(&self, node: Syntax<'_>) -> Option<String> {
        if node.kind() == "parenthesized_expression" && node.named_child_count() == 1 {
            return self.string_text(node.named_child(0)?);
        }
        if node.kind() == "concatenated_string" {
            let mut text = String::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                text.push_str(&self.string_text(child)?);
            }
            return Some(text);
        }
        if node.kind() != "string" {
            return None;
        }
        let raw = self.text(node);
        let quote = raw.find(['\'', '"'])?;
        if raw[..quote]
            .bytes()
            .any(|b| matches!(b, b'b' | b'B' | b'f' | b'F'))
        {
            return None;
        }
        let width = if raw[quote..].starts_with("\"\"\"") || raw[quote..].starts_with("\'\'\'") {
            3
        } else {
            1
        };
        Some(raw[quote + width..raw.len() - width].to_owned())
    }

    fn docstring(&mut self, body: Syntax<'_>, scope: usize) {
        let mut cursor = body.walk();
        let first = body
            .named_children(&mut cursor)
            .find(|n| n.kind() != "comment");
        if let Some(statement) = first.filter(|n| n.kind() == "expression_statement")
            && let Some(string) = statement.named_child(0)
            && let Some(text) = self
                .string_text(string)
                .filter(|s| s.trim().chars().count() > 20)
        {
            self.rationale(string, text.trim().into(), scope, "docstring");
        }
    }

    fn rationale(&mut self, node: Syntax<'_>, text: String, scope: usize, kind: &str) {
        let id = format!("python:{}:rationale@{}", self.facts.path, node.start_byte());
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let label = if flat.chars().count() > 80 {
            let prefix: String = flat.chars().take(79).collect();
            let cut = if flat.chars().nth(79) == Some(' ') {
                prefix.as_str()
            } else {
                prefix
                    .rsplit_once(' ')
                    .map_or(prefix.as_str(), |(words, _)| words)
            };
            format!("{}…", cut.trim_end())
        } else {
            flat
        };
        self.facts.nodes.push(Node {
            id: id.clone(),
            label,
            kind: "rationale".into(),
            file: self.facts.path.clone(),
            line: Some(line(node)),
            end_line: Some(line_end(node)),
            qualified_name: None,
            binding_key: None,
            metadata: json!({"kind": kind, "text": text, "source_text": self.text(node)}),
        });
        self.facts.edges.push(Edge {
            id: format!("rationale_for:{id}"),
            source: id,
            target: self.scopes[scope].owner.clone(),
            relation: "rationale_for".into(),
            directed: true,
            file: Some(self.facts.path.clone()),
            line: Some(line(node)),
            confidence: "static".into(),
            metadata: json!({"context": kind}),
        });
    }

    fn generated_module(&self, root: Syntax<'_>) -> bool {
        let head: String = self.source.chars().take(2048).collect();
        if [
            "DO NOT EDIT",
            "@generated",
            "Generated by the protocol buffer",
        ]
        .iter()
        .any(|marker| head.contains(marker))
        {
            return true;
        }
        let mut revision = false;
        let mut down_revision = false;
        let mut upgrade = false;
        let mut cursor = root.walk();
        for statement in root.named_children(&mut cursor) {
            if statement.kind() == "function_definition" {
                upgrade |= statement
                    .child_by_field_name("name")
                    .is_some_and(|n| self.text(n) == "upgrade");
            }
            if statement.kind() == "expression_statement"
                && let Some(assignment) = statement
                    .named_child(0)
                    .filter(|n| n.kind() == "assignment")
                && let Some(name) = assignment.child_by_field_name("left")
            {
                revision |= self.text(name) == "revision";
                down_revision |= self.text(name) == "down_revision";
            }
            if statement.kind() == "class_definition"
                && statement
                    .child_by_field_name("name")
                    .is_some_and(|n| self.text(n) == "Migration")
                && statement
                    .child_by_field_name("superclasses")
                    .is_some_and(|n| self.text(n).contains("migrations.Migration"))
                && statement
                    .child_by_field_name("body")
                    .is_some_and(|n| self.text(n).contains("operations"))
            {
                return true;
            }
        }
        revision && down_revision && upgrade
    }

    fn decorator_noise(&self, site: &PendingCall, key: Option<&str>) -> bool {
        if matches!(
            key,
            Some(
                "python:dataclasses:dataclass"
                    | "python:functools:wraps"
                    | "python:functools:lru_cache"
                    | "python:functools:cache"
                    | "python:abc:abstractmethod"
            )
        ) {
            return true;
        }
        if site.parts.len() != 1
            || !matches!(
                site.parts[0].as_str(),
                "property" | "staticmethod" | "classmethod"
            )
        {
            return false;
        }
        let mut current = Some(site.scope);
        while let Some(index) = current {
            if self.scopes[index].uncertain
                || self.scopes[index].bindings.contains_key(&site.parts[0])
            {
                return false;
            }
            current = self.scopes[index].parent;
        }
        true
    }

    fn bind(&mut self, scope: usize, name: String, binding: Binding) {
        self.scopes[scope]
            .bindings
            .entry(identifier(&name))
            .and_modify(|b| *b = Binding::Unknown)
            .or_insert(binding);
    }

    fn target(&mut self, node: Syntax<'_>, scope: usize) {
        match node.kind() {
            "identifier" => self.bind(scope, self.text(node).into(), Binding::Unknown),
            "attribute" => {
                if let Some(parts) = self.dotted(node)
                    && let Some((class, _)) = self.receiver(scope, &identifier(&parts[0]))
                {
                    self.receiver_writes.entry(class).or_default().insert(
                        if parts[1] == "__dict__" {
                            "*".into()
                        } else {
                            identifier(&parts[1])
                        },
                    );
                    return;
                }
                if let Some(object) = node.child_by_field_name("object") {
                    self.target(object, scope);
                }
            }
            "subscript" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.target(value, scope);
                }
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.target(child, scope);
                }
            }
        }
    }

    fn parameters(&mut self, node: Syntax<'_>, scope: usize) {
        for name in parameter_names(node) {
            self.target(name, scope);
        }
    }

    fn visit(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        if node.kind() == "type" {
            self.type_references(
                node,
                scope,
                &self.scopes[scope].owner.clone(),
                "variable_type",
            );
            return;
        }
        match node.kind() {
            "comment" => {
                if [
                    "# NOTE:",
                    "# IMPORTANT:",
                    "# HACK:",
                    "# WHY:",
                    "# RATIONALE:",
                    "# TODO:",
                    "# FIXME:",
                ]
                .iter()
                .any(|prefix| self.text(node).starts_with(prefix))
                {
                    self.rationale(node, self.text(node).to_owned(), scope, "comment");
                }
                return;
            }
            "decorated_definition" => {
                if let Some(definition) = node.child_by_field_name("definition") {
                    let mut cursor = node.walk();
                    let decorators: Vec<_> = node
                        .named_children(&mut cursor)
                        .filter(|n| n.kind() == "decorator")
                        .collect();
                    let builtin = decorators.len() == 1
                        && self.scopes[scope].kind == ScopeKind::Class
                        && decorators[0].named_child(0).is_some_and(|n| {
                            n.kind() == "identifier"
                                && matches!(self.text(n), "staticmethod" | "classmethod")
                        });
                    let owner = self.definition_id(definition, scope);
                    if builtin && !conditional {
                        self.builtin_methods.push((
                            scope,
                            self.text(decorators[0].named_child(0).unwrap()).into(),
                            owner.clone(),
                        ));
                    }
                    for decorator in decorators {
                        if let Some(expression) = decorator.named_child(0) {
                            let head = expression
                                .child_by_field_name("function")
                                .unwrap_or(expression);
                            self.reference(head, scope, &owner, "references", "decorator");
                            self.visit(expression, scope, conditional);
                        }
                    }
                    self.definition(definition, scope, conditional || !builtin);
                }
                return;
            }
            "function_definition" | "class_definition" => {
                self.definition(node, scope, conditional);
                return;
            }
            "import_statement" | "import_from_statement" => {
                self.import(node, scope, conditional);
                return;
            }
            "lambda"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => {
                // ponytail: anonymous scopes retain call sites, but do not infer their bindings.
                let child = self.scopes.len();
                self.scopes.push(Scope {
                    parent: Some(scope),
                    kind: ScopeKind::Opaque,
                    owner: self.scopes[scope].owner.clone(),
                    qualified: self.scopes[scope].qualified.clone(),
                    bindings: HashMap::new(),
                    uncertain: true,
                });
                let mut cursor = node.walk();
                for n in node.named_children(&mut cursor) {
                    self.visit(n, child, true);
                }
                return;
            }
            "assignment"
            | "augmented_assignment"
            | "type_alias_statement"
            | "for_statement"
            | "for_in_clause" => {
                if let Some(target) = node.child_by_field_name("left") {
                    self.target(target, scope);
                }
            }
            "named_expression" => {
                if let Some(target) = node.child_by_field_name("name") {
                    self.target(target, scope);
                    if self.scopes[scope].kind == ScopeKind::Opaque {
                        let mut parent = self.scopes[scope].parent;
                        while let Some(index) = parent {
                            self.target(target, index);
                            if self.scopes[index].kind != ScopeKind::Opaque {
                                break;
                            }
                            parent = self.scopes[index].parent;
                        }
                    }
                }
            }
            "as_pattern" | "except_clause" => {
                if let Some(target) = node.child_by_field_name("alias") {
                    self.target(target, scope);
                }
            }
            "delete_statement" | "global_statement" | "nonlocal_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.target(child, scope);
                }
                // A declaration can allow writes to an enclosing binding. Do not export a guessed target.
                if node.kind() != "delete_statement" {
                    let mut parent = self.scopes[scope].parent;
                    while let Some(index) = parent {
                        let mut cursor = node.walk();
                        for child in node.named_children(&mut cursor) {
                            self.target(child, index);
                        }
                        parent = self.scopes[index].parent;
                    }
                }
            }
            "case_clause" => self.scopes[scope].uncertain = true,
            "exec_statement" => self.scopes[scope].uncertain = true,
            "call" => {
                let parts = node
                    .child_by_field_name("function")
                    .and_then(|n| self.call_parts(n))
                    .unwrap_or_default();
                let function = parts.first().map(|name| identifier(name));
                if parts.len() == 1
                    && matches!(function.as_deref(), Some("exec" | "globals" | "locals"))
                {
                    self.scopes[scope].uncertain = true;
                    if function.as_deref() == Some("globals") {
                        self.scopes[0].uncertain = true;
                    }
                }
                if parts.len() == 1
                    && matches!(function.as_deref(), Some("setattr" | "delattr"))
                    && let Some(arguments) = node.child_by_field_name("arguments")
                    && let Some(receiver) = arguments.named_child(0)
                    && receiver.kind() == "identifier"
                    && let Some((class, _)) = self.receiver(scope, &identifier(self.text(receiver)))
                {
                    // A computed name can replace any member. Even a shadowed
                    // setter is not evidence that the receiver stays unchanged.
                    let name = arguments.named_child(1).and_then(|n| self.string_text(n));
                    self.receiver_writes.entry(class).or_default().insert(
                        name.filter(|n| !n.contains('\\'))
                            .unwrap_or_else(|| "*".into()),
                    );
                }
                self.calls.push(PendingCall {
                    scope,
                    start: node.start_byte(),
                    end: node.end_byte(),
                    line: line(node),
                    parts,
                });
            }
            _ => {}
        }
        let conditional = conditional
            || matches!(
                node.kind(),
                "if_statement"
                    | "for_statement"
                    | "while_statement"
                    | "try_statement"
                    | "with_statement"
                    | "match_statement"
                    | "decorated_definition"
            );
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child, scope, conditional);
        }
    }

    fn definition_id(&self, node: Syntax<'_>, scope: usize) -> String {
        let name = self.text(node.child_by_field_name("name").unwrap());
        let prefix = &self.scopes[scope].qualified;
        let qualified = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };
        format!(
            "python:{}:{qualified}@{}",
            self.facts.path,
            node.start_byte()
        )
    }

    fn definition(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        let name = self
            .text(node.child_by_field_name("name").unwrap())
            .to_owned();
        let qualified = if self.scopes[scope].qualified.is_empty() {
            name.clone()
        } else {
            format!("{}.{}", self.scopes[scope].qualified, name)
        };
        let id = format!(
            "python:{}:{}@{}",
            self.facts.path,
            qualified,
            node.start_byte()
        );
        let key = format!("python:{}:{}", self.facts.module, identifier(&qualified));
        let class = node.kind() == "class_definition";
        self.bind(
            scope,
            name.clone(),
            if conditional {
                Binding::Unknown
            } else {
                Binding::Definition {
                    key: key.clone(),
                    id: id.clone(),
                    start: node.end_byte(),
                }
            },
        );
        self.facts.nodes.push(Node {
            id: id.clone(),
            label: name,
            kind: if class {
                "class"
            } else if self.scopes[scope].kind == ScopeKind::Class {
                "method"
            } else {
                "function"
            }
            .into(),
            file: self.facts.path.clone(),
            line: Some(line(node)),
            end_line: Some(line_end(node)),
            qualified_name: Some(identifier(&qualified)),
            binding_key: Some(key),
            metadata: Value::Null,
        });
        self.facts.edges.push(Edge {
            id: format!("contains:{id}"),
            source: self.scopes[scope].owner.clone(),
            target: id.clone(),
            relation: "contains".into(),
            directed: true,
            file: Some(self.facts.path.clone()),
            line: Some(line(node)),
            confidence: "static".into(),
            metadata: Value::Null,
        });
        let child = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(scope),
            kind: if class {
                ScopeKind::Class
            } else {
                ScopeKind::Function
            },
            owner: id.clone(),
            qualified,
            bindings: HashMap::new(),
            uncertain: false,
        });
        if node.child_by_field_name("type_parameters").is_some() {
            self.scopes[child].uncertain = true;
        }
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.parameters(parameters, child);
            if !class
                && !conditional
                && self.scopes[scope].kind == ScopeKind::Class
                && !self
                    .builtin_methods
                    .iter()
                    .any(|(_, name, owner)| owner == &id && name == "staticmethod")
                && let Some(first) = parameters.named_child(0)
                && matches!(
                    first.kind(),
                    "identifier"
                        | "typed_parameter"
                        | "default_parameter"
                        | "typed_default_parameter"
                )
                && let Some(name) = parameter_names(first).first()
                && name.kind() == "identifier"
            {
                let name = identifier(self.text(*name));
                self.scopes[child].bindings.insert(
                    name,
                    Binding::Receiver {
                        class: scope,
                        method: child,
                    },
                );
            }
        }
        let body = node.child_by_field_name("body").unwrap();
        self.docstring(body, child);
        if let Some(parameters) = node.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                if let Some(annotation) = parameter.child_by_field_name("type") {
                    self.type_references(annotation, scope, &id, "parameter_type");
                }
            }
        }
        if let Some(annotation) = node.child_by_field_name("return_type") {
            self.type_references(annotation, scope, &id, "return_type");
        }
        if class {
            let literal = node
                .child_by_field_name("superclasses")
                .is_none_or(|bases| {
                    let mut cursor = bases.walk();
                    bases
                        .named_children(&mut cursor)
                        .filter(|n| n.kind() != "comment")
                        .all(|base| self.dotted(base).is_some())
                });
            self.facts
                .nodes
                .iter_mut()
                .find(|n| n.id == id)
                .unwrap()
                .metadata = json!({"python_literal_bases": literal});
        }
        if let Some(bases) = node.child_by_field_name("superclasses") {
            let mut cursor = bases.walk();
            for base in bases.named_children(&mut cursor) {
                if !matches!(
                    base.kind(),
                    "keyword_argument" | "list_splat" | "dictionary_splat"
                ) {
                    let head = base.child_by_field_name("value").unwrap_or(base);
                    self.reference(head, scope, &id, "inherits", "base_class");
                    if base.kind() == "subscript" {
                        let mut cursor = base.walk();
                        for argument in base.children_by_field_name("subscript", &mut cursor) {
                            self.type_references(argument, scope, &id, "generic_arg");
                        }
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for n in node.named_children(&mut cursor) {
            if n.id() == body.id() {
                self.visit(n, child, false);
            } else if n.kind() == "parameters" {
                let mut cursor = n.walk();
                for parameter in n.named_children(&mut cursor) {
                    if let Some(default) = parameter.child_by_field_name("value") {
                        self.visit(default, scope, conditional);
                    }
                }
            } else if n.kind() != "type" {
                self.visit(n, scope, conditional);
            }
        }
        if !class {
            self.collect_callable_flow(node, body, child);
        }
    }

    // Separate from public bindings: only same-frame, straight-line value
    // reads can use assignment history. Nothing here becomes an export alias.
    fn collect_callable_flow(&mut self, definition: Syntax<'_>, body: Syntax<'_>, scope: usize) {
        let mut flow = CallableFlow::default();
        let mut plain = HashSet::new();
        if let Some(parameters) = definition.child_by_field_name("parameters") {
            flow.blocked.extend(
                parameter_names(parameters)
                    .into_iter()
                    .map(|n| identifier(self.text(n))),
            );
        }
        flow.opaque = definition.child(0).is_some_and(|n| n.kind() == "async");
        let mut cursor = body.walk();
        let statements: Vec<_> = body.named_children(&mut cursor).collect();
        for statement in &statements {
            if statement.kind() == "expression_statement"
                && let Some(assignment) = statement.named_child(0)
                && assignment.kind() == "assignment"
                && assignment.child_by_field_name("type").is_none()
                && let Some(left) = assignment.child_by_field_name("left")
                && left.kind() == "identifier"
                && let Some(right) = assignment.child_by_field_name("right")
                && right.kind() != "assignment"
            {
                plain.insert(assignment.id());
                flow.writes.push(CallableWrite {
                    start: assignment.start_byte(),
                    end: assignment.end_byte(),
                    name: identifier(self.text(left)),
                    value: (right.kind() == "identifier").then(|| identifier(self.text(right))),
                });
            }
        }
        if flow.writes.is_empty() {
            return;
        }
        for statement in statements {
            if !matches!(
                statement.kind(),
                "expression_statement" | "return_statement"
            ) {
                continue;
            }
            let mut pending = vec![statement];
            while let Some(node) = pending.pop() {
                if matches!(
                    node.kind(),
                    "lambda"
                        | "list_comprehension"
                        | "set_comprehension"
                        | "dictionary_comprehension"
                        | "generator_expression"
                        | "conditional_expression"
                        | "boolean_operator"
                ) {
                    continue;
                }
                if node.kind() == "call" {
                    flow.calls.insert(node.start_byte());
                }
                let mut cursor = node.walk();
                pending.extend(node.named_children(&mut cursor));
            }
        }
        // Reads in nested scopes are captures even without a nonlocal write:
        // an escaping closure may run after a later assignment. Value escapes
        // (arguments, returns, containers, member access) likewise veto history.
        let mut pending = vec![(body, false)];
        while let Some((node, nested)) = pending.pop() {
            let nested = nested
                || matches!(
                    node.kind(),
                    "function_definition"
                        | "class_definition"
                        | "lambda"
                        | "list_comprehension"
                        | "set_comprehension"
                        | "dictionary_comprehension"
                        | "generator_expression"
                );
            if matches!(node.kind(), "yield" | "await" | "exec_statement") {
                flow.opaque = true;
            }
            if node.kind() == "call"
                && let Some(function) = node.child_by_field_name("function")
                && let Some(parts) = self.call_parts(function)
                && parts.len() == 1
                && matches!(
                    identifier(&parts[0]).as_str(),
                    "eval" | "exec" | "globals" | "locals"
                )
            {
                flow.opaque = true;
            }
            if node.kind() == "identifier"
                && let Some(parent) = node.parent()
            {
                let field = |name| {
                    parent
                        .child_by_field_name(name)
                        .is_some_and(|n| n.id() == node.id())
                };
                let local_read = !nested
                    && ((parent.kind() == "call"
                        && field("function")
                        && flow.calls.contains(&parent.start_byte()))
                        || (plain.contains(&parent.id()) && (field("left") || field("right"))));
                if !(local_read || (parent.kind() == "attribute" && field("attribute"))) {
                    flow.blocked.insert(identifier(self.text(node)));
                }
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor).map(|n| (n, nested)));
        }
        // A copied function value can escape or be mutated through either
        // name. Propagate local-write vetoes through copies in both directions,
        // conservatively across rebindings too. Definition names alone are not
        // seeds: declaring a same-frame function is not an escape of its copies.
        let mut copies: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut pending = Vec::new();
        for write in &flow.writes {
            if flow.blocked.contains(&write.name) {
                pending.push(write.name.as_str());
            }
            if let Some(value) = write.value.as_deref() {
                copies.entry(&write.name).or_default().push(value);
                copies.entry(value).or_default().push(&write.name);
            }
        }
        let mut visited = HashSet::new();
        while let Some(name) = pending.pop() {
            if visited.insert(name) {
                flow.blocked.insert(name.to_owned());
                if let Some(neighbors) = copies.get(name) {
                    pending.extend(neighbors.iter().copied());
                }
            }
        }
        flow.writes.sort_by_key(|write| write.end);
        self.callable_flows.insert(scope, flow);
    }

    fn callable_keys(&self) -> HashMap<usize, String> {
        let mut keys = HashMap::new();
        // Star-dependent definitions are published later by PythonContext.
        // A local value must not bypass that existing ambiguity proof.
        if self.callable_flows.is_empty() || !self.stars.is_empty() {
            return keys;
        }
        let mut counts = HashMap::new();
        for key in self
            .facts
            .nodes
            .iter()
            .filter_map(|n| n.binding_key.as_deref())
        {
            *counts.entry(key).or_insert(0_usize) += 1;
        }
        let functions: HashSet<_> = self
            .facts
            .nodes
            .iter()
            .filter(|n| {
                n.kind == "function"
                    && n.binding_key
                        .as_deref()
                        .is_some_and(|k| counts.get(k) == Some(&1))
            })
            .map(|n| n.id.as_str())
            .collect();
        let mut calls: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, call) in self.calls.iter().enumerate() {
            calls.entry(call.scope).or_default().push(index);
        }
        for (scope, flow) in &self.callable_flows {
            if flow.opaque || self.scopes[*scope].uncertain {
                continue;
            }
            let Some(calls) = calls.get_mut(scope) else {
                continue;
            };
            calls.sort_by_key(|index| self.calls[*index].start);
            let mut values: HashMap<&str, String> = HashMap::new();
            let mut writes = flow.writes.iter().peekable();
            for index in calls {
                let call = &self.calls[*index];
                while writes.peek().is_some_and(|write| write.end <= call.start) {
                    let write = writes.next().unwrap();
                    let key = write.value.as_deref().and_then(|name| {
                        values.get(name).cloned().or_else(|| {
                            self.callable_definition(*scope, name, write.start, &functions)
                        })
                    });
                    values.remove(write.name.as_str());
                    if !flow.blocked.contains(&write.name)
                        && let Some(key) = key
                    {
                        values.insert(&write.name, key);
                    }
                }
                if flow.calls.contains(&call.start) && call.parts.len() == 1 {
                    let name = identifier(&call.parts[0]);
                    if !(self.class_context(*scope) && private_name(&name))
                        && let Some(key) = values.get(name.as_str())
                    {
                        keys.insert(*index, key.clone());
                    }
                }
            }
        }
        keys
    }

    fn callable_definition(
        &self,
        scope: usize,
        name: &str,
        position: usize,
        functions: &HashSet<&str>,
    ) -> Option<String> {
        if self.class_context(scope) && (private_name(name) || name == "__class__") {
            return None;
        }
        let mut current = Some(scope);
        while let Some(index) = current {
            let owner = &self.scopes[index];
            if owner.kind != ScopeKind::Class {
                if owner.uncertain {
                    return None;
                }
                if let Some(binding) = owner.bindings.get(name) {
                    // No captured/future function guess across execution frames.
                    return match binding {
                        Binding::Definition { id, key, start }
                            if (index == scope || owner.kind == ScopeKind::Module)
                                && *start <= position
                                && functions.contains(id.as_str()) =>
                        {
                            Some(key.clone())
                        }
                        _ => None,
                    };
                }
            }
            current = owner.parent;
        }
        None
    }

    fn relative_module(&self, name: &str) -> Option<String> {
        let dots = name.bytes().take_while(|c| *c == b'.').count();
        if dots == 0 {
            return Some(name.into());
        }
        let mut package: Vec<_> = self
            .facts
            .module
            .split('.')
            .filter(|p| !p.is_empty())
            .collect();
        if !self.facts.path.ends_with("/__init__.py") && self.facts.path != "__init__.py" {
            package.pop();
        }
        if dots > package.len() {
            return None;
        }
        package.truncate(package.len() - dots + 1);
        if !name[dots..].is_empty() {
            package.push(&name[dots..]);
        }
        Some(package.join("."))
    }

    fn import(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        let from = node.kind() == "import_from_statement";
        let module = node
            .child_by_field_name("module_name")
            .and_then(|n| self.relative_module(&identifier(self.text(n))));
        let module_label = node
            .child_by_field_name("module_name")
            .and_then(|n| self.relative_module(self.text(n)));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "wildcard_import" {
                let keys = if scope == 0
                    && !conditional
                    && let Some(module) = &module
                {
                    self.stars
                        .push((module.clone(), node.end_byte(), line(node)));
                    vec![format!("module:{module}")]
                } else {
                    self.scopes[scope].uncertain = true;
                    vec![]
                };
                self.import_reference(
                    child,
                    scope,
                    "*".into(),
                    keys,
                    "star module is unavailable or ambiguous",
                );
            }
        }
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            let name_node = item.child_by_field_name("name").unwrap_or(item);
            let raw_name = self.text(name_node).to_owned();
            let name = identifier(&raw_name);
            let alias = item
                .child_by_field_name("alias")
                .map(|n| identifier(self.text(n)));
            let local = alias.clone().unwrap_or_else(|| {
                if from {
                    name.clone()
                } else {
                    name.split('.').next().unwrap().into()
                }
            });
            let (mut binding, mut keys) = if from {
                match &module {
                    Some(module) => {
                        let key = format!("python:{module}:{name}");
                        (
                            Binding::Symbol {
                                key: key.clone(),
                                start: node.end_byte(),
                            },
                            vec![key, format!("module:{module}.{name}")],
                        )
                    }
                    None => (Binding::Unknown, vec![]),
                }
            } else {
                (
                    Binding::Module {
                        module: name.clone(),
                        prefix: alias.unwrap_or_else(|| name.clone()),
                        start: node.end_byte(),
                    },
                    vec![format!("module:{name}")],
                )
            };
            if self.class_context(scope)
                && (name.split('.').any(private_name)
                    || module
                        .as_ref()
                        .is_some_and(|m| m.split('.').any(private_name)))
            {
                binding = Binding::Unknown;
                keys.clear();
            }
            self.bind(
                scope,
                local,
                if conditional {
                    Binding::Unknown
                } else {
                    binding
                },
            );
            let label = module_label
                .as_ref()
                .map_or_else(|| raw_name.clone(), |m| format!("{m}.{raw_name}"));
            self.import_reference(
                item,
                scope,
                label,
                keys,
                "import target is unavailable or ambiguous",
            );
        }
    }

    fn import_reference(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        label: String,
        keys: Vec<String>,
        reason: &str,
    ) {
        self.facts.references.push(Reference {
            id: format!(
                "import:{}:{}-{}",
                self.scopes[scope].owner,
                node.start_byte(),
                node.end_byte()
            ),
            source: self.scopes[scope].owner.clone(),
            label,
            relation: "imports".into(),
            file: self.facts.path.clone(),
            line: line(node),
            candidate_keys: keys,
            reason: reason.into(),
        });
    }

    fn dotted(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        dotted_text(node, self.source)
    }

    fn call_parts(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        if node.kind() == "attribute"
            && let Some(object) = node.child_by_field_name("object")
            && object.kind() == "call"
            && object
                .child_by_field_name("function")
                .is_some_and(|n| n.kind() == "identifier" && self.text(n) == "super")
            && object
                .child_by_field_name("arguments")
                .is_some_and(|n| n.named_child_count() == 0)
        {
            return Some(vec![
                "super()".into(),
                self.text(node.child_by_field_name("attribute")?).into(),
            ]);
        }
        self.dotted(node)
    }

    fn receiver(&self, scope: usize, name: &str) -> Option<(usize, usize)> {
        let mut current = Some(scope);
        while let Some(index) = current {
            let scope = &self.scopes[index];
            if scope.kind != ScopeKind::Class {
                if scope.uncertain {
                    return None;
                }
                if let Some(binding) = scope.bindings.get(name) {
                    return match binding {
                        Binding::Receiver { class, method } => Some((*class, *method)),
                        _ => None,
                    };
                }
            }
            current = scope.parent;
        }
        None
    }

    fn receiver_key(
        &self,
        class: usize,
        method: usize,
        member: &str,
        super_call: bool,
    ) -> Option<String> {
        // The enclosing method must still be a verified descriptor. Rebinding
        // the method or a decorator builtin invalidates its implicit receiver.
        self.facts
            .nodes
            .iter()
            .find(|n| n.id == self.scopes[method].owner)?
            .binding_key
            .as_ref()?;
        let class = self
            .facts
            .nodes
            .iter()
            .find(|n| n.id == self.scopes[class].owner)?
            .binding_key
            .as_deref()?;
        Some(format!(
            "{}:{}.{}",
            if super_call {
                "python-super"
            } else {
                "python-receiver"
            },
            class.strip_prefix("python:")?,
            member
        ))
    }

    fn call_key(&self, call: &PendingCall) -> Option<String> {
        let parts: Vec<_> = call.parts.iter().map(|part| identifier(part)).collect();
        let name = parts.first()?;
        if self.class_context(call.scope)
            && (name == "__class__" || parts.iter().any(|part| private_name(part)))
        {
            return None;
        }
        if name == "super()" && parts.len() == 2 {
            // Zero-argument super uses the *current* frame's first argument;
            // closures and anonymous scopes do not inherit that frame.
            let scope = &self.scopes[call.scope];
            if scope.uncertain || !self.stars.is_empty() {
                return None;
            }
            let (class, method) = scope.bindings.values().find_map(|b| match b {
                Binding::Receiver { class, method } => Some((*class, *method)),
                _ => None,
            })?;
            let mut current = Some(call.scope);
            while let Some(index) = current {
                let scope = &self.scopes[index];
                if scope.kind != ScopeKind::Class
                    && (scope.uncertain || scope.bindings.contains_key("super"))
                {
                    return None;
                }
                current = scope.parent;
            }
            return self.receiver_key(class, method, &parts[1], true);
        }
        let mut index = Some(call.scope);
        let mut deferred = false;
        while let Some(current) = index {
            let scope = &self.scopes[current];
            // Class namespaces are not lexical closures, including for nested classes.
            if current == call.scope || scope.kind != ScopeKind::Class {
                if scope.uncertain {
                    return None;
                }
                if let Some(binding) = scope.bindings.get(name) {
                    return match binding {
                        Binding::Receiver { class, method } if parts.len() == 2 => {
                            self.receiver_key(*class, *method, &parts[1], false)
                        }
                        Binding::Definition { key, start, .. } | Binding::Symbol { key, start }
                            if parts.len() == 1 && (deferred || *start <= call.start) =>
                        {
                            Some(key.clone())
                        }
                        Binding::Definition { key, id, start }
                            if parts.len() > 1
                                && (deferred || *start <= call.start)
                                && self
                                    .facts
                                    .nodes
                                    .iter()
                                    .any(|n| n.id == *id && n.kind == "class") =>
                        {
                            Some(format!(
                                "python-member:{}.{}",
                                key.strip_prefix("python:")?,
                                parts[1..].join(".")
                            ))
                        }
                        Binding::Symbol { key, start }
                            if parts.len() > 1 && (deferred || *start <= call.start) =>
                        {
                            Some(format!(
                                "python-member:{}.{}",
                                key.strip_prefix("python:")?,
                                parts[1..].join(".")
                            ))
                        }
                        Binding::Module {
                            module,
                            prefix,
                            start,
                        } if deferred || *start <= call.start => {
                            let dotted = parts.join(".");
                            let suffix = dotted.strip_prefix(&format!("{prefix}."))?;
                            Some(PythonContext::module_member(module, suffix))
                        }
                        _ => None,
                    }
                    .map(|key| {
                        // Keep the consuming module's binding name until context
                        // checks its stars. The provider key alone loses collisions
                        // with explicit imports, including module aliases and bases.
                        if scope.kind == ScopeKind::Module && !self.stars.is_empty() {
                            format!("python-local:{}:{name}:{key}", self.facts.module)
                        } else {
                            key
                        }
                    });
                }
            }
            if scope.kind == ScopeKind::Module && !self.stars.is_empty() {
                return (deferred || self.stars.iter().all(|(_, start, _)| *start <= call.start))
                    .then(|| PythonContext::module_member(&self.facts.module, &parts.join(".")));
            }
            deferred |= scope.kind == ScopeKind::Function;
            index = scope.parent;
        }
        None
    }

    fn class_context(&self, scope: usize) -> bool {
        let mut index = Some(scope);
        while let Some(current) = index {
            if self.scopes[current].kind == ScopeKind::Class {
                return true;
            }
            index = self.scopes[current].parent;
        }
        false
    }

    fn exports(&mut self) {
        // Only final, unambiguous module bindings are public import evidence.
        // __all__ governs star imports, not an explicit named import; do not
        // execute it or infer names from mutations of that value.
        let mut exports = BTreeMap::new();
        let mut blocked = BTreeSet::new();
        let mut definitions = BTreeMap::new();
        for (name, binding) in &self.scopes[0].bindings {
            let public = !name.starts_with('_')
                || self
                    .all
                    .as_array()
                    .is_some_and(|names| names.iter().any(|n| n.as_str() == Some(name)));
            let (key, start) = match binding {
                Binding::Symbol { key, start } if public => (key.clone(), *start),
                Binding::Module {
                    module,
                    prefix,
                    start,
                } if public && prefix == name => (format!("module:{module}"), *start),
                Binding::Definition { key, .. } => {
                    definitions.insert(name.clone(), key.clone());
                    continue;
                }
                _ => {
                    blocked.insert(name.clone());
                    continue;
                }
            };
            if self.scopes[0].uncertain {
                continue;
            }
            let line = self.source[..start].lines().count() as u32;
            exports.insert(name.clone(), json!({"target": key, "line": line}));
            let module = key
                .strip_prefix("module:")
                .or_else(|| {
                    key.strip_prefix("python:")
                        .and_then(|s| s.split_once(':').map(|(module, _)| module))
                })
                .unwrap();
            self.facts.references.push(Reference {
                id: format!("reexport:{}:{name}", self.scopes[0].owner),
                source: self.scopes[0].owner.clone(),
                label: name.clone(),
                relation: "re_exports".into(),
                file: self.facts.path.clone(),
                line,
                candidate_keys: vec![format!("module:{module}")],
                reason: "explicit public import target is unavailable or ambiguous".into(),
            });
        }
        for (target, start, line) in &self.stars {
            self.facts.references.push(Reference {
                id: format!("reexport-star:{}:{start}", self.scopes[0].owner),
                source: self.scopes[0].owner.clone(),
                label: "*".into(),
                relation: "re_exports".into(),
                file: self.facts.path.clone(),
                line: *line,
                candidate_keys: vec![format!("module:{target}")],
                reason: "static star module is unavailable or ambiguous".into(),
            });
        }
        let module = &mut self.facts.nodes[0];
        if module.metadata.is_null() {
            module.metadata = json!({});
        }
        module.metadata["python_exports"] = json!(exports);
        module.metadata["python_blocked_exports"] = json!(blocked);
        module.metadata["python_uncertain"] = json!(self.scopes[0].uncertain);
        module.metadata["python_all"] = self.all.clone();
        module.metadata["python_definitions"] = json!(definitions);
        module.metadata["python_stars"] =
            json!(self.stars.iter().map(|(m, _, _)| m).collect::<Vec<_>>());
    }

    fn finish(&mut self) {
        for (scope, name, id) in &self.builtin_methods {
            let mut current = Some(*scope);
            // Star imports can replace decorator builtins. Without inventory at
            // extraction time, do not certify the decorated method's identity.
            let mut shadowed = !self.stars.is_empty();
            while let Some(index) = current {
                shadowed |=
                    self.scopes[index].uncertain || self.scopes[index].bindings.contains_key(name);
                current = self.scopes[index].parent;
            }
            if shadowed {
                for binding in self.scopes[*scope].bindings.values_mut() {
                    if matches!(binding, Binding::Definition { id: bound, .. } if bound == id) {
                        *binding = Binding::Unknown;
                    }
                }
            }
        }
        let valid: HashMap<_, _> = self
            .scopes
            .iter()
            .filter(|s| !s.uncertain)
            .flat_map(|s| s.bindings.values())
            .filter_map(|b| {
                if let Binding::Definition { id, key, .. } = b {
                    Some((id.clone(), key.clone()))
                } else {
                    None
                }
            })
            .collect();
        for node in self.facts.nodes.iter_mut().filter(|n| n.kind != "module") {
            node.binding_key = valid.get(&node.id).cloned();
        }
        // Only verified class ownership exports member aliases. A nested function
        // with the same qualified spelling must never satisfy an imported member.
        for scope in &self.scopes {
            if scope.kind != ScopeKind::Class
                || !valid.contains_key(&scope.owner)
                || scope.uncertain
                || scope.qualified.split('.').any(private_name)
            {
                continue;
            }
            let mut ancestor = scope.parent;
            let mut uncertain = false;
            while let Some(index) = ancestor {
                let parent = &self.scopes[index];
                uncertain |= parent.uncertain
                    || (parent.kind != ScopeKind::Module && !valid.contains_key(&parent.owner));
                ancestor = parent.parent;
            }
            if uncertain {
                continue;
            }
            for binding in scope.bindings.values() {
                if let Binding::Definition { id, key, .. } = binding
                    && let Some(node) = self.facts.nodes.iter_mut().find(|n| {
                        n.id == *id
                            && n.kind == "method"
                            && n.binding_key.is_some()
                            && !private_name(&n.label)
                    })
                {
                    if node.metadata.is_null() {
                        node.metadata = json!({});
                    }
                    node.metadata["binding_aliases"] = json!([format!(
                        "python-member:{}",
                        key.strip_prefix("python:").unwrap()
                    )]);
                }
            }
        }
        if !self.scopes[0].uncertain
            && let Some((package, module)) = self.facts.module.rsplit_once('.')
        {
            for binding in self.scopes[0].bindings.values() {
                if let Binding::Definition { id, .. } = binding
                    && let Some(node) = self.facts.nodes.iter_mut().find(|n| {
                        n.id == *id
                            && matches!(n.kind.as_str(), "class" | "function")
                            && n.binding_key.is_some()
                    })
                {
                    if node.metadata.is_null() {
                        node.metadata = json!({});
                    }
                    node.metadata["binding_aliases"] = json!([format!(
                        "python-member:{package}:{module}.{}",
                        node.qualified_name.as_deref().unwrap()
                    )]);
                }
            }
        }
        for evidence in &self.evidence {
            let key = self.call_key(&evidence.site);
            if evidence.context == "decorator"
                && self.decorator_noise(&evidence.site, key.as_deref())
            {
                continue;
            }
            let reference_id = format!(
                "{}:{}:{}:{}-{}",
                evidence.relation,
                evidence.context,
                evidence.owner,
                evidence.site.start,
                evidence.site.end
            );
            if let Some(node) = self.facts.nodes.iter_mut().find(|n| n.id == evidence.owner) {
                if node.metadata.is_null() {
                    node.metadata = json!({});
                }
                if node.metadata.get("python_references").is_none() {
                    node.metadata["python_references"] = json!([]);
                }
                node.metadata["python_references"].as_array_mut().unwrap().push(json!({
                    "reference_id": reference_id, "context": evidence.context,
                    "line": evidence.site.line, "text": &self.source[evidence.site.start..evidence.site.end],
                }));
            }
            self.facts.references.push(Reference {
                id: reference_id,
                source: evidence.owner.clone(),
                label: evidence.site.parts.join("."),
                relation: evidence.relation.into(),
                file: self.facts.path.clone(),
                line: evidence.site.line,
                candidate_keys: key.into_iter().collect(),
                reason: format!(
                    "{}: target is unavailable, shadowed, or uncertain",
                    evidence.context
                ),
            });
        }
        let callable_keys = self.callable_keys();
        for (index, call) in self.calls.iter().enumerate() {
            let key = callable_keys
                .get(&index)
                .cloned()
                .or_else(|| self.call_key(call));
            let reference_id = format!(
                "call:{}:{}-{}",
                self.scopes[call.scope].owner, call.start, call.end
            );
            if key.as_ref().is_some_and(|k| {
                k.starts_with("python-receiver:") || k.starts_with("python-super:")
            }) {
                let node = self
                    .facts
                    .nodes
                    .iter_mut()
                    .find(|n| n.id == self.scopes[call.scope].owner)
                    .unwrap();
                if node.metadata.is_null() {
                    node.metadata = json!({});
                }
                if node.metadata.get("python_references").is_none() {
                    node.metadata["python_references"] = json!([]);
                }
                node.metadata["python_references"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({
                        "reference_id": reference_id, "context": "receiver_declaration",
                        "line": call.line, "text": &self.source[call.start..call.end],
                    }));
            }
            let reason = if key.is_some() {
                "static target is unavailable or ambiguous"
            } else {
                "dynamic, shadowed, or uncertain Python binding"
            };
            self.facts.references.push(Reference {
                id: reference_id,
                source: self.scopes[call.scope].owner.clone(),
                label: if call.parts.is_empty() {
                    "<dynamic call>".into()
                } else {
                    call.parts.join(".")
                },
                relation: "calls".into(),
                file: self.facts.path.clone(),
                line: call.line,
                candidate_keys: key.into_iter().collect(),
                reason: reason.into(),
            });
        }
        for (class, scope) in self
            .scopes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == ScopeKind::Class)
        {
            let bases: Vec<_> = self
                .evidence
                .iter()
                .filter(|e| e.owner == scope.owner && e.relation == "inherits")
                .map(|e| {
                    self.call_key(&e.site).or_else(|| {
                        if e.site.parts != ["object"] || !self.stars.is_empty() {
                            return None;
                        }
                        let mut current = Some(e.site.scope);
                        while let Some(index) = current {
                            let parent = &self.scopes[index];
                            if parent.uncertain || parent.bindings.contains_key("object") {
                                return None;
                            }
                            current = parent.parent;
                        }
                        Some("python-builtin:object".into())
                    })
                })
                .collect();
            let members: BTreeMap<_, _> = scope
                .bindings
                .iter()
                .map(|(name, binding)| {
                    let key = match binding {
                        Binding::Definition { key, id, .. } if valid.contains_key(id) => {
                            Some(key.clone())
                        }
                        _ => None,
                    };
                    (name.clone(), key)
                })
                .collect();
            if let Some(node) = self.facts.nodes.iter_mut().find(|n| n.id == scope.owner) {
                node.metadata["python_bases"] = json!(bases);
                node.metadata["python_members"] = json!(members);
                node.metadata["python_class_uncertain"] = json!(scope.uncertain);
                node.metadata["python_receiver_writes"] = json!(
                    self.receiver_writes
                        .get(&class)
                        .cloned()
                        .unwrap_or_default()
                );
            }
        }
        self.exports();
        if !self.stars.is_empty() {
            // File-local syntax cannot prove that a star leaves these names in
            // place. Keep the candidate inventory for context, but do not publish
            // keys or aliases to Store until the whole module set proves them.
            for node in &mut self.facts.nodes {
                if node.kind == "module" {
                    continue;
                }
                if let Some(key) = node.binding_key.take() {
                    let name = node
                        .qualified_name
                        .as_deref()
                        .unwrap()
                        .split('.')
                        .next()
                        .unwrap();
                    let aliases = node
                        .metadata
                        .get("binding_aliases")
                        .cloned()
                        .unwrap_or_else(|| json!([]));
                    if node.metadata.is_null() {
                        node.metadata = json!({});
                    }
                    node.metadata["python_pending_binding"] =
                        json!({"name": name, "key": key, "aliases": aliases});
                    node.metadata
                        .as_object_mut()
                        .unwrap()
                        .remove("binding_aliases");
                }
            }
        }
    }
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

impl PythonContext {
    pub fn from_facts(facts: &[FileFacts]) -> Self {
        let mut context = Self::default();
        let mut inventory: Vec<_> = facts.iter().collect();
        inventory.sort_by(|a, b| a.path.cmp(&b.path));
        for facts in inventory {
            // Failed Python parses still reserve module identity, so a second
            // root's same module cannot silently win over an invalid source.
            let root = facts
                .nodes
                .iter()
                .find(|n| n.kind == "module" && n.id.starts_with("python:"));
            if root.is_none() && !facts.path.ends_with(".py") {
                continue;
            }
            let mut bindings = BTreeSet::new();
            for node in &facts.nodes {
                if node.kind == "class"
                    && let Some(name) = &node.qualified_name
                {
                    context
                        .classes
                        .entry(format!("python:{}:{name}", facts.module))
                        .or_default()
                        .push(PythonClass {
                            bases: serde_json::from_value(node.metadata["python_bases"].clone())
                                .unwrap_or_default(),
                            members: serde_json::from_value(
                                node.metadata["python_members"].clone(),
                            )
                            .unwrap_or_default(),
                            uncertain: python_definition_key(node).is_none()
                                || node.metadata["python_literal_bases"] != true
                                || node.metadata["python_class_uncertain"] == true,
                            receiver_writes: serde_json::from_value(
                                node.metadata["python_receiver_writes"].clone(),
                            )
                            .unwrap_or_default(),
                        });
                }
                bindings.extend(python_definition_key(node).map(str::to_owned));
                for aliases in [
                    node.metadata.get("binding_aliases"),
                    node.metadata["python_pending_binding"].get("aliases"),
                ] {
                    if node.kind == "method" {
                        context.methods.extend(
                            aliases
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned),
                        );
                    }
                    bindings.extend(
                        aliases
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned),
                    );
                }
            }
            let metadata = root.map_or(&Value::Null, |n| &n.metadata);
            let exports = metadata
                .get("python_exports")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, entry)| {
                    Some((name.clone(), entry.get("target")?.as_str()?.to_owned()))
                })
                .collect();
            let blocked = metadata
                .get("python_blocked_exports")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            context
                .modules
                .entry(facts.module.clone())
                .or_default()
                .push(PythonModule {
                    exports,
                    definitions: serde_json::from_value(metadata["python_definitions"].clone())
                        .unwrap_or_default(),
                    stars: serde_json::from_value(metadata["python_stars"].clone())
                        .unwrap_or_default(),
                    all: metadata["python_all"].clone(),
                    blocked,
                    uncertain: root.is_none()
                        || metadata
                            .get("python_uncertain")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                });
            context.bindings.extend(bindings);
        }
        // Keep even invalid descendant hierarchies: a conflicting C3 order
        // must not certify calls on an ancestor's implicit receiver.
        for (child, variants) in &context.classes {
            for info in variants {
                for base in info.bases.iter().flatten() {
                    if let Some((base, _)) = context.resolve(base, &mut BTreeSet::new())
                        && context.classes.contains_key(&base)
                    {
                        context
                            .children
                            .entry(base)
                            .or_default()
                            .insert(child.clone());
                    }
                }
            }
        }
        // Stamp binding context, not reference usage or source locations.
        // Ordinary terminal definitions/body edits keep per-file parsing; Store
        // already rebinds references when those defining keys change.
        let mut hash = blake3::Hasher::new();
        hash.update(b"python-import-context-7");
        for (module, files) in &context.modules {
            if files.len() == 1 && files[0].exports.is_empty() && files[0].stars.is_empty() {
                continue;
            }
            let mut variants: Vec<_> = files
                .iter()
                .map(|file| {
                    let resolved: BTreeMap<_, _> = file
                        .exports
                        .keys()
                        .map(|name| {
                            (
                                name,
                                context.resolve(
                                    &format!("python:{module}:{name}"),
                                    &mut BTreeSet::new(),
                                ),
                            )
                        })
                        .collect();
                    let stars = (!file.stars.is_empty()).then(|| {
                        context
                            .star_bindings(
                                file,
                                &mut BTreeSet::from([module.clone()]),
                                &mut BTreeMap::new(),
                            )
                            .map(|bindings| {
                                bindings
                                    .into_iter()
                                    .map(|(name, target)| {
                                        let resolved = target.and_then(|_| {
                                            context.resolve(
                                                &format!("python:{module}:{name}"),
                                                &mut BTreeSet::new(),
                                            )
                                        });
                                        (name, resolved)
                                    })
                                    .collect::<BTreeMap<_, _>>()
                            })
                    });
                    json!([file.exports, resolved, file.stars, stars]).to_string()
                })
                .collect();
            variants.sort();
            let input = json!([module, variants]).to_string();
            hash.update(&(input.len() as u64).to_le_bytes());
            hash.update(input.as_bytes());
        }
        // Fingerprint the available member routes, never their call sites.
        // Adding/removing an inherited call is only a body edit. Inherited
        // endpoints and suppressed aliases still need consumer refresh when the
        // binding context changes; unchanged terminal keys rebind in Store.
        let mut member_keys: BTreeSet<_> = context
            .bindings
            .iter()
            .filter(|key| key.starts_with("python-member:"))
            .cloned()
            .collect();
        let mut mros = BTreeMap::new();
        for class in context.classes.keys() {
            let mro = context.mro(class, &mut BTreeSet::new(), &mut mros);
            // Unknown/changed bases can change a cleared lookup into a direct
            // key even without inherited methods. Ordinary root classes still
            // use Store's terminal-key addition/deletion handling.
            let root_class = mro.as_ref().is_some_and(|order| {
                order.len() == 2 && order[0] == *class && order[1] == "python-builtin:object"
            });
            if !root_class {
                let input = json!(["class", class, mro]).to_string();
                hash.update(&(input.len() as u64).to_le_bytes());
                hash.update(input.as_bytes());
            }
            if let Some(mro) = mro {
                for ancestor in mro {
                    for info in context.classes.get(&ancestor).into_iter().flatten() {
                        for name in info.members.keys() {
                            member_keys.insert(format!(
                                "python-member:{}.{name}",
                                class.strip_prefix("python:").unwrap()
                            ));
                        }
                    }
                }
            }
        }
        for key in member_keys {
            let resolved = context
                .resolve(&key, &mut BTreeSet::new())
                .map(|(key, _)| key);
            if resolved.as_deref() != Some(&key) {
                let input = json!([key, resolved]).to_string();
                hash.update(&(input.len() as u64).to_le_bytes());
                hash.update(input.as_bytes());
            }
            if let Some((class, member)) = key
                .strip_prefix("python-member:")
                .and_then(|k| k.rsplit_once('.'))
            {
                let class = format!("python:{class}");
                if context.classes.contains_key(&class) {
                    let receiver = context.receiver_target(&class, member, false);
                    let super_target = context.receiver_target(&class, member, true);
                    // Only exceptions to ordinary member lookup and available
                    // super routes affect consumers. Never hash receiver sites,
                    // method bodies, byte offsets, or ordinary root definitions.
                    if receiver != resolved || super_target.is_some() {
                        let input = json!(["receiver", key, receiver, super_target]).to_string();
                        hash.update(&(input.len() as u64).to_le_bytes());
                        hash.update(input.as_bytes());
                    }
                }
            }
        }
        context.fingerprint = hash.finalize().to_hex().to_string();
        context
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Publish verified star-dependent bindings and rewrite references to their
    /// defining keys; native IDs and ownership never move to a forwarding module.
    /// Reparse callers when the context fingerprint changes, including on export
    /// deletion or ambiguity.
    pub fn apply(&self, facts: &mut FileFacts) {
        if !facts
            .nodes
            .iter()
            .any(|n| n.kind == "module" && n.id.starts_with("python:"))
        {
            return;
        }
        for node in &mut facts.nodes {
            let Some(pending) = node.metadata.get("python_pending_binding").cloned() else {
                continue;
            };
            let publish = pending["name"]
                .as_str()
                .is_some_and(|name| self.unshadowed_binding(&facts.module, name));
            node.binding_key = if publish {
                pending["key"].as_str().map(str::to_owned)
            } else {
                None
            };
            let metadata = node.metadata.as_object_mut().unwrap();
            metadata.remove("binding_aliases");
            if publish
                && pending["aliases"]
                    .as_array()
                    .is_some_and(|aliases| !aliases.is_empty())
            {
                metadata.insert("binding_aliases".into(), pending["aliases"].clone());
            }
        }
        let mut expanded = Vec::new();
        for reference in &facts.references {
            if reference.relation != "imports" || reference.label != "*" {
                continue;
            }
            let Some(module) = reference
                .candidate_keys
                .first()
                .and_then(|k| k.strip_prefix("module:"))
            else {
                continue;
            };
            if let Some(exports) =
                self.star_exports(module, &mut BTreeSet::new(), &mut BTreeMap::new())
            {
                for (name, target) in exports {
                    let Some(target) = target else { continue };
                    let mut symbol = reference.clone();
                    symbol.id = format!("{}:{name}", symbol.id);
                    symbol.label = name;
                    symbol.candidate_keys = vec![target];
                    if !facts.references.iter().any(|r| r.id == symbol.id) {
                        expanded.push(symbol);
                    }
                }
            }
        }
        // A subclass can override an implicit receiver's target. Keep the
        // declaration reachable without claiming that it is the runtime callee.
        for reference in &facts.references {
            if reference.relation != "calls" {
                continue;
            }
            let Some(key) = reference.candidate_keys.first() else {
                continue;
            };
            let Some((class, member)) = key
                .strip_prefix("python-receiver:")
                .and_then(|rest| rest.rsplit_once('.'))
            else {
                continue;
            };
            let class = format!("python:{class}");
            if self.receiver_target(&class, member, false).is_some() {
                continue;
            }
            let Some(mro) = self.mro(&class, &mut BTreeSet::new(), &mut BTreeMap::new()) else {
                continue;
            };
            let Some(target) = self
                .receiver_in_mro(&mro, &class, member, false)
                .filter(|target| self.methods.contains(target))
            else {
                continue;
            };
            let mut declaration = reference.clone();
            declaration.id.push_str(":declared_member");
            declaration.relation = "declared_member".into();
            declaration.candidate_keys = vec![target];
            declaration.reason =
                "receiver declaration; subclass dispatch remains unresolved".into();
            if !facts.references.iter().any(|r| r.id == declaration.id) {
                expanded.push(declaration);
            }
        }
        facts.references.extend(expanded);
        for reference in &mut facts.references {
            let mut keys = Vec::new();
            for key in &reference.candidate_keys {
                match self.resolve(key, &mut BTreeSet::new()) {
                    Some((target, claimed)) => {
                        // Calling an imported module is not a statically known
                        // function call, even when its module identity is known.
                        if (reference.relation != "calls" || !target.starts_with("module:"))
                            && !keys.contains(&target)
                        {
                            keys.push(target);
                        }
                        if claimed {
                            break;
                        }
                    }
                    None => {
                        keys.clear();
                        break;
                    }
                }
            }
            reference.candidate_keys = keys;
        }
    }

    // The bool means an explicit binding owns this name, preventing a missing
    // named re-export from falling through to an unrelated same-named submodule.
    fn resolve(&self, key: &str, seen: &mut BTreeSet<String>) -> Option<(String, bool)> {
        if seen.len() >= 64 || !seen.insert(key.to_owned()) {
            return None;
        }
        if let Some(rest) = key
            .strip_prefix("python-receiver:")
            .or_else(|| key.strip_prefix("python-super:"))
        {
            let (class, member) = rest.rsplit_once('.')?;
            return self
                .receiver_target(
                    &format!("python:{class}"),
                    member,
                    key.starts_with("python-super:"),
                )
                .map(|target| (target, true));
        }
        if let Some(local) = key.strip_prefix("python-local:") {
            let (module, local) = local.split_once(':')?;
            let (name, target) = local.split_once(':')?;
            if !self.unshadowed_binding(module, name) {
                return None;
            }
            return self.resolve(target, seen);
        }
        if let Some(module) = key.strip_prefix("module:") {
            if self
                .modules
                .get(module)
                .is_some_and(|files| files.len() != 1)
            {
                return None;
            }
            return Some((key.into(), self.bindings.contains(key)));
        }
        let (member, rest) = if let Some(rest) = key.strip_prefix("python-member:") {
            (true, rest)
        } else if let Some(rest) = key.strip_prefix("python:") {
            (false, rest)
        } else {
            return Some((key.into(), false));
        };
        let Some((module, name)) = rest.split_once(':') else {
            return Some((key.into(), false));
        };
        let (head, suffix) = name
            .split_once('.')
            .map_or((name, None), |(head, tail)| (head, Some(tail)));
        let mut known_name = false;
        if let Some(files) = self.modules.get(module) {
            if files.len() != 1 {
                return None;
            }
            let info = &files[0];
            if info.uncertain || info.blocked.contains(head) {
                // A terminal name keeps its original lookup key. The Store
                // removes/rebinds that definition when this module changes,
                // without reparsing unrelated callers. Claim the name to stop
                // an invalid named import falling back to a sibling submodule.
                let missing_member = member
                    && !self.bindings.contains(key)
                    && name.rsplit_once('.').is_some_and(|(class, _)| {
                        !self
                            .classes
                            .contains_key(&format!("python:{module}:{class}"))
                    });
                return ((!member && suffix.is_none()) || missing_member)
                    .then(|| (key.into(), true));
            }
            let stars = self.star_bindings(
                info,
                &mut BTreeSet::from([module.to_owned()]),
                &mut BTreeMap::new(),
            )?;
            let star = stars.get(head);
            if star.is_some()
                && (info.exports.contains_key(head) || info.definitions.contains_key(head))
            {
                return None;
            }
            let target = match star {
                Some(target) => Some(target.as_ref()?),
                None => info.exports.get(head),
            };
            if let Some(target) = target {
                let forwarded = match (target.strip_prefix("module:"), suffix) {
                    (Some(module), Some(tail)) => Self::module_member(module, tail),
                    (Some(_), None) => target.clone(),
                    (None, Some(tail)) => {
                        let symbol = target.strip_prefix("python:")?;
                        format!(
                            "{}:{symbol}.{tail}",
                            if member { "python-member" } else { "python" }
                        )
                    }
                    (None, None) => target.clone(),
                };
                return self.resolve(&forwarded, seen).map(|(key, _)| (key, true));
            }
            known_name = self.bindings.contains(&format!("python:{module}:{head}"));
            if !info.stars.is_empty() && !known_name {
                return None;
            }
        }
        if member && let Some((class, method)) = name.rsplit_once('.') {
            let class_key = format!("python:{module}:{class}");
            if self.classes.contains_key(&class_key) {
                let mro = self.mro(&class_key, &mut BTreeSet::new(), &mut BTreeMap::new())?;
                for class in mro {
                    if class == "python-builtin:object" {
                        continue;
                    }
                    let info = self.classes.get(&class)?.first()?;
                    if let Some(target) = info.members.get(method) {
                        let target = target.as_ref()?;
                        let alias = format!("python-member:{}", target.strip_prefix("python:")?);
                        // A same-spelled submodule alias cannot prove an inherited member.
                        if alias != key && self.bindings.contains(key) {
                            return None;
                        }
                        return self.bindings.contains(&alias).then_some((alias, true));
                    }
                }
                // Preserve an unavailable terminal lookup for Store to bind if
                // the method is added later. A submodule alias with that spelling
                // cannot stand in for the verified class's missing member.
                return (!self.bindings.contains(key)).then(|| (key.into(), true));
            }
        }
        if self.bindings.contains(key) || known_name {
            return Some((key.into(), true));
        }
        // `from package import child` can import a visible submodule, including
        // namespace packages. Never select one when a named binding owns child.
        let submodule = format!("{module}.{head}");
        if self.modules.contains_key(&submodule) {
            let target = match suffix {
                Some(tail) => Self::module_member(&submodule, tail),
                None => format!("module:{submodule}"),
            };
            return self.resolve(&target, seen).map(|(key, _)| (key, true));
        }
        Some((key.into(), false))
    }

    fn receiver_target(&self, class: &str, member: &str, super_call: bool) -> Option<String> {
        let mut cache = BTreeMap::new();
        let mro = self.mro(class, &mut BTreeSet::new(), &mut cache)?;
        let target = self.receiver_in_mro(&mro, class, member, super_call)?;
        let mut pending: Vec<_> = self.children.get(class).into_iter().flatten().collect();
        let mut seen = BTreeSet::new();
        while let Some(child) = pending.pop() {
            if !seen.insert(child) {
                continue;
            }
            let mro = self.mro(child, &mut BTreeSet::new(), &mut cache)?;
            if self
                .receiver_in_mro(&mro, class, member, super_call)
                .as_ref()
                != Some(&target)
            {
                return None;
            }
            pending.extend(self.children.get(child).into_iter().flatten());
        }
        Some(target)
    }

    fn receiver_in_mro(
        &self,
        mro: &[String],
        class: &str,
        member: &str,
        super_call: bool,
    ) -> Option<String> {
        for ancestor in mro {
            for info in self.classes.get(ancestor).into_iter().flatten() {
                if info.receiver_writes.contains("__class__")
                    || info.receiver_writes.contains("*")
                    || (!super_call
                        && (info.receiver_writes.contains(member)
                            || info.members.contains_key("__getattribute__")
                            || info.members.contains_key("__getattr__")))
                {
                    return None;
                }
            }
        }
        let start = if super_call {
            mro.iter().position(|key| key == class)? + 1
        } else {
            0
        };
        for ancestor in &mro[start..] {
            if ancestor == "python-builtin:object" {
                continue;
            }
            let info = self.classes.get(ancestor)?.first()?;
            if let Some(target) = info.members.get(member) {
                let alias = format!(
                    "python-member:{}",
                    target.as_ref()?.strip_prefix("python:")?
                );
                return self.methods.contains(&alias).then_some(alias);
            }
        }
        // A missing ordinary member keeps a terminal key so Store can bind an
        // added declaration without reparsing all unrelated Python files. An
        // existing alias cannot establish a member absent from this MRO.
        let key = format!(
            "python-member:{}.{}",
            class.strip_prefix("python:").unwrap(),
            member
        );
        (!super_call && !self.bindings.contains(&key)).then_some(key)
    }

    fn unshadowed_binding(&self, module: &str, name: &str) -> bool {
        let Some(files) = self.modules.get(module) else {
            return false;
        };
        files.len() == 1
            && !files[0].uncertain
            && self
                .star_bindings(
                    &files[0],
                    &mut BTreeSet::from([module.to_owned()]),
                    &mut BTreeMap::new(),
                )
                .is_some_and(|stars| !stars.contains_key(name))
    }

    // Keep blocked names in the map: absence and an explicitly unknown export
    // have different meanings when a consumer also has other star imports.
    fn star_bindings(
        &self,
        info: &PythonModule,
        seen: &mut BTreeSet<String>,
        cache: &mut BTreeMap<String, Option<PythonExports>>,
    ) -> Option<PythonExports> {
        let mut names = BTreeMap::new();
        for module in &info.stars {
            for (name, target) in self.star_exports(module, seen, cache)? {
                names
                    .entry(name)
                    .and_modify(|value| *value = None)
                    .or_insert(target);
            }
        }
        Some(names)
    }

    fn star_exports(
        &self,
        module: &str,
        seen: &mut BTreeSet<String>,
        cache: &mut BTreeMap<String, Option<PythonExports>>,
    ) -> Option<PythonExports> {
        if let Some(result) = cache.get(module) {
            return result.clone();
        }
        if seen.len() >= 64 || !seen.insert(module.to_owned()) {
            return None;
        }
        let result = (|| {
            let files = self.modules.get(module)?;
            if files.len() != 1 {
                return None;
            }
            let info = &files[0];
            if info.uncertain || info.all == false {
                return None;
            }
            let mut names = self.star_bindings(info, seen, cache)?;
            for (name, target) in info.definitions.iter().chain(&info.exports) {
                names
                    .entry(name.clone())
                    .and_modify(|value| *value = None)
                    .or_insert_with(|| Some(target.clone()));
            }
            for name in &info.blocked {
                names.insert(name.clone(), None);
            }
            if let Some(all) = info.all.as_array() {
                Some(
                    all.iter()
                        .filter_map(Value::as_str)
                        .map(|name| (name.to_owned(), names.get(name).cloned().flatten()))
                        .collect(),
                )
            } else {
                names.retain(|name, _| !name.starts_with('_'));
                Some(names)
            }
        })();
        seen.remove(module);
        cache.insert(module.to_owned(), result.clone());
        result
    }

    // C3 linearization: never select a convenient base when the written order
    // is inconsistent, cyclic, incomplete, or depends on a custom metaclass.
    fn mro(
        &self,
        key: &str,
        seen: &mut BTreeSet<String>,
        cache: &mut BTreeMap<String, Option<Vec<String>>>,
    ) -> Option<Vec<String>> {
        if let Some(result) = cache.get(key) {
            return result.clone();
        }
        if key == "python-builtin:object" {
            return Some(vec![key.to_owned()]);
        }
        if seen.len() >= 64 || !seen.insert(key.to_owned()) {
            return None;
        }
        let result = (|| {
            let (resolved, _) = self.resolve(key, &mut BTreeSet::new())?;
            if resolved != key {
                return self.mro(&resolved, seen, cache);
            }
            let classes = self.classes.get(key)?;
            if classes.len() != 1 || classes[0].uncertain {
                return None;
            }
            let info = &classes[0];
            let mut bases = Vec::new();
            let mut sequences = Vec::new();
            for base in &info.bases {
                let (base, _) = self.resolve(base.as_ref()?, &mut BTreeSet::new())?;
                if bases.contains(&base) {
                    return None;
                }
                sequences.push(self.mro(&base, seen, cache)?);
                bases.push(base);
            }
            if bases.is_empty() {
                bases.push("python-builtin:object".into());
            }
            sequences.push(bases);
            let mut result = vec![key.to_owned()];
            while sequences.iter().any(|s| !s.is_empty()) {
                let head = sequences
                    .iter()
                    .filter_map(|s| s.first())
                    .find(|candidate| {
                        sequences
                            .iter()
                            .all(|s| !s.iter().skip(1).any(|n| n == *candidate))
                    })?
                    .clone();
                result.push(head.clone());
                for sequence in &mut sequences {
                    if sequence.first() == Some(&head) {
                        sequence.remove(0);
                    }
                }
            }
            Some(result)
        })();
        seen.remove(key);
        cache.insert(key.to_owned(), result.clone());
        result
    }

    fn module_member(module: &str, name: &str) -> String {
        format!(
            "{}:{module}:{name}",
            if name.contains('.') {
                "python-member"
            } else {
                "python"
            }
        )
    }
}
