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

fn language(path: &str) -> Option<(&'static str, Language)> {
    Some(match path.rsplit_once('.')?.1 {
        "c" | "h" => ("c", tree_sitter_c::LANGUAGE.into()),
        "cu" | "cuh" => ("cpp", tree_sitter_cuda::LANGUAGE.into()),
        "cc" | "cpp" | "cxx" | "C" | "hh" | "hpp" | "hxx" | "H" | "metal" => {
            ("cpp", tree_sitter_cpp::LANGUAGE.into())
        }
        "java" => ("java", tree_sitter_java::LANGUAGE.into()),
        "cs" => ("csharp", tree_sitter_c_sharp::LANGUAGE.into()),
        "kt" | "kts" => ("kotlin", tree_sitter_kotlin_ng::LANGUAGE.into()),
        "swift" => ("swift", tree_sitter_swift::LANGUAGE.into()),
        _ => return None,
    })
}

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
impl<'s, 't> Compiled<'s, 't> {
    fn name(&self, n: Syntax<'_>) -> Option<String> {
        if n.kind() == "field_expression" && self.e.language == "cpp" {
            let receiver = n.child_by_field_name("argument")?;
            let field = n.child_by_field_name("field")?;
            return self
                .name(receiver)
                .zip(self.name(field))
                .map(|(a, b)| format!("{a}.{b}"));
        }
        simple_name(self.e.text(n))
    }
    fn key(&self, qualified: &str) -> String {
        self.member_key("symbol", qualified)
    }
    fn member_key(&self, kind: &str, qualified: &str) -> String {
        for header in &self.includes {
            if let Some(symbol) = qualified.strip_prefix(&format!("@{header}.")) {
                return header_key(kind, header, symbol);
            }
        }
        format!("{}:{kind}:{qualified}", self.e.language)
    }
    fn bind(&mut self, scope: usize, name: &str, value: Binding) {
        if self.e.language == "swift"
            && let Some(local) = self
                .swift_callables
                .locals
                .get_mut(&(scope, name.to_owned()))
        {
            local.blocked = true;
        }
        self.scopes
            .get_mut(&scope)
            .unwrap()
            .bindings
            .entry(name.into())
            .and_modify(|v| *v = Binding::Unknown)
            .or_insert(value);
    }
    fn lookup(&self, mut scope: usize, name: &str) -> Option<&Binding> {
        loop {
            let ctx = &self.scopes[&scope];
            if ctx.uncertain {
                return Some(&Binding::Unknown);
            }
            if let Some(b) = ctx.bindings.get(name) {
                return Some(b);
            }
            scope = self.e.scopes[scope].parent?;
        }
    }
    // This lookup also sees uncertain closure scopes, solely to invalidate a
    // captured local. Positive function identity still requires normal lookup.
    fn swift_binding_scope(&self, mut scope: usize, name: &str) -> Option<usize> {
        loop {
            if self.scopes[&scope].bindings.contains_key(name) {
                return Some(scope);
            }
            scope = self.e.scopes[scope].parent?;
        }
    }
    fn swift_function_value(&self, scope: usize, n: Syntax<'_>) -> Option<SwiftFunctionValue> {
        if n.kind() != "simple_identifier" {
            return None;
        }
        let name = self.e.text(n);
        let Binding::Symbol(key) = self.lookup(scope, name)? else {
            return None;
        };
        let node = (*self.swift_callables.functions.get(key)?)?;
        Some(SwiftFunctionValue {
            scope: self.swift_binding_scope(scope, name)?,
            name: name.to_owned(),
            key: key.clone(),
            node,
        })
    }
    fn swift_conditional_scope(&self, mut scope: usize) -> bool {
        loop {
            if self
                .swift_callables
                .conditional_depth
                .get(&scope)
                .is_some_and(|depth| *depth > 0)
            {
                return true;
            }
            let Some(parent) = self.e.scopes[scope].parent else {
                return false;
            };
            scope = parent;
        }
    }
    fn swift_callable_read(&mut self, n: Syntax<'t>, scope: usize) {
        if n.kind() == "directive" {
            // Swift's #if markers are siblings, not containers for the guarded
            // statements. Decline the affected body's entire callable timeline.
            self.swift_callables
                .conditional_owners
                .insert(self.e.scopes[scope].owner.clone());
            let depth = self
                .swift_callables
                .conditional_depth
                .entry(scope)
                .or_default();
            match n.child(0).map(|c| c.kind()) {
                Some("#if") => *depth += 1,
                Some("#elseif" | "#else") => *depth = (*depth).max(1),
                Some("#endif") => *depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if n.kind() == "assignment"
            && let Some(name) = swift_assignment_name(n)
            && self.swift_binding_scope(scope, self.e.text(name)).is_none()
        {
            // The ordinary assignment visitor creates an unknown binding for
            // this spelling. Remember the write before that can hide a later
            // outer declaration captured by a nested function.
            self.swift_callables
                .unbound_writes
                .push((scope, self.e.text(name).to_owned()));
        }
        if n.kind() == "simple_identifier"
            && !n.parent().is_some_and(|p| p.kind() == "navigation_suffix")
        {
            // A member suffix is not a lexical read. Exclude it from both the
            // immediate capture check and the deferred capture/escape pass.
            // The navigation receiver still visits the normal lexical path.
            let name = self.e.text(n).to_owned();
            if let Some(binding_scope) = self.swift_binding_scope(scope, &name)
                && self.e.scopes[scope].owner != self.e.scopes[binding_scope].owner
                && let Some(local) = self.swift_callables.locals.get_mut(&(binding_scope, name))
            {
                local.blocked = true;
            }
            // Resolve captures after all declarations have been visited: nested
            // functions and closures do not execute on their textual timeline.
            self.swift_callables.uses.push((n, scope));
        }
        if n.kind() != "call_expression" {
            return;
        }
        let Some(target) = n.named_child(0).filter(|c| c.kind() == "simple_identifier") else {
            return;
        };
        let name = self.e.text(target).to_owned();
        let Some(binding_scope) = self.swift_binding_scope(scope, &name) else {
            return;
        };
        let binding = (binding_scope, name);
        let Some(local) = self.swift_callables.locals.get(&binding) else {
            return;
        };
        // Swift represents subscripts as call expressions too. Only an ordinary
        // parenthesized invocation, directly in this function body, has proof.
        let ordinary = n.named_child(1).is_some_and(|suffix| {
            suffix.kind() == "call_suffix"
                && suffix.named_child_count() == 1
                && suffix.named_child(0).is_some_and(|args| {
                    args.kind() == "value_arguments"
                        && args.child(0).is_some_and(|c| c.kind() == "(")
                })
        });
        let statement = n
            .parent()
            .filter(|p| {
                p.kind() == "control_transfer_statement"
                    && p.child(0).is_some_and(|c| c.kind() == "return")
                    && p.child_by_field_name("result") == Some(n)
            })
            .unwrap_or(n);
        let value = if ordinary && swift_body_statement(statement) && !self.uncertain(scope) {
            local.value.clone()
        } else {
            None
        };
        self.swift_callables.calls.insert(
            (scope, n.start_byte()),
            SwiftCallableCall {
                binding,
                assignment: local.assignment,
                value,
            },
        );
    }
    fn swift_callable_write(&mut self, n: Syntax<'_>, scope: usize) {
        if n.kind() == "property_declaration"
            && self.scopes[&scope].local
            && swift_body_statement(n)
            && !self.uncertain(scope)
        {
            let Some(pattern) = n.child_by_field_name("name") else {
                return;
            };
            let Some(name) = pattern.child_by_field_name("bound_identifier") else {
                return;
            };
            let mut cursor = n.walk();
            if name.kind() != "simple_identifier"
                || pattern.child_count() != 1
                || n.children_by_field_name("name", &mut cursor).count() != 1
                || children(n).iter().any(|c| {
                    matches!(
                        c.kind(),
                        "modifiers"
                            | "computed_property"
                            | "willset_didset_block"
                            | "type_constraints"
                    )
                })
            {
                return;
            }
            let Some(value) = n.child_by_field_name("value") else {
                return;
            };
            let value = self.swift_function_value(scope, value);
            let mutable = children(n).iter().any(|c| {
                c.kind() == "value_binding_pattern"
                    && c.child_by_field_name("mutability")
                        .is_some_and(|m| self.e.text(m) == "var")
            });
            let span = (n.start_byte(), n.end_byte());
            self.swift_callables
                .locals
                .entry((scope, self.e.text(name).to_owned()))
                .and_modify(|local| local.blocked = true)
                .or_insert(SwiftCallableLocal {
                    declaration: span,
                    assignment: span,
                    value,
                    mutable,
                    blocked: false,
                });
        } else if n.kind() == "assignment" {
            let Some(name) = swift_assignment_name(n) else {
                return;
            };
            let name = self.e.text(name).to_owned();
            let Some(binding_scope) = self.swift_binding_scope(scope, &name) else {
                return;
            };
            let value = n
                .child_by_field_name("result")
                .and_then(|value| self.swift_function_value(scope, value));
            if let Some(local) = self.swift_callables.locals.get_mut(&(binding_scope, name)) {
                if scope != binding_scope
                    || !swift_body_statement(n)
                    || !local.mutable
                    || !n
                        .child_by_field_name("operator")
                        .is_some_and(|op| self.e.text(op) == "=")
                {
                    local.blocked = true;
                } else {
                    local.value = value;
                    local.assignment = (n.start_byte(), n.end_byte());
                }
            }
        }
    }
    fn swift_callable_escapes(&mut self) {
        for (mut scope, name) in std::mem::take(&mut self.swift_callables.unbound_writes) {
            loop {
                if let Some(local) = self.swift_callables.locals.get_mut(&(scope, name.clone())) {
                    local.blocked = true;
                    break;
                }
                if let Some(Binding::Symbol(key)) = self.scopes[&scope].bindings.get(&name)
                    && self.swift_callables.functions.contains_key(key)
                {
                    self.swift_callables.escaped.insert((scope, name));
                    break;
                }
                let Some(parent) = self.e.scopes[scope].parent else {
                    break;
                };
                scope = parent;
            }
        }
        for (n, scope) in std::mem::take(&mut self.swift_callables.uses) {
            let name = self.e.text(n).to_owned();
            let Some(binding_scope) = self.swift_binding_scope(scope, &name) else {
                continue;
            };
            let binding = (binding_scope, name);
            let escaped = std::iter::successors(n.parent(), |p| p.parent())
                .take_while(|p| p.kind() != "statements")
                .any(|p| {
                    (p.kind() == "prefix_expression"
                        && p.child_by_field_name("operation")
                            .is_some_and(|op| self.e.text(op) == "&"))
                        || (p.kind() == "assignment"
                            && swift_assignment_name(p).is_none()
                            && p.child_by_field_name("target").is_some_and(|target| {
                                target.start_byte() <= n.start_byte()
                                    && n.end_byte() <= target.end_byte()
                            }))
                });
            if escaped {
                self.swift_callables.escaped.insert(binding.clone());
            }
            if let Some(local) = self.swift_callables.locals.get_mut(&binding)
                && (escaped || self.e.scopes[scope].owner != self.e.scopes[binding_scope].owner)
            {
                local.blocked = true;
            }
        }
    }
    fn swift_callable_target<'a>(
        &self,
        call: &'a SwiftCallableCall,
    ) -> Option<&'a SwiftFunctionValue> {
        if self.swift_callables.locals[&call.binding].blocked
            || self
                .swift_callables
                .conditional_owners
                .contains(&self.e.scopes[call.binding.0].owner)
        {
            return None;
        }
        let value = call.value.as_ref()?;
        // A snapshot records a declaration identity, never just a spelling. A
        // later overload, shadowing declaration, or unknown write invalidates it.
        (matches!(self.scopes[&value.scope].bindings.get(&value.name),
            Some(Binding::Symbol(key)) if key == &value.key)
            && !self
                .swift_callables
                .escaped
                .contains(&(value.scope, value.name.clone()))
            && self.swift_callables.functions.get(&value.key) == Some(&Some(value.node))
            && self.e.facts.nodes[value.node].binding_key.as_ref() == Some(&value.key))
        .then_some(value)
    }
    fn child(&mut self, scope: usize, n: Syntax<'_>) -> usize {
        let child = self.e.block(scope, n);
        let mut ctx = self.scopes[&scope].clone();
        ctx.bindings.clear();
        self.scopes.insert(child, ctx);
        child
    }
    fn qualify(&self, scope: usize, name: &str) -> Option<String> {
        let (head, tail) = name.split_once('.').unwrap_or((name, ""));
        match self.lookup(scope, head) {
            Some(Binding::Type(q)) => {
                return Some(if tail.is_empty() {
                    q.clone()
                } else {
                    format!("{q}.{tail}")
                });
            }
            Some(Binding::Module(module)) if !tail.is_empty() => {
                return Some(format!("!{module}.{tail}"));
            }
            Some(Binding::Unknown | Binding::TypeParameter | Binding::Value(_)) => return None,
            _ => {}
        }
        if name.contains('.') {
            Some(name.into())
        } else if matches!(self.e.language, "c" | "cpp")
            && self.includes.len() == 1
            && self.scopes[&scope].namespace.starts_with('@')
        {
            Some(format!("@{}.{name}", self.includes[0]))
        } else {
            Some(format!("{}.{name}", self.scopes[&scope].namespace))
        }
    }
    fn define(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        name: &str,
        kind: &str,
        qualified: String,
        public: bool,
    ) -> usize {
        let key = self.key(&qualified);
        let cross_project_public = self.cross_project_public(n, scope, kind, &qualified);
        let swift_exported = self.e.language == "swift"
            && !self.scopes[&scope].local
            && (self.modifier(n, "public") || self.modifier(n, "open"))
            && self
                .e
                .facts
                .nodes
                .iter()
                .find(|node| node.id == self.e.scopes[scope].owner)
                .is_some_and(|node| {
                    node.kind == "module"
                        || (node.kind != "extension" && node.metadata["swift_exported"] == true)
                });
        let child = self
            .e
            .define(n, scope, name, kind, public.then_some(key), false);
        self.e.facts.nodes.last_mut().unwrap().metadata["qualified_symbol"] = json!(qualified);
        self.e.facts.nodes.last_mut().unwrap().qualified_name = Some(qualified.clone());
        self.e.facts.nodes.last_mut().unwrap().metadata["cross_project_public"] =
            json!(cross_project_public);
        self.e.facts.nodes.last_mut().unwrap().metadata["explicit_public"] =
            json!(self.modifier(n, "public") || self.modifier(n, "open"));
        self.e.facts.nodes.last_mut().unwrap().metadata["declaration_certain"] =
            json!(!self.uncertain(scope));
        self.e.facts.nodes.last_mut().unwrap().metadata["partial"] =
            json!(self.modifier(n, "partial"));
        self.e.facts.nodes.last_mut().unwrap().metadata["member_accessible"] = json!(
            !self.modifier(n, "private")
                && !self.modifier(n, "fileprivate")
                && !self.modifier(n, "protected")
                && (self.e.language != "java"
                    || self.modifier(n, "public")
                    || self.scopes[&scope].abstract_members)
                && (!matches!(self.e.language, "cpp" | "csharp")
                    || cross_project_public
                    || self.modifier(n, "public")
                    || self.scopes[&scope].abstract_members
                    || (self.e.language == "cpp" && self.cpp_member_public(n)))
        );
        if self.e.language == "swift" {
            self.e.facts.nodes.last_mut().unwrap().metadata["explicit_access"] = json!(
                [
                    "public",
                    "open",
                    "internal",
                    "package",
                    "fileprivate",
                    "private"
                ]
                .into_iter()
                .find(|access| self.modifier(n, access))
            );
            self.e.facts.nodes.last_mut().unwrap().metadata["swift_exported"] =
                json!(swift_exported);
        }
        let mut ctx = self.scopes[&scope].clone();
        ctx.prefix = qualified;
        ctx.bindings.clear();
        self.scopes.insert(child, ctx);
        child
    }
    // Access evidence is independent of callable binding: a public virtual method
    // is still dynamic, and a public member of a hidden type is not exported.
    fn cross_project_public(
        &self,
        n: Syntax<'_>,
        scope: usize,
        kind: &str,
        qualified: &str,
    ) -> bool {
        if self.scopes[&scope].local
            || self.uncertain(scope)
            || !matches!(
                kind,
                "namespace"
                    | "class"
                    | "struct"
                    | "union"
                    | "enum"
                    | "interface"
                    | "type"
                    | "function"
                    | "method"
                    | "declaration"
            )
            || ["private", "protected", "internal", "file"]
                .iter()
                .any(|m| self.modifier(n, m))
        {
            return false;
        }
        let Some(owner) = self
            .e
            .facts
            .nodes
            .iter()
            .find(|node| node.id == self.e.scopes[scope].owner)
        else {
            return false;
        };
        if owner.kind != "module" && owner.metadata["cross_project_public"] != true {
            return false;
        }
        match self.e.language {
            "java" | "csharp" => {
                kind == "namespace" || self.modifier(n, "public") || owner.kind == "interface"
            }
            "kotlin" => true,
            "cpp" => {
                if qualified.starts_with('@')
                    || matches!(
                        self.e.facts.nodes[0].metadata["dialect"].as_str(),
                        Some("cpp_cli" | "metal")
                    )
                {
                    return false;
                }
                if kind == "namespace" {
                    return true;
                }
                if self.scopes[&scope].class.is_none() {
                    // Qualified out-of-line members need their class declaration
                    // to prove access; this file alone cannot grant it.
                    return !self.modifier(n, "static")
                        && !n
                            .child_by_field_name("declarator")
                            .and_then(declarator_name)
                            .is_some_and(|name| self.e.text(name).contains("::"));
                }
                self.cpp_member_public(n)
            }
            _ => false,
        }
    }
    fn cpp_member_public(&self, mut declaration: Syntax<'_>) -> bool {
        while let Some(parent) = declaration.parent() {
            if parent.kind() == "friend_declaration" {
                return false;
            }
            if parent.kind() == "field_declaration_list" {
                let mut public = parent
                    .parent()
                    .is_some_and(|ty| matches!(ty.kind(), "struct_specifier" | "union_specifier"));
                for sibling in children(parent) {
                    if sibling.start_byte() >= declaration.start_byte() {
                        break;
                    }
                    if sibling.kind().starts_with("preproc_") {
                        return false;
                    }
                    if sibling.kind() == "access_specifier" {
                        public = self.e.text(sibling).trim().trim_end_matches(':') == "public";
                    }
                }
                return public;
            }
            declaration = parent;
        }
        false
    }
    fn visit(&mut self, n: Syntax<'t>, mut scope: usize) {
        let kind = n.kind();
        if self.e.language == "swift" {
            self.swift_callable_read(n, scope);
        }
        if kind == "template_declaration" {
            scope = self.child(scope, n);
        }
        if matches!(
            kind,
            "package_declaration" | "package_header" | "file_scoped_namespace_declaration"
        ) {
            return;
        }
        if matches!(
            kind,
            "import_declaration"
                | "import"
                | "using_directive"
                | "using_declaration"
                | "namespace_alias_definition"
                | "preproc_include"
        ) {
            self.import(n, scope);
            return;
        }
        if matches!(kind, "namespace_definition" | "namespace_declaration") {
            let name = n.child_by_field_name("name").and_then(|n| self.name(n));
            let prefix = name
                .as_ref()
                .map(|s| {
                    let p = &self.scopes[&scope].prefix;
                    if p.starts_with('@') {
                        s.clone()
                    } else {
                        format!("{p}.{s}")
                    }
                })
                .unwrap_or_else(|| format!("@{}:anonymous@{}", self.e.facts.path, n.start_byte()));
            let child = self.define(
                n,
                scope,
                name.as_deref().unwrap_or("<anonymous>"),
                "namespace",
                prefix.clone(),
                true,
            );
            self.scopes.get_mut(&child).unwrap().namespace = prefix.clone();
            if let Some(name) = name {
                self.bind(scope, &name, Binding::Type(prefix));
            }
            if let Some(body) = n.child_by_field_name("body") {
                for c in children(body) {
                    self.visit(c, child);
                }
            }
            return;
        }
        let type_kind = match kind {
            "class_specifier" | "class_declaration" | "object_declaration" | "companion_object"
            | "record_declaration" => Some("class"),
            "struct_specifier" | "struct_declaration" => Some("struct"),
            "union_specifier" => Some("union"),
            "enum_specifier" | "enum_declaration" => Some("enum"),
            "interface_declaration" | "annotation_type_declaration" | "protocol_declaration" => {
                Some("interface")
            }
            _ => None,
        };
        if let Some(mut ty) = type_kind {
            // A use such as `struct Item *value` is type evidence, not another definition.
            if matches!(
                kind,
                "struct_specifier" | "class_specifier" | "union_specifier" | "enum_specifier"
            ) && n.child_by_field_name("body").is_none()
            {
                return;
            }
            if let Some(name) = n
                .child_by_field_name("name")
                .and_then(|n| self.name(n))
                .or_else(|| (kind == "companion_object").then(|| "Companion".into()))
            {
                let declaration = n
                    .child_by_field_name("declaration_kind")
                    .map(|n| self.e.text(n))
                    .unwrap_or(ty);
                if self.e.language == "kotlin" && self.modifier(n, "interface") {
                    ty = "interface";
                }
                if self.e.language == "kotlin" && self.modifier(n, "enum") {
                    ty = "enum";
                }
                if matches!(declaration, "struct" | "enum" | "extension") {
                    ty = declaration;
                }
                let q = if self.scopes[&scope].local
                    || self.modifier(n, "private")
                    || self.modifier(n, "fileprivate")
                {
                    format!(
                        "@{}:{}.{}",
                        self.e.facts.path, self.e.scopes[scope].owner, name
                    )
                } else {
                    format!("{}.{name}", self.scopes[&scope].prefix)
                };
                let closed = ty == "struct"
                    || ty == "enum"
                    || (ty == "extension"
                        && !self.modifier(n, "dynamic")
                        && !self.e.text(n).contains("@objc"))
                    || self.modifier(n, "final")
                    || (self.e.language == "csharp" && self.modifier(n, "sealed"))
                    || (self.e.language == "kotlin"
                        && !self.modifier(n, "open")
                        && ty != "interface");
                let child = self.define(
                    n,
                    scope,
                    &name,
                    ty,
                    q.clone(),
                    ty != "extension" && !self.uncertain(scope),
                );
                if ty != "extension" {
                    self.bind(scope, &name, Binding::Type(q.clone()));
                }
                let ctx = self.scopes.get_mut(&child).unwrap();
                ctx.class = Some((q, closed));
                ctx.local = false;
                ctx.abstract_members = ty == "interface";
                if kind == "companion_object" {
                    self.e.facts.nodes.last_mut().unwrap().metadata["companion"] = json!(true);
                }
                if ty == "extension" {
                    self.pending.push(Pending {
                        node: n,
                        scope: child,
                        label: name.clone(),
                        relation: "extends",
                        target: Some(name.clone()),
                        constructor: true,
                        context: Some("extension_type"),
                    });
                }
                self.inheritance(n, child);
                for c in children(n) {
                    if matches!(
                        c.kind(),
                        "class_body"
                            | "interface_body"
                            | "annotation_type_body"
                            | "enum_body"
                            | "enum_class_body"
                            | "field_declaration_list"
                            | "declaration_list"
                            | "protocol_body"
                            | "enum_member_declaration_list"
                            | "enumerator_list"
                    ) {
                        for d in children(c) {
                            self.visit(d, child);
                        }
                    } else if matches!(
                        c.kind(),
                        "primary_constructor"
                            | "class_parameters"
                            | "formal_parameters"
                            | "parameter_list"
                            | "type_parameters"
                            | "type_parameter_list"
                            | "template_parameter_list"
                            | "modifiers"
                    ) {
                        self.visit(c, child);
                    }
                }
                return;
            }
            // Anonymous types still own their fields; they must not shadow the enclosing scope.
            let anonymous = self.child(scope, n);
            for c in children(n) {
                self.visit(c, anonymous);
            }
            return;
        }
        if matches!(
            kind,
            "enum_constant" | "enum_member_declaration" | "enum_entry" | "enumerator"
        ) {
            self.enum_case(n, scope);
            return;
        }
        if self.e.language == "java" && matches!(kind, "annotation" | "marker_annotation") {
            if let Some(name) = n.child_by_field_name("name") {
                self.type_evidence(name, scope, "attribute");
            }
            let mut pending = children(n);
            while let Some(child) = pending.pop() {
                if child.kind() == "class_literal" {
                    for ty in children(child) {
                        self.type_evidence(ty, scope, "attribute");
                    }
                } else {
                    pending.extend(children(child));
                }
            }
            return;
        }
        if matches!(
            kind,
            "function_definition"
                | "function_declaration"
                | "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "local_function_statement"
                | "init_declaration"
                | "deinit_declaration"
                | "protocol_function_declaration"
                | "annotation_type_element_declaration"
        ) {
            self.function(n, scope);
            return;
        }
        if matches!(
            kind,
            "property_declaration" | "protocol_property_declaration" | "subscript_declaration"
        ) && self.property(n, scope)
        {
            return;
        }
        if matches!(
            kind,
            "accessor_declaration"
                | "computed_getter"
                | "computed_setter"
                | "computed_modify"
                | "willset_clause"
                | "didset_clause"
                | "getter"
                | "setter"
        ) {
            let name = n
                .child_by_field_name("name")
                .map(|n| self.e.text(n))
                .unwrap_or(kind)
                .to_owned();
            let q = format!("{}.{}", self.scopes[&scope].prefix, name);
            scope = self.define(n, scope, &name, "accessor", q, false);
            self.scopes.get_mut(&scope).unwrap().local = true;
        }
        if matches!(
            kind,
            "lambda_expression"
                | "lambda_literal"
                | "anonymous_method_expression"
                | "object_literal"
        ) {
            let q = format!("@{}:lambda@{}", self.e.facts.path, n.start_byte());
            scope = self.define(n, scope, "<lambda>", "function", q, false);
            self.scopes.get_mut(&scope).unwrap().local = true;
            // Capture lists and inferred parameters vary by grammar. Do not guess their bindings.
            self.scopes.get_mut(&scope).unwrap().uncertain = true;
        } else if matches!(
            kind,
            "block"
                | "compound_statement"
                | "statements"
                | "for_statement"
                | "enhanced_for_statement"
                | "for_in_statement"
                | "catch_clause"
                | "catch_block"
                | "foreach_statement"
        ) {
            scope = self.child(scope, n);
        }
        if matches!(
            kind,
            "type_alias" | "typealias_declaration" | "alias_declaration" | "type_definition"
        ) {
            let name = n
                .child_by_field_name("name")
                .or_else(|| n.child_by_field_name("declarator"))
                .and_then(|c| self.name(c));
            if let Some(name) = name {
                let q = format!("{}.{name}", self.scopes[&scope].prefix);
                let child = self.define(n, scope, &name, "type", q, true);
                let target = n
                    .child_by_field_name("value")
                    .or_else(|| n.child_by_field_name("type"))
                    .and_then(|c| self.name(c));
                let resolved = target.as_deref().and_then(|t| self.qualify(scope, t));
                self.bind(
                    scope,
                    &name,
                    resolved.map(Binding::Type).unwrap_or(Binding::Unknown),
                );
                self.pending.push(Pending {
                    node: n,
                    scope: child,
                    label: target.clone().unwrap_or_default(),
                    relation: "aliases",
                    target,
                    constructor: true,
                    context: None,
                });
                return;
            }
        }
        if kind == "pattern"
            && self.e.language == "swift"
            && let Some(name) = n
                .child_by_field_name("bound_identifier")
                .or_else(|| n.child_by_field_name("name"))
                .and_then(|n| self.name(n))
        {
            self.bind(scope, &name, Binding::Value(None));
        }
        if matches!(
            kind,
            "formal_parameter"
                | "parameter"
                | "parameter_declaration"
                | "optional_parameter_declaration"
                | "class_parameter"
                | "catch_formal_parameter"
                | "catch_declaration"
                | "variable_declarator"
                | "variable_declaration"
                | "property_declaration"
                | "type_parameter"
                | "type_parameter_declaration"
                | "enhanced_for_statement"
                | "foreach_statement"
                | "type_pattern"
                | "declaration_pattern"
                | "var_pattern"
                | "catch_block"
        ) {
            self.variable(n, scope);
        }
        if matches!(kind, "declaration" | "field_declaration")
            && matches!(self.e.language, "c" | "cpp")
        {
            for c in children(n) {
                if let Some(d) = c
                    .child_by_field_name("declarator")
                    .or(Some(c))
                    .and_then(declarator_name)
                    && (c.kind().contains("declarator")
                        || matches!(c.kind(), "identifier" | "field_identifier"))
                {
                    let name = self.e.text(d).to_owned();
                    if is_function_declarator(c) {
                        self.prototype(n, c, scope, &name);
                    } else {
                        self.variable_declarator(n, c, d, scope);
                    }
                }
            }
        }
        if matches!(
            kind,
            "assignment_expression" | "assignment" | "assignment_statement"
        ) && let Some(left) = n
            .child_by_field_name("left")
            .or_else(|| n.child_by_field_name("target"))
            .or_else(|| n.named_child(0))
            && let Some(name) = self.name(left)
        {
            let mut at = scope;
            loop {
                if self.scopes[&at].bindings.contains_key(&name) {
                    self.scopes
                        .get_mut(&at)
                        .unwrap()
                        .bindings
                        .insert(name.clone(), Binding::Unknown);
                    break;
                }
                let Some(parent) = self.e.scopes[at].parent else {
                    self.bind(scope, &name, Binding::Unknown);
                    break;
                };
                at = parent;
            }
        }
        if matches!(
            kind,
            "call_expression"
                | "invocation_expression"
                | "method_invocation"
                | "object_creation_expression"
                | "new_expression"
                | "constructor_invocation"
        ) {
            let constructor = matches!(
                kind,
                "object_creation_expression" | "new_expression" | "constructor_invocation"
            );
            let target_node = if constructor {
                n.child_by_field_name("type").or_else(|| n.named_child(0))
            } else {
                n.child_by_field_name("function")
                    .or_else(|| n.child_by_field_name("name"))
                    .or_else(|| n.named_child(0))
            };
            if let Some(target_node) = target_node {
                let mut label = self.e.text(target_node).to_owned();
                let mut target = self.name(target_node);
                if kind == "method_invocation"
                    && let Some(object) = n.child_by_field_name("object")
                {
                    label = format!("{}.{}", self.e.text(object), label);
                    target = self
                        .name(object)
                        .zip(target)
                        .map(|(a, b)| format!("{a}.{b}"));
                }
                self.pending.push(Pending {
                    node: n,
                    scope,
                    label,
                    relation: "calls",
                    target,
                    constructor,
                    context: None,
                });
            }
        }
        if matches!(
            kind,
            "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif"
        ) {
            // Both branches may be present without a compilation configuration.
            scope = self.child(scope, n);
            if !include_guard(n, self.e.source) {
                self.scopes.get_mut(&scope).unwrap().uncertain = true;
            }
        }
        if kind == "function_declarator"
            && n.parent()
                .is_some_and(|p| matches!(p.kind(), "declaration" | "field_declaration"))
        {
            return;
        }
        if matches!(kind, "preproc_def" | "preproc_function_def") {
            if let Some(name) = n.child_by_field_name("name") {
                let name = self.e.text(name).to_owned();
                self.bind(scope, &name, Binding::Unknown);
            }
            return;
        }
        for child in children(n) {
            self.visit(child, scope);
        }
        if self.e.language == "swift" {
            self.swift_callable_write(n, scope);
        }
    }
    fn modifier(&self, n: Syntax<'_>, word: &str) -> bool {
        children(n)
            .into_iter()
            .filter(|c| {
                matches!(
                    c.kind(),
                    "modifiers"
                        | "modifier"
                        | "storage_class_specifier"
                        | "inheritance_modifier"
                        | "virtual_function_specifier"
                )
            })
            .any(|c| self.e.text(c).split_whitespace().any(|s| s == word))
            || {
                let mut cursor = n.walk();
                n.children(&mut cursor).any(|c| c.kind() == word)
            }
    }
    fn function(&mut self, n: Syntax<'t>, scope: usize) {
        if let Some(test) = self.tests.iter().find(|test| test.start == n.start_byte()) {
            let label = test.label.clone();
            let macro_name = test.macro_name.clone();
            let q = format!("@{}:test@{}", self.e.facts.path, test.start);
            let child = self.define(n, scope, &label, "test", q, false);
            self.e.facts.nodes.last_mut().unwrap().metadata["test_macro"] = json!(macro_name);
            self.scopes.get_mut(&child).unwrap().local = true;
            if let Some(body) = n.child_by_field_name("body") {
                self.visit(body, child);
            }
            return;
        }
        let name_node = n.child_by_field_name("name").or_else(|| {
            n.child_by_field_name("declarator")
                .and_then(declarator_name)
        });
        let name = name_node
            .map(|c| {
                self.name(c)
                    .unwrap_or_else(|| self.e.text(c).trim().to_owned())
            })
            .or_else(|| match n.kind() {
                "init_declaration" => Some("init".into()),
                "deinit_declaration" => Some("deinit".into()),
                _ => None,
            });
        let Some(name) = name else {
            return;
        };
        let ctx = self.scopes[&scope].clone();
        let member = ctx.class.is_some() && !ctx.local;
        let static_member = member
            && (self.modifier(n, "static")
                || (self.e.language == "kotlin"
                    && ctx.class.as_ref().is_some_and(|(_, closed)| *closed)
                    && n.parent().is_some_and(|p| {
                        p.parent().is_some_and(|p| {
                            matches!(p.kind(), "object_declaration" | "companion_object")
                        })
                    })));
        let dynamic = member
            && !static_member
            && (ctx.abstract_members
                || self.e.facts.nodes[0].metadata["dialect"] == "cpp_cli"
                || self.modifier(n, "abstract")
                || self.modifier(n, "virtual")
                || self.modifier(n, "override")
                || self.modifier(n, "open")
                || self.modifier(n, "dynamic")
                || children(n)
                    .iter()
                    .any(|c| c.kind() == "attribute" && self.e.text(*c).contains("objc"))
                || (matches!(self.e.language, "java" | "swift")
                    && !ctx.class.as_ref().unwrap().1
                    && !self.modifier(n, "final")
                    && !self.modifier(n, "private")));
        let hidden = ctx.local
            || (!member && self.modifier(n, "static"))
            || self.modifier(n, "private")
            || self.modifier(n, "fileprivate");
        let qualified = if ctx.local {
            format!(
                "@{}:{}.{}",
                self.e.facts.path, self.e.scopes[scope].owner, name
            )
        } else if hidden {
            format!("@{}:{}.{}", self.e.facts.path, ctx.prefix, name)
        } else if name.contains('.') && self.e.language == "cpp" {
            if ctx.prefix.starts_with('@') {
                name.clone()
            } else {
                format!("{}.{name}", ctx.prefix)
            }
        } else {
            format!("{}.{name}", ctx.prefix)
        };
        let extension = self.e.language == "kotlin"
            && name_node.is_some_and(|name| {
                children(n).into_iter().any(|c| {
                    c.end_byte() <= name.start_byte()
                        && matches!(c.kind(), "user_type" | "nullable_type" | "receiver_type")
                })
            });
        let parameterless = self.parameterless(n);
        let uncertain = self.uncertain(scope) || extension || simple_name(&name).is_none();
        let swift_conditional = self.e.language == "swift" && self.swift_conditional_scope(scope);
        let child = self.define(
            n,
            scope,
            &name,
            if member { "method" } else { "function" },
            qualified.clone(),
            !dynamic && !uncertain,
        );
        if swift_conditional {
            self.swift_callables
                .conditional_owners
                .insert(self.e.scopes[child].owner.clone());
        }
        self.bind(
            scope,
            &name,
            if dynamic || uncertain {
                Binding::Unknown
            } else {
                Binding::Symbol(self.key(&qualified))
            },
        );
        let node = self.e.facts.nodes.last_mut().unwrap();
        node.metadata["header_definition"] =
            json!(matches!(self.e.language, "c" | "cpp") && !hidden && !dynamic && !uncertain);
        node.metadata["static"] = json!(static_member);
        node.metadata["extension"] = json!(extension);
        node.metadata["parameterless"] = json!(parameterless);
        node.metadata["declaration_certain"] = json!(!uncertain);
        node.metadata["dynamic_dispatch"] = json!(dynamic);
        if member && !dynamic && !uncertain && !hidden {
            node.metadata["binding_aliases"] = json!([format!(
                "{}:{}:{qualified}",
                self.e.language,
                if static_member { "static" } else { "member" }
            )]);
        }
        if self.e.language == "swift"
            && !member
            && !uncertain
            && !swift_conditional
            && !children(n).iter().any(|c| c.kind() == "type_parameters")
            && let Some(key) = node.binding_key.clone()
        {
            let index = self.e.facts.nodes.len() - 1;
            self.swift_callables
                .functions
                .entry(key)
                .and_modify(|node| *node = None)
                .or_insert(Some(index));
        }
        let child_ctx = self.scopes.get_mut(&child).unwrap();
        child_ctx.local = true;
        // Type names in a method are relative to the surrounding namespace, not the method.
        child_ctx.prefix = ctx.prefix;
        if let Some(ty) = n
            .child_by_field_name("return_type")
            .or_else(|| n.child_by_field_name("returns"))
            .or_else(|| n.child_by_field_name("type"))
        {
            self.type_evidence(ty, child, "return_type");
        } else if self.e.language == "kotlin" {
            for ty in children(n).into_iter().filter(|c| {
                is_type(c.kind()) && name_node.is_some_and(|name| c.start_byte() > name.end_byte())
            }) {
                self.type_evidence(ty, child, "return_type");
            }
        }
        for c in children(n) {
            self.visit(c, child);
        }
    }
    fn type_evidence(&mut self, n: Syntax<'t>, scope: usize, context: &'static str) {
        if matches!(
            n.kind(),
            "primitive_type"
                | "predefined_type"
                | "integral_type"
                | "floating_point_type"
                | "void_type"
        ) {
            return;
        }
        if let Some(name) = type_name(n, self.e.source) {
            self.pending.push(Pending {
                node: n,
                scope,
                label: name.clone(),
                relation: "references",
                target: Some(name),
                constructor: true,
                context: Some(context),
            });
            // Generic arguments carry their own evidence, never callable specialization keys.
            for c in children(n) {
                if matches!(
                    c.kind(),
                    "type_arguments" | "type_argument_list" | "template_argument_list"
                ) {
                    for arg in children(c) {
                        self.type_evidence(arg, scope, "generic_arg");
                    }
                }
            }
        } else {
            for c in children(n) {
                if is_type(c.kind())
                    || matches!(
                        c.kind(),
                        "type_projection"
                            | "identifier"
                            | "simple_identifier"
                            | "type_arguments"
                            | "type_argument_list"
                            | "template_argument_list"
                    )
                {
                    self.type_evidence(c, scope, context);
                }
            }
        }
    }
    fn property(&mut self, n: Syntax<'t>, scope: usize) -> bool {
        if self.scopes[&scope].local {
            return false;
        }
        let declaration = children(n)
            .into_iter()
            .find(|n| n.kind() == "variable_declaration")
            .unwrap_or(n);
        let name = declaration
            .child_by_field_name("name")
            .or_else(|| {
                children(declaration)
                    .into_iter()
                    .find(|n| matches!(n.kind(), "identifier" | "simple_identifier"))
            })
            .and_then(|n| self.name(n))
            .or_else(|| (n.kind() == "subscript_declaration").then(|| "subscript".into()));
        let Some(name) = name else {
            return false;
        };
        if let Some(ty) = declared_type(declaration).or_else(|| declared_type(n)) {
            self.type_evidence(ty, scope, "field");
        }
        let ty = declared_type(declaration)
            .or_else(|| declared_type(n))
            .and_then(|n| type_name(n, self.e.source))
            .and_then(|t| self.qualify(scope, &t));
        self.bind(scope, &name, Binding::Value(ty));
        let q = if self.modifier(n, "private") || self.modifier(n, "fileprivate") {
            format!(
                "@{}:{}.{}",
                self.e.facts.path, self.scopes[&scope].prefix, name
            )
        } else {
            format!("{}.{name}", self.scopes[&scope].prefix)
        };
        let child = self.define(n, scope, &name, "property", q.clone(), false);
        let key = format!("{}:property:{q}", self.e.language);
        let safe = !self.uncertain(scope);
        self.e.facts.nodes.last_mut().unwrap().binding_key = safe.then_some(key);
        self.scopes.get_mut(&child).unwrap().local = true;
        for c in children(n) {
            if c.id() != declaration.id() {
                self.visit(c, child);
            }
        }
        true
    }
    fn prototype(&mut self, n: Syntax<'t>, declarator: Syntax<'t>, scope: usize, name: &str) {
        let parameterless = !n
            .parent()
            .is_some_and(|p| p.kind() == "template_declaration")
            && self.parameterless(declarator);
        let q = format!("{}.{name}", self.scopes[&scope].prefix);
        let child = self.define(n, scope, name, "declaration", q.clone(), false);
        let symbol = q
            .strip_prefix(&format!("@{}.", self.e.facts.path))
            .unwrap_or(&q)
            .to_owned();
        let member = self.scopes[&scope].class.is_some();
        let static_member = member && self.modifier(n, "static");
        let dynamic = self.modifier(n, "virtual")
            || self.modifier(n, "override")
            || self.scopes[&scope].abstract_members
            || (member && !static_member && self.e.facts.nodes[0].metadata["dialect"] == "cpp_cli");
        let safe = !dynamic && !self.uncertain(scope);
        let node = self.e.facts.nodes.last_mut().unwrap();
        node.metadata["header_declaration"] = json!(symbol);
        node.metadata["static"] = json!(static_member);
        node.metadata["parameterless"] = json!(parameterless);
        node.metadata["dynamic_dispatch"] = json!(dynamic);
        if safe && is_header(&self.e.facts.path) {
            node.binding_key = Some(header_key("declaration", &self.e.facts.path, &symbol));
            let mut aliases = vec![header_key("symbol", &self.e.facts.path, &symbol)];
            if member {
                aliases.push(header_key(
                    if static_member { "static" } else { "member" },
                    &self.e.facts.path,
                    &symbol,
                ));
            }
            node.metadata["binding_aliases"] = json!(aliases);
        }
        self.e.reference(
            n,
            child,
            name.into(),
            "implemented_by",
            vec![header_key("implementation", &self.e.facts.path, &symbol)],
            "matching definition must include this exact header",
        );
        self.scopes.get_mut(&child).unwrap().local = true;
        if let Some(ty) = n.child_by_field_name("type") {
            self.type_evidence(ty, child, "return_type");
        }
        let mut pending = vec![declarator];
        while let Some(d) = pending.pop() {
            if let Some(params) = d.child_by_field_name("parameters") {
                self.visit(params, child);
            }
            if let Some(d) = d.child_by_field_name("declarator") {
                pending.push(d);
            }
        }
    }
    // Only a written empty parameter list, without method type parameters,
    // supplies this declaration-shape proof. It is never overload resolution.
    fn parameterless(&self, mut n: Syntax<'_>) -> bool {
        loop {
            if children(n)
                .iter()
                .any(|c| matches!(c.kind(), "type_parameters" | "type_parameter_list"))
                || n.parent()
                    .is_some_and(|p| p.kind() == "template_declaration")
            {
                return false;
            }
            if let Some(parameters) = n.child_by_field_name("parameters").or_else(|| {
                children(n)
                    .into_iter()
                    .find(|c| c.kind() == "function_value_parameters")
            }) {
                let mut cursor = parameters.walk();
                return parameters
                    .children(&mut cursor)
                    .all(|c| matches!(c.kind(), "(" | ")" | "comment"));
            }
            if self.e.language == "swift" {
                let mut cursor = n.walk();
                let tokens: Vec<_> = n
                    .children(&mut cursor)
                    .filter(|c| c.kind() != "comment")
                    .collect();
                return tokens
                    .windows(2)
                    .any(|pair| pair[0].kind() == "(" && pair[1].kind() == ")");
            }
            let Some(declarator) = n.child_by_field_name("declarator") else {
                return false;
            };
            n = declarator;
        }
    }
    fn uncertain(&self, mut scope: usize) -> bool {
        loop {
            if self.scopes[&scope].uncertain {
                return true;
            }
            let Some(p) = self.e.scopes[scope].parent else {
                return false;
            };
            scope = p;
        }
    }
    fn variable(&mut self, n: Syntax<'t>, scope: usize) {
        if n.kind() == "property_declaration" && self.e.language == "kotlin" {
            return;
        }
        let name = n
            .child_by_field_name("name")
            .or_else(|| n.child_by_field_name("left"))
            .or_else(|| {
                n.child_by_field_name("declarator")
                    .and_then(declarator_name)
            })
            .or_else(|| {
                children(n).into_iter().find(|c| {
                    matches!(
                        c.kind(),
                        "identifier" | "simple_identifier" | "type_identifier"
                    ) && Some(*c) != n.child_by_field_name("type")
                })
            });
        let Some(name) = name else {
            return;
        };
        if matches!(n.kind(), "type_parameter" | "type_parameter_declaration") {
            let name = self.e.text(name).to_owned();
            self.bind(scope, &name, Binding::TypeParameter);
            return;
        }
        self.variable_declarator(n, n, name, scope);
    }
    fn variable_declarator(
        &mut self,
        declaration: Syntax<'t>,
        declarator: Syntax<'t>,
        name: Syntax<'t>,
        scope: usize,
    ) {
        let Some(name) = self.name(name) else {
            return;
        };
        let ty_node =
            declared_type(declaration).or_else(|| declaration.parent().and_then(declared_type));
        let field = self.scopes[&scope].class.is_some() && !self.scopes[&scope].local;
        let context = if field {
            "field"
        } else if matches!(
            declaration.kind(),
            "formal_parameter"
                | "parameter"
                | "parameter_declaration"
                | "optional_parameter_declaration"
                | "class_parameter"
        ) {
            "parameter_type"
        } else {
            "variable_type"
        };
        if let Some(ty) = ty_node {
            self.type_evidence(ty, scope, context);
        }
        let ty = ty_node
            .and_then(|n| {
                // An existential names a protocol declaration, not an implementor.
                let n = if self.e.language == "swift" && n.kind() == "existential_type" {
                    n.named_child(0).filter(|n| n.kind() == "user_type")?
                } else {
                    n
                };
                self.name(n)
            })
            .filter(|s| !matches!(s.as_str(), "var" | "auto" | "dynamic" | "Any" | "AnyObject"));
        let ty = if contains_kind(declarator, "function_declarator") {
            None
        } else {
            ty.and_then(|t| self.qualify(scope, &t))
        };
        self.bind(scope, &name, Binding::Value(ty));
        if field {
            let q = format!("{}.{name}", self.scopes[&scope].prefix);
            self.define(declaration, scope, &name, "field", q.clone(), false);
            let key = format!("{}:field:{q}", self.e.language);
            let safe = !self.uncertain(scope);
            self.e.facts.nodes.last_mut().unwrap().binding_key = safe.then_some(key);
        }
    }
    fn inheritance(&mut self, n: Syntax<'t>, scope: usize) {
        for child in children(n) {
            if matches!(
                child.kind(),
                "superclass"
                    | "super_interfaces"
                    | "extends_interfaces"
                    | "base_list"
                    | "base_class_clause"
                    | "inheritance_specifier"
                    | "delegation_specifiers"
            ) {
                self.base_types(
                    child,
                    scope,
                    if child.kind() == "super_interfaces" {
                        "implements"
                    } else {
                        "inherits"
                    },
                );
            }
        }
    }
    fn base_types(&mut self, n: Syntax<'t>, scope: usize, relation: &'static str) {
        if self.e.language == "kotlin" && n.kind() == "explicit_delegation" {
            if let Some(ty) = children(n).into_iter().find(|c| is_type(c.kind())) {
                self.base_types(ty, scope, "implements");
                self.pending.push(Pending {
                    node: n,
                    scope,
                    label: self.e.text(n).into(),
                    relation: "delegates_to",
                    target: children(n).last().and_then(|n| self.name(*n)),
                    constructor: false,
                    context: Some("delegation_type"),
                });
            }
            return;
        }
        let relation = if self.e.language == "kotlin"
            && n.kind() == "delegation_specifier"
            && !children(n)
                .iter()
                .any(|c| c.kind() == "constructor_invocation")
            && !self.scopes[&scope].abstract_members
        {
            "implements"
        } else {
            relation
        };
        if matches!(
            n.kind(),
            "type_identifier"
                | "user_type"
                | "scoped_type_identifier"
                | "qualified_identifier"
                | "identifier"
                | "qualified_name"
                | "generic_name"
                | "template_type"
                | "generic_type"
        ) {
            let target = type_name(n, self.e.source);
            for c in children(n).into_iter().filter(|c| {
                matches!(
                    c.kind(),
                    "type_arguments" | "type_argument_list" | "template_argument_list"
                )
            }) {
                for arg in children(c) {
                    self.type_evidence(arg, scope, "generic_arg");
                }
            }
            self.pending.push(Pending {
                node: n,
                scope,
                label: self.e.text(n).into(),
                relation,
                target,
                constructor: true,
                context: None,
            });
        } else {
            for c in children(n) {
                if c.kind() != "argument_list" && c.kind() != "value_arguments" {
                    self.base_types(c, scope, relation);
                }
            }
        }
    }
    fn enum_case(&mut self, n: Syntax<'t>, scope: usize) {
        let mut cursor = n.walk();
        let mut names: Vec<_> = n.children_by_field_name("name", &mut cursor).collect();
        if names.is_empty() {
            names.extend(
                children(n)
                    .into_iter()
                    .find(|c| matches!(c.kind(), "identifier" | "simple_identifier")),
            );
        }
        for name in names {
            let Some(label) = self.name(name) else {
                continue;
            };
            let qualified = format!("{}.{label}", self.scopes[&scope].prefix);
            let owner = self.e.scopes[scope].owner.clone();
            let child = self.define(name, scope, &label, "enum_case", qualified, false);
            let id = self.e.scopes[child].owner.clone();
            self.e.facts.edges.push(crate::model::Edge {
                id: format!("case_of:{id}"),
                source: owner,
                target: id,
                relation: "case_of".into(),
                directed: true,
                file: Some(self.e.facts.path.clone()),
                line: Some(name.start_position().row as u32 + 1),
                confidence: "static".into(),
                metadata: json!({"syntax":"enum case"}),
            });
            for c in children(n) {
                if c.kind() == "enum_type_parameters" {
                    for ty in children(c).into_iter().filter(|c| is_type(c.kind())) {
                        self.type_evidence(ty, scope, "type");
                    }
                } else if matches!(c.kind(), "argument_list" | "value_arguments" | "class_body") {
                    self.visit(c, child);
                }
            }
        }
    }
    fn import(&mut self, n: Syntax<'t>, scope: usize) {
        let text = self.e.text(n);
        self.e.facts.nodes[0].metadata["imports"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "syntax": text, "kind": n.kind(), "scope": self.e.scopes[scope].owner,
                "start_byte": n.start_byte(), "end_byte": n.end_byte()
            }));
        if n.kind() == "preproc_include" {
            let path = n.child_by_field_name("path");
            let label = path.map(|p| self.e.text(p)).unwrap_or(text).to_owned();
            let keys = if label.starts_with('"') {
                let relative = relative_path(
                    self.e.facts.path.rsplit_once('/').map_or("", |(p, _)| p),
                    label.trim_matches('"'),
                );
                if let Some(path) = &relative
                    && !self.uncertain(scope)
                    && !self.includes.contains(path)
                {
                    self.includes.push(path.clone());
                }
                relative
                    .map(|p| {
                        vec![format!(
                            "{}:file:{p}",
                            language(&p).map_or(self.e.language, |(lang, _)| lang)
                        )]
                    })
                    .unwrap_or_default()
            } else {
                vec![]
            };
            self.e.reference(
                n,
                scope,
                label,
                "includes",
                keys,
                "include target is external or unavailable; preprocessing is not performed",
            );
            return;
        }
        let raw = text.trim().trim_end_matches(';').trim();
        let mut raw = raw.strip_prefix("global ").unwrap_or(raw);
        for prefix in ["import ", "using ", "namespace "] {
            if let Some(s) = raw.strip_prefix(prefix) {
                raw = s.trim();
                break;
            }
        }
        let static_import = raw.starts_with("static ");
        raw = raw.strip_prefix("static ").unwrap_or(raw);
        let (target, alias) = if let Some((alias, target)) = raw.split_once('=') {
            (target.trim(), Some(alias.trim()))
        } else if let Some((target, alias)) = raw.split_once(" as ") {
            (target.trim(), Some(alias.trim()))
        } else {
            (raw, None)
        };
        let target = simple_name(target);
        let mut keys = vec![];
        if let Some(target) = target {
            keys.push(self.key(&target));
            if self.e.language == "swift" && !target.contains('.') {
                self.bind(scope, &target, Binding::Module(target.clone()));
                self.e.facts.nodes[0].metadata["swift_imports"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(target));
            }
            let alias = alias.unwrap_or_else(|| target.rsplit('.').next().unwrap());
            let is_namespace_using =
                self.e.language == "csharp" && !static_import && !raw.contains('=');
            if !is_namespace_using
                && self.e.language != "swift"
                && !(self.e.language == "csharp" && static_import)
            {
                let binding = if static_import
                    || (self.e.language == "kotlin"
                        && target
                            .rsplit('.')
                            .next()
                            .is_some_and(|s| s.chars().next().is_some_and(char::is_lowercase)))
                {
                    Binding::Symbol(self.key(&target))
                } else {
                    Binding::Type(target.clone())
                };
                if let Some(alias) = simple_name(alias) {
                    self.bind(scope, &alias, binding);
                }
            }
            // Namespace/wildcard imports cannot be priority candidates: Store resolves
            // the first available key, which would hide collisions across imports.
        }
        self.e.reference(
            n,
            scope,
            raw.into(),
            "imports",
            keys,
            "import target is external, a namespace, or unavailable",
        );
    }
    fn resolve(&self, p: &Pending<'_>) -> Vec<String> {
        let Some(target) = &p.target else {
            return vec![];
        };
        if self.uncertain(p.scope) {
            return vec![];
        }
        if p.relation == "delegates_to" {
            return match self.lookup(p.scope, target) {
                Some(Binding::Value(Some(ty))) => vec![self.key(ty)],
                _ => vec![],
            };
        }
        if p.constructor {
            if matches!(
                self.lookup(p.scope, target.split('.').next().unwrap_or(target)),
                Some(Binding::TypeParameter)
            ) {
                return vec![];
            }
            let parent = if matches!(p.relation, "inherits" | "implements" | "extends") {
                self.e.scopes[p.scope].parent.unwrap_or(p.scope)
            } else {
                p.scope
            };
            return self
                .qualify(parent, target)
                .map(|q| vec![self.key(&q)])
                .unwrap_or_default();
        }
        let parts: Vec<_> = target.split('.').collect();
        let head = parts[0];
        if parts.len() == 1 {
            return match self.lookup(p.scope, head) {
                Some(Binding::Symbol(k)) => vec![k.clone()],
                Some(Binding::Type(q)) => vec![self.key(q)],
                Some(_) => vec![],
                None if matches!(self.e.language, "java" | "csharp" | "swift")
                    && self.scopes[&p.scope].class.is_some() =>
                {
                    vec![self.key(&format!(
                        "{}.{head}",
                        self.scopes[&p.scope].class.as_ref().unwrap().0
                    ))]
                }
                None => {
                    let mut keys =
                        vec![self.key(&format!("{}.{head}", self.scopes[&p.scope].prefix))];
                    if matches!(self.e.language, "c" | "cpp") && self.includes.len() == 1 {
                        keys.push(header_key("symbol", &self.includes[0], head));
                    }
                    keys
                }
            };
        }
        if matches!(head, "this" | "self") {
            if parts.len() == 3 {
                let mut scope = p.scope;
                loop {
                    if self.e.scopes[scope].class {
                        return match self.scopes[&scope].bindings.get(parts[1]) {
                            Some(Binding::Value(Some(q))) => {
                                vec![self.member_key("member", &format!("{q}.{}", parts[2]))]
                            }
                            _ => vec![],
                        };
                    }
                    let Some(parent) = self.e.scopes[scope].parent else {
                        return vec![];
                    };
                    scope = parent;
                }
            }
            if parts.len() != 2 {
                return vec![];
            }
            return self.scopes[&p.scope]
                .class
                .as_ref()
                .map(|(q, _)| vec![self.key(&format!("{q}.{}", parts[1]))])
                .unwrap_or_default();
        }
        if matches!(head, "base" | "super") && parts.len() == 2 {
            return self.scopes[&p.scope]
                .class
                .as_ref()
                .map(|(q, _)| vec![format!("{}:base:{q}.{}", self.e.language, parts[1])])
                .unwrap_or_default();
        }
        match self.lookup(p.scope, head) {
            Some(Binding::Value(Some(q))) if parts.len() == 2 => {
                vec![self.member_key("member", &format!("{q}.{}", parts[1]))]
            }
            Some(Binding::Type(q)) => {
                let qualified = format!("{q}.{}", parts[1..].join("."));
                if self.e.language == "cpp" {
                    vec![self.key(&qualified)]
                } else {
                    vec![format!("{}:static:{qualified}", self.e.language)]
                }
            }
            Some(Binding::Module(module)) => vec![self.member_key(
                if parts.len() == 2 { "symbol" } else { "static" },
                &format!("!{module}.{}", parts[1..].join(".")),
            )],
            Some(_) => vec![],
            None if p.label.contains("::") && self.e.language == "cpp" => {
                let mut keys = vec![self.key(target)];
                if self.includes.len() == 1 {
                    keys.push(header_key("static", &self.includes[0], target));
                }
                keys
            }
            None if target.contains('.')
                && matches!(
                    self.e.language,
                    "cpp" | "java" | "csharp" | "kotlin" | "swift"
                ) =>
            {
                // Qualified package/namespace syntax is explicit, unlike an unknown object.
                // A two-part unknown receiver could be a variable; require a type spelling.
                if parts.len() >= 3 || head.chars().next().is_some_and(char::is_uppercase) {
                    let q = if parts.len() == 2 {
                        format!("{}.{target}", self.scopes[&p.scope].namespace)
                    } else {
                        target.clone()
                    };
                    vec![if self.e.language == "cpp" {
                        self.key(&q)
                    } else {
                        format!("{}:static:{q}", self.e.language)
                    }]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }
    fn finish(&mut self) {
        self.swift_callable_escapes();
        if matches!(self.e.language, "c" | "cpp") {
            let own = format!("@{}.", self.e.facts.path);
            let mut links = vec![];
            for node in &mut self.e.facts.nodes {
                let Some(q) = node.metadata["qualified_symbol"].as_str() else {
                    continue;
                };
                let symbol = q.strip_prefix(&own).unwrap_or(q).to_owned();
                let mut aliases: Vec<String> = node.metadata["binding_aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                if is_header(&self.e.facts.path)
                    && node.binding_key.is_some()
                    && matches!(
                        node.kind.as_str(),
                        "class" | "struct" | "union" | "enum" | "type" | "function" | "method"
                    )
                {
                    aliases.push(header_key("symbol", &self.e.facts.path, &symbol));
                    if node.kind == "method" {
                        aliases.push(header_key(
                            if node.metadata["static"] == true {
                                "static"
                            } else {
                                "member"
                            },
                            &self.e.facts.path,
                            &symbol,
                        ));
                    }
                }
                if node.metadata["header_definition"] == true {
                    for header in &self.includes {
                        aliases.push(header_key("implementation", header, &symbol));
                        links.push(crate::model::Reference {
                            id: format!("declared_by:{}:{header}", node.id),
                            source: node.id.clone(),
                            label: symbol.clone(),
                            relation: "declared_by".into(),
                            file: self.e.facts.path.clone(),
                            line: node.line.unwrap_or(1),
                            candidate_keys: vec![header_key("declaration", header, &symbol)],
                            reason:
                                "definition must match a declaration in this exact quoted header"
                                    .into(),
                        });
                    }
                }
                if !aliases.is_empty() {
                    aliases.sort();
                    aliases.dedup();
                    node.metadata["binding_aliases"] = json!(aliases);
                }
            }
            self.e.facts.references.extend(links);
        }
        let pending = std::mem::take(&mut self.pending);
        let mut seen_types = HashSet::new();
        for p in pending {
            if p.context.is_some()
                && !seen_types.insert((
                    self.e.scopes[p.scope].owner.clone(),
                    p.node.start_byte(),
                    p.node.end_byte(),
                    p.context,
                ))
            {
                continue;
            }
            let callable = self
                .swift_callables
                .calls
                .get(&(p.scope, p.node.start_byte()))
                .filter(|_| p.relation == "calls");
            let proven = callable.and_then(|call| self.swift_callable_target(call));
            let keys = if callable.is_some() {
                proven
                    .map(|value| vec![value.key.clone()])
                    .unwrap_or_default()
            } else {
                self.resolve(&p)
            };
            let callable_evidence = callable.zip(proven).map(|(call, value)| {
                let local = &self.swift_callables.locals[&call.binding];
                json!({
                    "declaration_start_byte": local.declaration.0,
                    "declaration_end_byte": local.declaration.1,
                    "assignment_start_byte": call.assignment.0,
                    "assignment_end_byte": call.assignment.1,
                    "target_id": self.e.facts.nodes[value.node].id,
                })
            });
            let relation = if p.relation == "inherits"
                && matches!(self.e.language, "csharp" | "swift" | "kotlin")
                && !self.scopes[&p.scope].abstract_members
                && self
                    .e
                    .facts
                    .nodes
                    .iter()
                    .filter(|n| n.binding_key.as_ref().is_some_and(|key| keys.contains(key)))
                    .count()
                    == 1
                && self.e.facts.nodes.iter().any(|n| {
                    n.kind == "interface"
                        && n.binding_key.as_ref().is_some_and(|key| keys.contains(key))
                }) {
                "implements"
            } else {
                p.relation
            };
            let reason = p.context.map_or_else(|| "target is external, ambiguous, virtual, shadowed, or requires unsupported type/build information".to_owned(), |context| format!("{context}: type target is external, generic, shadowed, or ambiguous"));
            self.e.reference(
                p.node,
                p.scope,
                p.label.clone(),
                relation,
                keys.clone(),
                &reason,
            );
            if let Some(mut evidence) = callable_evidence {
                let owner = &self.e.scopes[p.scope].owner;
                evidence["reference_id"] = json!(self.e.facts.references.last().unwrap().id);
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| &n.id == owner) {
                    if !node.metadata["swift_callable_values"].is_array() {
                        node.metadata["swift_callable_values"] = json!([]);
                    }
                    node.metadata["swift_callable_values"]
                        .as_array_mut()
                        .unwrap()
                        .push(evidence);
                }
            }
            if let Some(context) = p.context {
                let owner = &self.e.scopes[p.scope].owner;
                let id = self.e.facts.references.last().unwrap().id.clone();
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| &n.id == owner) {
                    if !node.metadata["type_references"].is_array() {
                        node.metadata["type_references"] = json!([]);
                    }
                    node.metadata["type_references"].as_array_mut().unwrap().push(json!({"reference_id":id,"label":p.label,"context":context,"start_byte":p.node.start_byte(),"end_byte":p.node.end_byte(),"line":p.node.start_position().row+1,"candidate_keys":keys}));
                }
            }
        }
    }
}
fn swift_body_statement(n: Syntax<'_>) -> bool {
    n.parent().is_some_and(|p| {
        p.kind() == "statements" && p.parent().is_some_and(|p| p.kind() == "function_body")
    })
}
fn swift_assignment_name(n: Syntax<'_>) -> Option<Syntax<'_>> {
    let target = n.child_by_field_name("target")?;
    target.named_child(0).filter(|c| {
        target.kind() == "directly_assignable_expression"
            && target.named_child_count() == 1
            && c.kind() == "simple_identifier"
    })
}
fn declarator_name(mut n: Syntax<'_>) -> Option<Syntax<'_>> {
    loop {
        if matches!(
            n.kind(),
            "identifier"
                | "field_identifier"
                | "qualified_identifier"
                | "operator_name"
                | "destructor_name"
        ) {
            return Some(n);
        }
        n = n.child_by_field_name("declarator").or_else(|| {
            if n.kind() == "parenthesized_declarator" {
                n.named_child(0)
            } else {
                None
            }
        })?;
    }
}
fn contains_kind(n: Syntax<'_>, kind: &str) -> bool {
    n.kind() == kind || children(n).into_iter().any(|c| contains_kind(c, kind))
}
fn simple_name(text: &str) -> Option<String> {
    let text = text.trim().replace("::", ".");
    let text = text
        .strip_prefix("global.")
        .unwrap_or(&text)
        .trim_start_matches('.');
    if text.is_empty() {
        return None;
    }
    // Only identifier paths. Calls, subscripts, generic dispatch, optional chaining,
    // pointer dereferences, and operators never collapse to a bare method name.
    let parts = text
        .split('.')
        .map(|p| p.trim().trim_matches('`').trim_start_matches('@'))
        .collect::<Vec<_>>();
    if parts.iter().any(|p| {
        p.is_empty()
            || !p
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_alphabetic() || (i > 0 && c.is_numeric()))
    }) {
        return None;
    }
    Some(parts.join("."))
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
fn swift_module_key(kind: &str, module: &str, symbol: &str) -> String {
    format!("swift:{kind}:module:{}:{module}:{symbol}", module.len())
}
fn swift_export_key(kind: &str, module: &str, symbol: &str) -> String {
    format!("swift:{kind}:export:{}:{module}:{symbol}", module.len())
}
fn header_key(kind: &str, header: &str, symbol: &str) -> String {
    format!("c-cpp:header-{kind}:{header}:{symbol}")
}
fn is_header(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some("h" | "hh" | "hpp" | "hxx" | "H" | "cuh")
    )
}
fn cpp_header(source: &str) -> Result<bool> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_cpp::LANGUAGE.into())?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(false);
    };
    let mut pending = vec![(tree.root_node(), 0)];
    while let Some((node, depth)) = pending.pop() {
        if depth > 256 {
            return Ok(false);
        }
        if !node.has_error()
            && (matches!(
                node.kind(),
                "class_specifier"
                    | "namespace_definition"
                    | "template_declaration"
                    | "alias_declaration"
                    | "base_class_clause"
                    | "access_specifier"
                    | "qualified_identifier"
            ) || (node.kind() == "field_declaration"
                && children(node).into_iter().any(is_function_declarator)))
        {
            return Ok(true);
        }
        if !matches!(
            node.kind(),
            "comment" | "string_literal" | "raw_string_literal" | "preproc_arg"
        ) {
            pending.extend(children(node).into_iter().map(|c| (c, depth + 1)));
        }
    }
    Ok(false)
}
fn include_guard(n: Syntax<'_>, source: &str) -> bool {
    n.kind() == "preproc_ifdef"
        && source[n.byte_range()].trim_start().starts_with("#ifndef")
        && n.parent().is_some_and(|p| p.kind() == "translation_unit")
        && n.child_by_field_name("alternative").is_none()
        && n.child_by_field_name("name").is_some_and(|name| {
            children(n)
                .into_iter()
                .find(|c| c.kind() == "preproc_def")
                .and_then(|d| d.child_by_field_name("name"))
                .is_some_and(|defined| source[name.byte_range()] == source[defined.byte_range()])
        })
}
fn is_function_declarator(mut n: Syntax<'_>) -> bool {
    loop {
        if n.kind() == "function_declarator" {
            return n
                .child_by_field_name("declarator")
                .is_some_and(|d| d.kind() != "parenthesized_declarator");
        }
        let Some(inner) = n.child_by_field_name("declarator") else {
            return false;
        };
        n = inner;
    }
}
fn is_type(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "scoped_identifier"
            | "user_type"
            | "qualified_identifier"
            | "scoped_type_identifier"
            | "qualified_name"
            | "generic_name"
            | "generic_type"
            | "template_type"
            | "array_type"
            | "nullable_type"
            | "optional_type"
            | "pointer_type"
            | "ref_type"
            | "function_type"
            | "type_annotation"
            | "tuple_type"
            | "type"
            | "opaque_type"
            | "existential_type"
            | "struct_specifier"
            | "enum_specifier"
            | "union_specifier"
    )
}
fn declared_type(n: Syntax<'_>) -> Option<Syntax<'_>> {
    let ty = n
        .child_by_field_name("type")
        .or_else(|| children(n).into_iter().find(|c| is_type(c.kind())));
    ty.and_then(|t| {
        if t.kind() == "type_annotation" {
            t.child_by_field_name("name")
        } else {
            Some(t)
        }
    })
}
fn type_name(n: Syntax<'_>, source: &str) -> Option<String> {
    if !is_type(n.kind()) && !matches!(n.kind(), "identifier" | "simple_identifier") {
        return None;
    }
    if let Some(name) = simple_name(&source[n.byte_range()]) {
        return Some(name);
    }
    if matches!(
        n.kind(),
        "generic_type"
            | "generic_name"
            | "template_type"
            | "user_type"
            | "struct_specifier"
            | "enum_specifier"
            | "union_specifier"
    ) {
        if let Some(name) = n
            .child_by_field_name("name")
            .and_then(|n| simple_name(&source[n.byte_range()]))
        {
            return Some(name);
        }
        let names: Vec<_> = children(n)
            .into_iter()
            .filter(|n| {
                matches!(
                    n.kind(),
                    "identifier"
                        | "type_identifier"
                        | "scoped_type_identifier"
                        | "qualified_name"
                        | "qualified_identifier"
                )
            })
            .filter_map(|n| simple_name(&source[n.byte_range()]))
            .collect();
        if !names.is_empty() {
            return Some(names.join("."));
        }
    }
    None
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
fn dialect_tokens(source: &str) -> Vec<DialectToken<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() {
                if bytes[i] == b'\n' && !source[..i].trim_end_matches('\r').ends_with('\\') {
                    break;
                }
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i = source[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |n| i + 2 + n + 2);
            continue;
        }
        let mut opaque = false;
        if bytes[i] == b'#' && source[..i].rsplit('\n').next().unwrap().trim().is_empty() {
            opaque = true;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\n' && !source[..i].trim_end_matches('\r').ends_with('\\') {
                    break;
                }
                i += 1;
            }
        } else if let Some(prefix) = ["R\"", "u8R\"", "uR\"", "UR\"", "LR\""]
            .into_iter()
            .find(|prefix| source[i..].starts_with(prefix))
        {
            opaque = true;
            let delimiter_start = i + prefix.len();
            i = source[delimiter_start..]
                .find('(')
                .filter(|n| *n <= 16)
                .and_then(|n| {
                    let end = format!("){}\"", &source[delimiter_start..delimiter_start + n]);
                    source[delimiter_start + n + 1..]
                        .find(&end)
                        .map(|offset| delimiter_start + n + 1 + offset + end.len())
                })
                .unwrap_or(bytes.len());
        } else if matches!(bytes[i], b'\'' | b'"') {
            opaque = true;
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else {
                    let closed = bytes[i] == quote;
                    i += 1;
                    if closed {
                        break;
                    }
                }
            }
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 128 {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 128)
            {
                i += 1;
            }
        } else {
            i += if [b"::", b"[[", b"]]", b"^=", b"%="]
                .iter()
                .any(|p| bytes[i..].starts_with(*p))
            {
                2
            } else {
                1
            };
        }
        tokens.push(DialectToken {
            text: &source[start..i],
            start,
            end: i,
            opaque,
        });
    }
    tokens
}

fn matching_token(
    tokens: &[DialectToken<'_>],
    start: usize,
    open: &str,
    close: &str,
) -> Option<usize> {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(start) {
        if token.opaque {
            continue;
        }
        if token.text == open {
            depth += 1;
        }
        if depth > 256 {
            return None;
        }
        if token.text == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn normalize_dialect(source: &str, metal: bool) -> Option<DialectSource> {
    let tokens = dialect_tokens(source);
    let managed = !metal
        && (tokens.windows(2).any(|t| {
            !t[0].opaque
                && !t[1].opaque
                && matches!(t[0].text, "ref" | "value" | "interface")
                && matches!(t[1].text, "class" | "struct")
        }) || tokens.windows(2).any(|t| {
            !t[0].opaque && t[0].text == "gcnew" && !t[1].opaque && simple_name(t[1].text).is_some()
        }) || tokens.windows(3).any(|t| {
            t[0].text == "[" && matches!(t[1].text, "assembly" | "module") && t[2].text == ":"
        }));
    if !metal && !managed {
        return None;
    }
    let mut edits: Vec<(usize, usize, &'static str)> = vec![];
    if managed {
        for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.opaque) {
            if matches!(token.text, "ref" | "value" | "interface")
                && tokens
                    .get(i + 1)
                    .is_some_and(|t| matches!(t.text, "class" | "struct"))
            {
                edits.push((token.start, token.end, "managed_type"));
                if i > 0 && matches!(tokens[i - 1].text, "public" | "private") {
                    edits.push((tokens[i - 1].start, tokens[i - 1].end, "managed_visibility"));
                }
            } else if token.text == "gcnew"
                && tokens
                    .get(i + 1)
                    .is_some_and(|t| !t.opaque && simple_name(t.text).is_some())
            {
                edits.push((token.start, token.end, "managed_allocation"));
            } else if token.text == "["
                && tokens
                    .get(i + 1)
                    .is_some_and(|t| matches!(t.text, "assembly" | "module"))
                && tokens.get(i + 2).is_some_and(|t| t.text == ":")
                && let Some(end) = matching_token(&tokens, i, "[", "]")
            {
                edits.push((token.start, tokens[end].end, "managed_attribute"));
            }
        }
    }
    // Suffixes are only removed from declarations outside executable bodies.
    // This deliberately excludes ambiguous local `Name ^ value` expressions.
    let mut declaration_scope = vec![true];
    let mut start = 0;
    for (i, token) in tokens.iter().enumerate() {
        if token.opaque {
            if token.text.starts_with('#') {
                start = i + 1;
            }
            continue;
        }
        if token.text == "{" {
            let prefix = &tokens[start..i];
            let nested_type = !prefix.iter().any(|t| t.text == "=")
                && prefix
                    .iter()
                    .any(|t| matches!(t.text, "class" | "struct" | "namespace" | "union"));
            if *declaration_scope.last().unwrap() && !nested_type {
                normalize_declaration(prefix, metal, managed, &mut edits);
            }
            declaration_scope.push(nested_type);
            start = i + 1;
        } else if token.text == "}" {
            if declaration_scope.len() > 1 {
                declaration_scope.pop();
            }
            start = i + 1;
        } else if token.text == ";" {
            if *declaration_scope.last().unwrap() {
                normalize_declaration(&tokens[start..i], metal, managed, &mut edits);
            }
            start = i + 1;
        } else if token.text == ":"
            && i == start + 1
            && matches!(tokens[start].text, "public" | "private" | "protected")
        {
            start = i + 1;
        }
    }
    if edits.is_empty() {
        return None;
    }
    edits.sort_unstable();
    edits.dedup();
    let mut bytes = source.as_bytes().to_vec();
    let mut spans = vec![];
    for (start, end, kind) in edits {
        for byte in &mut bytes[start..end] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
        if kind == "managed_allocation" {
            bytes[start..start + 3].copy_from_slice(b"new");
        }
        spans.push(json!({"kind":kind,"start_byte":start,"end_byte":end}));
    }
    Some(DialectSource {
        source: String::from_utf8(bytes).expect("only complete tokens replaced"),
        dialect: if metal { "metal" } else { "cpp_cli" },
        spans,
    })
}

fn normalize_declaration(
    tokens: &[DialectToken<'_>],
    metal: bool,
    managed: bool,
    edits: &mut Vec<(usize, usize, &'static str)>,
) {
    if tokens.is_empty() {
        return;
    }
    let mut pending = vec![];
    let mut at = 0;
    let shader = metal && matches!(tokens[0].text, "kernel" | "vertex" | "fragment");
    if shader {
        pending.push((tokens[0].start, tokens[0].end, "metal_stage"));
        at += 1;
    }
    let Some(next) = declaration_type(tokens, at, managed, &mut pending) else {
        return;
    };
    at = next;
    // A declarator must follow the type. Expression operators and initializers
    // cannot be used as evidence for another type suffix.
    if tokens
        .get(at)
        .is_none_or(|t| t.opaque || simple_name(t.text).is_none())
    {
        return;
    }
    at += 1;
    if tokens.get(at).is_some_and(|t| t.text == "(") {
        let Some(end) = matching_token(tokens, at, "(", ")") else {
            return;
        };
        let mut first = at + 1;
        let mut depth = 0usize;
        for i in at + 1..=end {
            if matches!(tokens[i].text, "<" | "(" | "[[") {
                depth += 1;
            }
            if (i == end || tokens[i].text == ",") && depth == 0 {
                normalize_parameter(&tokens[first..i], metal, managed, &mut pending);
                first = i + 1;
            }
            if matches!(tokens[i].text, ">" | ")" | "]]") {
                depth = depth.saturating_sub(1);
            }
        }
    } else if shader
        || !tokens
            .get(at)
            .is_none_or(|t| matches!(t.text, "=" | "[" | ","))
    {
        return;
    }
    edits.extend(pending);
}

fn normalize_parameter(
    tokens: &[DialectToken<'_>],
    metal: bool,
    managed: bool,
    edits: &mut Vec<(usize, usize, &'static str)>,
) {
    let mut at = 0;
    if metal
        && tokens
            .first()
            .is_some_and(|t| matches!(t.text, "device" | "constant" | "thread" | "threadgroup"))
    {
        let t = tokens[0];
        edits.push((t.start, t.end, "metal_address_space"));
        at += 1;
    }
    if declaration_type(tokens, at, managed, edits).is_none() {
        return;
    }
    if metal {
        for (i, token) in tokens.iter().enumerate() {
            if token.text == "[["
                && let Some(end) = matching_token(tokens, i, "[[", "]]")
                && metal_attribute(&tokens[i + 1..end])
            {
                edits.push((token.start, tokens[end].end, "metal_attribute"));
            }
        }
    }
}

fn metal_attribute(tokens: &[DialectToken<'_>]) -> bool {
    match tokens {
        [name] => matches!(
            name.text,
            "thread_position_in_grid"
                | "thread_position_in_threadgroup"
                | "threadgroup_position_in_grid"
                | "threads_per_threadgroup"
                | "stage_in"
                | "position"
        ),
        [name, open, number, close] => {
            matches!(name.text, "buffer" | "texture" | "sampler" | "color")
                && open.text == "("
                && close.text == ")"
                && !number.text.is_empty()
                && number.text.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

fn declaration_type(
    tokens: &[DialectToken<'_>],
    mut at: usize,
    managed: bool,
    edits: &mut Vec<(usize, usize, &'static str)>,
) -> Option<usize> {
    while tokens.get(at).is_some_and(|t| {
        matches!(
            t.text,
            "static"
                | "inline"
                | "const"
                | "volatile"
                | "virtual"
                | "extern"
                | "constexpr"
                | "unsigned"
                | "signed"
                | "long"
                | "short"
        )
    }) {
        at += 1;
    }
    let name = tokens.get(at)?;
    if name.opaque
        || simple_name(name.text).is_none()
        || matches!(
            name.text,
            "return" | "throw" | "if" | "while" | "for" | "switch" | "delete" | "new"
        )
    {
        return None;
    }
    at += 1;
    loop {
        if tokens.get(at).is_some_and(|t| t.text == "::") {
            let name = tokens.get(at + 1)?;
            if name.opaque || simple_name(name.text).is_none() {
                return None;
            }
            at += 2;
        } else if tokens.get(at).is_some_and(|t| t.text == "<") {
            let end = matching_token(tokens, at, "<", ">")?;
            if managed {
                for i in at + 1..end {
                    if matches!(tokens[i].text, "^" | "%")
                        && matches!(tokens[i + 1].text, ">" | ",")
                    {
                        edits.push((tokens[i].start, tokens[i].end, "managed_type_suffix"));
                    }
                }
            }
            at = end + 1;
        } else {
            break;
        }
    }
    while let Some(token) = tokens.get(at) {
        if matches!(token.text, "*" | "&" | "const" | "volatile") {
            at += 1;
        } else if managed && matches!(token.text, "^" | "%") {
            edits.push((token.start, token.end, "managed_type_suffix"));
            at += 1;
        } else {
            break;
        }
    }
    Some(at)
}

struct StringTest {
    start: usize,
    label: String,
    macro_name: String,
}

// Repair only the omitted separator after an abstract interface signature. A
// same-line space becomes ';'; original byte offsets and all line breaks survive.
fn compact_kotlin(source: &str) -> Option<DialectSource> {
    let tokens = dialect_tokens(source);
    let mut stack = vec![false];
    let mut start = 0;
    let mut bytes = source.as_bytes().to_vec();
    let mut spans = vec![];
    for (i, t) in tokens.iter().enumerate().filter(|(_, t)| !t.opaque) {
        if t.text == "{" {
            stack.push(
                tokens[start..i]
                    .iter()
                    .any(|t| !t.opaque && t.text == "interface"),
            );
            start = i + 1;
        } else if t.text == "}" {
            if *stack.last().unwrap()
                && let Some(fun) = (start..i).find(|j| tokens[*j].text == "fun")
                && !tokens[fun..i]
                    .iter()
                    .any(|t| matches!(t.text, "=" | ";" | "{"))
                && let Some(open) = (fun + 1..i).find(|j| tokens[*j].text == "(")
                && matching_token(&tokens, open, "(", ")").is_some_and(|end| end < i)
                && let Some(previous) = i.checked_sub(1).map(|j| tokens[j])
                && !source[previous.end..t.start].contains(['\n', '\r'])
                && let Some(offset) = source.as_bytes()[previous.end..t.start]
                    .iter()
                    .rposition(|b| matches!(b, b' ' | b'\t'))
            {
                let at = previous.end + offset;
                bytes[at] = b';';
                spans.push(
                    json!({"kind":"kotlin_abstract_separator","start_byte":at,"end_byte":at+1}),
                );
            }
            if stack.len() > 1 {
                stack.pop();
            }
            start = i + 1;
        } else if t.text == ";" {
            start = i + 1;
        }
    }
    (!spans.is_empty()).then(|| DialectSource {
        source: String::from_utf8(bytes).unwrap(),
        dialect: "kotlin",
        spans,
    })
}

fn recover_string_tests(source: &str, normalized: &mut Option<DialectSource>) -> Vec<StringTest> {
    let tokens = dialect_tokens(source);
    let mut tests = vec![];
    let mut edits = vec![];
    let mut bodies = vec![];
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate() {
        if t.opaque {
            continue;
        }
        if t.text == "{" {
            depth += 1;
        }
        if t.text == "}" {
            depth = depth.saturating_sub(1);
        }
        let top = depth == 0 && matches!(t.text, "TEST_CASE" | "TEST_CASE_TEMPLATE" | "SCENARIO");
        let subcase = t.text == "SUBCASE"
            && bodies
                .iter()
                .any(|(start, end)| t.start > *start && t.end < *end);
        if !(top || subcase) || tokens.get(i + 1).is_none_or(|t| t.text != "(") {
            continue;
        }
        let Some(label) = tokens
            .get(i + 2)
            .filter(|t| t.opaque && t.text.starts_with('"') && t.text.ends_with('"'))
        else {
            continue;
        };
        let Some(close) = matching_token(&tokens, i + 1, "(", ")") else {
            continue;
        };
        if tokens.get(close + 1).is_none_or(|t| t.text != "{") {
            continue;
        }
        let Some(end) = matching_token(&tokens, close + 1, "{", "}") else {
            continue;
        };
        edits.push((
            t.start,
            tokens[close].end,
            if top { "void t()" } else { "if(1)" },
        ));
        if top {
            tests.push(StringTest {
                start: t.start,
                label: label.text.into(),
                macro_name: t.text.into(),
            });
            bodies.push((tokens[close + 1].start, tokens[end].end));
        }
    }
    if edits.is_empty() {
        return tests;
    }
    let parsed = normalized.get_or_insert_with(|| DialectSource {
        source: source.into(),
        dialect: "cpp",
        spans: vec![],
    });
    let mut bytes = parsed.source.as_bytes().to_vec();
    for (start, end, replacement) in edits {
        for b in &mut bytes[start..end] {
            if !matches!(*b, b'\n' | b'\r') {
                *b = b' ';
            }
        }
        bytes[start..start + replacement.len()].copy_from_slice(replacement.as_bytes());
        parsed
            .spans
            .push(json!({"kind":"cpp_string_test","start_byte":start,"end_byte":end}));
    }
    parsed.source = String::from_utf8(bytes).unwrap();
    tests
}

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
impl CompiledInventory {
    fn unit(&self, node: &crate::model::Node) -> String {
        if let Some(module) = node.metadata["swift_module"].as_str() {
            return format!("swift:{module}");
        }
        self.units
            .get(&node.file)
            .cloned()
            .unwrap_or_else(|| format!("file:{}", node.file))
    }
    fn canonical(&self, from: &crate::model::Node, key: &str) -> Option<String> {
        let normalized = context_key(&from.file, key);
        let group = self
            .types
            .get(&(self.unit(from), normalized.clone()))
            .or_else(|| {
                // Export keys already encode exact Swift module identity and visibility.
                normalized.starts_with("swift:").then_some(())?;
                if !normalized.contains(":export:") {
                    return None;
                }
                self.types.get(&(String::new(), normalized))
            })?;
        if group.len() == 1
            || group
                .iter()
                .all(|id| self.nodes[id].metadata["partial"] == true)
        {
            group.first().cloned()
        } else {
            None
        }
    }
    fn group(&self, id: &str) -> Vec<String> {
        let node = &self.nodes[id];
        let Some(key) = node.binding_key.as_ref() else {
            return vec![id.into()];
        };
        self.types
            .get(&(self.unit(node), context_key(&node.file, key)))
            .cloned()
            .unwrap_or_else(|| vec![id.into()])
    }
    fn member(
        &self,
        id: &str,
        name: &str,
        static_call: bool,
        seen: &mut HashSet<String>,
    ) -> Option<String> {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return None;
        }
        let group = self.group(id);
        let own: Vec<_> = self
            .nodes
            .values()
            .filter(|n| {
                n.label == name && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
            })
            .collect();
        if !own.is_empty() {
            return (own.len() == 1
                && matches!(own[0].kind.as_str(), "method" | "function" | "declaration")
                && (!static_call || own[0].metadata["static"] == true)
                && own[0].metadata["member_accessible"] == true
                && own[0].metadata["declaration_certain"] == true
                && self.inherited_name_absent(id, name, &mut HashSet::new()))
            .then(|| own[0].id.clone());
        }
        if static_call {
            let companions: Vec<_> = self
                .nodes
                .values()
                .filter(|n| {
                    n.metadata["companion"] == true
                        && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
                })
                .collect();
            if companions.len() == 1 {
                return self.member(&companions[0].id, name, true, seen);
            }
        }
        let mut found = HashSet::new();
        if !self.hierarchy_known(id, &mut HashSet::new()) {
            return None;
        }
        for part in group {
            let bases = self.bases.get(&part)?.as_ref()?;
            if bases.len() > 1 {
                return None;
            }
            for base in bases {
                if let Some(member) = self.member(base, name, static_call, &mut seen.clone()) {
                    found.insert(member);
                } else if self.bases.get(base).is_some_and(Option::is_none) {
                    return None;
                }
            }
        }
        (found.len() == 1).then(|| found.into_iter().next().unwrap())
    }
    // A nearest declaration does not prove that an ancestor's overload is
    // inapplicable. Without signature resolution, repeated inherited names
    // remain ambiguous. Implemented interface contracts are not class overloads.
    fn inherited_name_absent(&self, id: &str, name: &str, seen: &mut HashSet<String>) -> bool {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return false;
        }
        self.group(id).iter().all(|part| {
            self.bases
                .get(part)
                .and_then(Option::as_ref)
                .is_some_and(|bases| {
                    bases.iter().all(|base| {
                        if self.nodes[id].kind != "interface"
                            && self.nodes[base].kind == "interface"
                        {
                            return true;
                        }
                        let group = self.group(base);
                        !self.nodes.values().any(|n| {
                            n.label == name
                                && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
                        }) && self.inherited_name_absent(base, name, &mut seen.clone())
                    })
                })
        })
    }
    // Declaration navigation can retain separate known branches even when a
    // call cannot select one. An overload or uncertain branch poisons this
    // lookup; repeated names only shadow proven parameterless declarations.
    fn declared_members(
        &self,
        id: &str,
        name: &str,
        static_call: bool,
        seen: &mut HashSet<String>,
    ) -> Option<Vec<String>> {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return None;
        }
        let group = self.group(id);
        let mut inherited = vec![];
        for part in &group {
            for base in self.bases.get(part)?.as_ref()? {
                inherited.extend(self.declared_members(
                    base,
                    name,
                    static_call,
                    &mut seen.clone(),
                )?);
            }
        }
        inherited.sort();
        inherited.dedup();
        let own: Vec<_> = self
            .nodes
            .values()
            .filter(|n| {
                n.label == name && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
            })
            .collect();
        if own.is_empty() {
            return Some(inherited);
        }
        if own.len() != 1
            || !matches!(own[0].kind.as_str(), "method" | "function" | "declaration")
            || (static_call && own[0].metadata["static"] != true)
            || own[0].metadata["member_accessible"] != true
            || own[0].metadata["declaration_certain"] != true
        {
            return None;
        }
        if !inherited.is_empty()
            && (own[0].metadata["parameterless"] != true
                || inherited.iter().any(|id| {
                    self.nodes[id].metadata["parameterless"] != true
                        || self.nodes[id].metadata["static"] != own[0].metadata["static"]
                }))
        {
            return None;
        }
        Some(vec![own[0].id.clone()])
    }
    fn hierarchy_known(&self, id: &str, seen: &mut HashSet<String>) -> bool {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return false;
        }
        self.group(id).iter().all(|part| {
            self.bases
                .get(part)
                .and_then(Option::as_ref)
                .is_some_and(|bases| {
                    bases
                        .iter()
                        .all(|base| self.hierarchy_known(base, &mut seen.clone()))
                })
        })
    }
    fn navigation_key(&self, id: &str) -> String {
        let n = &self.nodes[id];
        let unit = self.unit(n);
        let owner = self.parents.get(id).and_then(|p| self.nodes.get(p));
        let identity = if matches!(n.kind.as_str(), "method" | "function" | "declaration") {
            owner
                .and_then(|n| n.binding_key.clone())
                .map(|key| format!("{}.{}", context_key(&n.file, &key), n.label))
        } else {
            n.binding_key.as_ref().map(|key| context_key(&n.file, key))
        };
        format!(
            "compiled:declaration:{}:{unit}:{}",
            unit.len(),
            identity.unwrap_or_else(|| format!(
                "{}:{}",
                n.file,
                n.qualified_name.as_deref().unwrap_or(&n.label)
            ))
        )
    }
}
fn context_key(path: &str, key: &str) -> String {
    // The parent supplies the assembly boundary before unnamespaced C# names
    // can participate. Without it the inventory's unit remains file-local.
    key.strip_prefix(&format!("csharp:symbol:@{path}."))
        .map_or_else(
            || key.to_owned(),
            |symbol| format!("csharp:symbol:{symbol}"),
        )
}
fn type_node(n: &crate::model::Node) -> bool {
    matches!(
        n.kind.as_str(),
        "class" | "struct" | "union" | "enum" | "interface" | "type"
    )
}
fn node_keys(n: &crate::model::Node) -> Vec<String> {
    n.binding_key
        .iter()
        .cloned()
        .chain(
            n.metadata["binding_aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned)),
        )
        .collect()
}
impl CompiledContext {
    pub fn new(files: &[FileFacts], units: &BTreeMap<String, String>) -> Self {
        let mut result = Self::default();
        let mut inventory = CompiledInventory {
            nodes: BTreeMap::new(),
            units: units.clone(),
            parents: BTreeMap::new(),
            types: BTreeMap::new(),
            bases: BTreeMap::new(),
        };
        let files: Vec<_> = files
            .iter()
            .filter(|f| {
                f.nodes.first().is_some_and(|n| {
                    matches!(
                        n.metadata["language"].as_str(),
                        Some("cpp" | "java" | "csharp" | "kotlin" | "swift")
                    )
                })
            })
            .collect();
        for f in &files {
            for n in &f.nodes {
                inventory.nodes.insert(n.id.clone(), n.clone());
            }
            for edge in &f.edges {
                if edge.relation == "contains" {
                    inventory
                        .parents
                        .insert(edge.target.clone(), edge.source.clone());
                }
            }
        }
        for n in inventory.nodes.values().filter(|n| type_node(n)) {
            inventory.bases.insert(n.id.clone(), Some(vec![]));
            for key in node_keys(n) {
                let key = context_key(&n.file, &key);
                inventory
                    .types
                    .entry((inventory.unit(n), key.clone()))
                    .or_default()
                    .push(n.id.clone());
                if key.starts_with("swift:") && key.contains(":export:") {
                    inventory
                        .types
                        .entry((String::new(), key))
                        .or_default()
                        .push(n.id.clone());
                }
            }
        }
        for group in inventory.types.values_mut() {
            group.sort();
            group.dedup();
        }
        // Publish declaration identities independently of current call sites.
        // A new caller must not require rewriting an unchanged provider file.
        for node in inventory.nodes.values() {
            let canonical_type = type_node(node)
                && node
                    .binding_key
                    .as_ref()
                    .and_then(|key| inventory.canonical(node, key))
                    .as_deref()
                    == Some(node.id.as_str());
            if canonical_type
                || (matches!(node.kind.as_str(), "method" | "declaration")
                    && node.metadata["member_accessible"] == true
                    && node.metadata["declaration_certain"] == true)
            {
                result
                    .aliases
                    .entry(node.id.clone())
                    .or_default()
                    .push(inventory.navigation_key(&node.id));
            }
        }
        let mut implementers: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        for f in &files {
            for r in &f.references {
                if !matches!(r.relation.as_str(), "inherits" | "implements") {
                    continue;
                }
                let Some(owner) = inventory.nodes.get(&r.source) else {
                    continue;
                };
                let target = r
                    .candidate_keys
                    .iter()
                    .find_map(|k| inventory.canonical(owner, k));
                if let Some(target) = target {
                    if let Some(Some(bases)) = inventory.bases.get_mut(&r.source) {
                        bases.push(target.clone());
                    }
                    let interface =
                        inventory.nodes[&target].kind == "interface" && owner.kind != "interface";
                    if interface {
                        result.relations.insert(r.id.clone(), "implements".into());
                        implementers
                            .entry(target)
                            .or_default()
                            .insert(r.source.clone());
                    }
                } else {
                    inventory.bases.insert(r.source.clone(), None);
                }
            }
        }
        for group in inventory.types.values() {
            if group.len() > 1
                && group
                    .iter()
                    .all(|id| inventory.nodes[id].metadata["partial"] == true)
            {
                let primary = &group[0];
                for part in &group[1..] {
                    result.link(
                        &inventory,
                        part,
                        primary,
                        "partial_of",
                        "explicit partial declarations in one proven compilation unit",
                    );
                }
            }
        }
        // An extension's access defaults are useful only after its target type
        // and module have been proved, not from the extended spelling alone.
        for f in &files {
            for r in f.references.iter().filter(|r| r.relation == "extends") {
                let Some(extension) = inventory
                    .nodes
                    .get(&r.source)
                    .filter(|n| n.kind == "extension")
                else {
                    continue;
                };
                let Some(target) = r
                    .candidate_keys
                    .iter()
                    .find_map(|k| inventory.canonical(extension, k))
                else {
                    continue;
                };
                let target = &inventory.nodes[&target];
                if target.metadata["swift_exported"] != true
                    || extension.metadata["swift_module"].as_str().is_none()
                    || extension.metadata["swift_module"] != target.metadata["swift_module"]
                {
                    continue;
                }
                for member in inventory
                    .nodes
                    .values()
                    .filter(|n| inventory.parents.get(&n.id) == Some(&extension.id))
                {
                    if member.metadata["dynamic_dispatch"] != false
                        || member.metadata["member_accessible"] != true
                        || member.metadata["declaration_certain"] != true
                        || !(member.metadata["explicit_public"] == true
                            || (member.metadata["explicit_access"].is_null()
                                && extension.metadata["explicit_public"] == true))
                    {
                        continue;
                    }
                    for key in node_keys(member) {
                        if let Some((kind, symbol)) =
                            key.strip_prefix("swift:").and_then(|k| k.split_once(':'))
                            && let Some(symbol) = symbol.strip_prefix(&format!(
                                "module:{}:{}:",
                                member.metadata["swift_module"].as_str().unwrap().len(),
                                member.metadata["swift_module"].as_str().unwrap()
                            ))
                        {
                            result.aliases.entry(member.id.clone()).or_default().push(
                                swift_export_key(
                                    kind,
                                    member.metadata["swift_module"].as_str().unwrap(),
                                    symbol,
                                ),
                            );
                        }
                    }
                }
            }
        }
        for f in &files {
            for r in &f.references {
                let Some(source) = inventory.nodes.get(&r.source) else {
                    continue;
                };
                if r.relation == "calls" {
                    for key in &r.candidate_keys {
                        if let Some(target) = inventory.canonical(source, key)
                            && inventory.group(&target).len() > 1
                        {
                            let candidate = inventory.navigation_key(&target);
                            result
                                .aliases
                                .entry(target)
                                .or_default()
                                .push(candidate.clone());
                            result.candidates.insert(r.id.clone(), vec![candidate]);
                            break;
                        }
                        let key = context_key(&f.path, key);
                        let Some((owner, name)) = key.rsplit_once('.') else {
                            continue;
                        };
                        let static_call = owner.contains(":static:");
                        let base_call = owner.contains(":base:");
                        let type_key = owner
                            .replacen(":member:", ":symbol:", 1)
                            .replacen(":base:", ":symbol:", 1)
                            .replacen(":static:", ":symbol:", 1);
                        let kotlin_static = static_call && key.starts_with("kotlin:");
                        let cpp_qualified = r.label.contains("::")
                            && matches!(
                                f.nodes
                                    .first()
                                    .and_then(|n| n.metadata["language"].as_str()),
                                Some("cpp")
                            );
                        let type_key = if cpp_qualified {
                            type_key.replacen(":header-static:", ":header-symbol:", 1)
                        } else {
                            type_key
                        };
                        let Some(owner) = inventory.canonical(source, &type_key) else {
                            // A duplicate receiver must not fall through to a unique
                            // method key (one duplicate may lack that method).
                            if (kotlin_static || cpp_qualified)
                                && inventory
                                    .types
                                    .contains_key(&(inventory.unit(source), type_key))
                            {
                                result.candidates.insert(r.id.clone(), vec![]);
                                break;
                            }
                            continue;
                        };
                        // Keep qualified-only C++ definitions usable when the header
                        // supplied no member declaration. A known declaration, however,
                        // must enforce its access and dispatch evidence before fallback.
                        if kotlin_static
                            || (cpp_qualified
                                && inventory.nodes.values().any(|n| {
                                    n.label == name
                                        && inventory
                                            .parents
                                            .get(&n.id)
                                            .is_some_and(|p| inventory.group(&owner).contains(p))
                                }))
                        {
                            result.candidates.insert(r.id.clone(), vec![]);
                        }
                        let target = if base_call {
                            inventory
                                .bases
                                .get(&owner)
                                .and_then(Option::as_ref)
                                .filter(|bases| bases.len() == 1)
                                .and_then(|bases| {
                                    inventory.member(&bases[0], name, false, &mut HashSet::new())
                                })
                        } else {
                            inventory.member(&owner, name, static_call, &mut HashSet::new())
                        };
                        let Some(target) = target else {
                            // Keep declaration evidence separate from callable candidates.
                            // base/super calls retain their existing single-base boundary.
                            let declaration_owner = if base_call {
                                inventory
                                    .bases
                                    .get(&owner)
                                    .and_then(Option::as_ref)
                                    .filter(|bases| bases.len() == 1)
                                    .map(|bases| bases[0].as_str())
                            } else {
                                Some(owner.as_str())
                            };
                            if let Some(targets) = declaration_owner.and_then(|owner| {
                                inventory.declared_members(
                                    owner,
                                    name,
                                    static_call,
                                    &mut HashSet::new(),
                                )
                            }) {
                                for target in targets {
                                    let node = &inventory.nodes[&target];
                                    if node.metadata["swift_module"].is_string()
                                        && node.metadata["swift_module"]
                                            != source.metadata["swift_module"]
                                        && node.metadata["swift_exported"] != true
                                    {
                                        continue;
                                    }
                                    result.link_reference(
                                        &inventory,
                                        r,
                                        &target,
                                        "declared_member",
                                        "accessible declaration on a proven receiver-type branch; no overload selection or runtime dispatch claim",
                                    );
                                }
                            }
                            continue;
                        };
                        let node = &inventory.nodes[&target];
                        if node.metadata["swift_module"].is_string()
                            && node.metadata["swift_module"] != source.metadata["swift_module"]
                            && node.metadata["swift_exported"] != true
                        {
                            continue;
                        }
                        result.link_reference(&inventory,r,&target,"declared_member","written receiver type identifies this declaration; runtime dispatch is unresolved when virtual");
                        if node.metadata["dynamic_dispatch"] == false && node.binding_key.is_some()
                        {
                            let candidate = inventory.navigation_key(&target);
                            result
                                .aliases
                                .entry(target.clone())
                                .or_default()
                                .push(candidate.clone());
                            result.candidates.insert(r.id.clone(), vec![candidate]);
                        }
                        break;
                    }
                } else if !matches!(r.relation.as_str(), "declared_member" | "partial_of") {
                    for key in &r.candidate_keys {
                        if let Some(target) = inventory.canonical(source, key)
                            && inventory.group(&target).len() > 1
                        {
                            let candidate = inventory.navigation_key(&target);
                            result
                                .aliases
                                .entry(target)
                                .or_default()
                                .push(candidate.clone());
                            result.candidates.insert(r.id.clone(), vec![candidate]);
                            break;
                        }
                    }
                }
            }
        }
        for (interface, implementers) in implementers {
            if !inventory.nodes[&interface].id.starts_with("csharp:") || implementers.len() != 1 {
                continue;
            }
            let implementer = implementers.into_iter().next().unwrap();
            for method in inventory
                .nodes
                .values()
                .filter(|n| inventory.parents.get(&n.id) == Some(&interface) && n.kind == "method")
            {
                if let Some(target) =
                    inventory.member(&implementer, &method.label, false, &mut HashSet::new())
                    && inventory.parents.get(&target) != Some(&interface)
                {
                    result.link(&inventory,&method.id,&target,"implemented_by","one declared implementation in this compilation unit; not a runtime dispatch guarantee");
                }
            }
        }
        let mut proof = vec![];
        for f in &files {
            let mut nodes:Vec<_>=f.nodes.iter().map(|n|json!({"kind":n.kind,"name":n.qualified_name,"keys":node_keys(n),"parent":inventory.parents.get(&n.id).and_then(|id|inventory.nodes.get(id)).and_then(|n|n.qualified_name.as_ref()),"partial":n.metadata["partial"],"public":n.metadata["cross_project_public"],"explicit_public":n.metadata["explicit_public"],"explicit_access":n.metadata["explicit_access"],"certain":n.metadata["declaration_certain"],"parameterless":n.metadata["parameterless"],"accessible":n.metadata["member_accessible"],"dynamic":n.metadata["dynamic_dispatch"],"static":n.metadata["static"],"companion":n.metadata["companion"],"swift_module":n.metadata["swift_module"],"swift_exported":n.metadata["swift_exported"]})).collect();
            nodes.sort_by_key(serde_json::Value::to_string);
            let mut relations:Vec<_>=f.references.iter().filter(|r|matches!(r.relation.as_str(),"inherits"|"implements"|"extends"|"delegates_to")).map(|r|json!({"owner":inventory.nodes.get(&r.source).and_then(|n|n.qualified_name.as_ref()),"relation":r.relation,"keys":r.candidate_keys})).collect();
            relations.sort_by_key(serde_json::Value::to_string);
            proof.push(json!({"file":f.path,"unit":units.get(&f.path),"nodes":nodes,"relations":relations}));
        }
        proof.sort_by_key(serde_json::Value::to_string);
        result.fingerprint = blake3::hash(serde_json::to_string(&proof).unwrap().as_bytes())
            .to_hex()
            .to_string();
        for aliases in result.aliases.values_mut() {
            aliases.sort();
            aliases.dedup();
        }
        for refs in result.references.values_mut() {
            refs.sort_by(|a, b| a.id.cmp(&b.id));
            refs.dedup_by(|a, b| a.id == b.id);
        }
        result
    }
    fn link(
        &mut self,
        inventory: &CompiledInventory,
        source: &str,
        target: &str,
        relation: &str,
        reason: &str,
    ) {
        let node = &inventory.nodes[source];
        let reference = crate::model::Reference {
            id: format!("compiled:{relation}:{source}:{target}"),
            source: source.into(),
            label: inventory.nodes[target].label.clone(),
            relation: relation.into(),
            file: node.file.clone(),
            line: node.line.unwrap_or(1),
            candidate_keys: vec![],
            reason: reason.into(),
        };
        self.link_reference(inventory, &reference, target, relation, reason);
    }
    fn link_reference(
        &mut self,
        inventory: &CompiledInventory,
        original: &crate::model::Reference,
        target: &str,
        relation: &str,
        reason: &str,
    ) {
        let key = inventory.navigation_key(target);
        self.aliases
            .entry(target.into())
            .or_default()
            .push(key.clone());
        let mut reference = original.clone();
        reference.id = format!("compiled:{relation}:{}:{target}", original.id);
        reference.relation = relation.into();
        reference.candidate_keys = vec![key];
        reference.reason = reason.into();
        self.references
            .entry(reference.file.clone())
            .or_default()
            .push(reference);
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn apply(&self, facts: &mut FileFacts) {
        for node in &mut facts.nodes {
            if let Some(extra) = self.aliases.get(&node.id) {
                let mut aliases: Vec<String> = node.metadata["binding_aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                aliases.extend(extra.iter().cloned());
                aliases.sort();
                aliases.dedup();
                node.metadata["binding_aliases"] = json!(aliases);
            }
        }
        for reference in &mut facts.references {
            if let Some(keys) = self.candidates.get(&reference.id) {
                reference.candidate_keys = keys.clone();
            }
            if let Some(relation) = self.relations.get(&reference.id) {
                reference.relation = relation.clone();
            }
        }
        if let Some(extra) = self.references.get(&facts.path) {
            let old: HashSet<_> = facts.references.iter().map(|r| r.id.clone()).collect();
            facts
                .references
                .extend(extra.iter().filter(|r| !old.contains(&r.id)).cloned());
        }
        for node in &mut facts.nodes {
            if let Some(types) = node
                .metadata
                .get_mut("type_references")
                .and_then(serde_json::Value::as_array_mut)
            {
                for evidence in types {
                    if let Some(r) = facts
                        .references
                        .iter()
                        .find(|r| evidence["reference_id"] == r.id)
                    {
                        evidence["candidate_keys"] = json!(r.candidate_keys);
                    }
                }
            }
        }
    }
}
