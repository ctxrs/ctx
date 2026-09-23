use crate::model::{Diagnostic, Edge, FileFacts, Node, Reference};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use tree_sitter::{Language, Node as Syntax, Parser, Tree};

pub(super) fn children(node: Syntax<'_>) -> Vec<Syntax<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}
pub(super) fn line(node: Syntax<'_>) -> u32 {
    node.start_position().row as u32 + 1
}
pub(super) fn end_line(node: Syntax<'_>) -> u32 {
    let end = node.end_position();
    (end.row + usize::from(end.column != 0 || end.row == 0)) as u32
}
pub(super) fn module_path(path: &str) -> String {
    path.rsplit_once('.')
        .filter(|(stem, suffix)| !stem.is_empty() && !stem.ends_with('/') && !suffix.contains('/'))
        .map_or(path, |(stem, _)| stem)
        .into()
}
pub(super) fn relative_path(base: &str, import: &str) -> Option<String> {
    let mut parts: Vec<_> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in import.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}
pub(super) fn tree(
    language: Language,
    source: &str,
    facts: &mut FileFacts,
) -> Result<Option<Tree>> {
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(facts, None, "Source exceeds the 4 MiB indexing limit");
        return Ok(None);
    }
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .context("language parser did not return a tree")?;
    let mut pending = vec![(tree.root_node(), 0)];
    while let Some((node, depth)) = pending.pop() {
        if node.is_error() || node.is_missing() || depth > 256 {
            diagnostic(
                facts,
                Some(line(node)),
                if depth > 256 {
                    "Syntax nesting exceeds the indexing limit; no facts indexed"
                } else {
                    "Syntax error; no facts indexed"
                },
            );
            return Ok(None);
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor).map(|n| (n, depth + 1)));
    }
    Ok(Some(tree))
}
pub(super) fn diagnostic(facts: &mut FileFacts, line: Option<u32>, message: &str) {
    facts.diagnostics.push(Diagnostic {
        file: facts.path.clone(),
        line,
        message: message.into(),
    });
}

#[derive(Clone)]
pub(super) enum Binding {
    Symbol {
        keys: Vec<String>,
        id: Option<String>,
    },
    Namespace {
        prefixes: Vec<String>,
        separator: &'static str,
    },
    Path(String),
    Require {
        modules: Vec<String>,
        imported: Option<String>,
    },
    Unknown,
}
impl Binding {
    pub fn symbol(key: String) -> Self {
        Self::Symbol {
            keys: vec![key],
            id: None,
        }
    }
}
pub(super) struct Scope {
    pub parent: Option<usize>,
    pub owner: String,
    pub qualified: String,
    pub bindings: HashMap<String, Binding>,
    pub uncertain: bool,
    pub class: bool,
    pub function: bool,
    pub callable_capture: bool,
    pub fallback: Option<String>,
}
struct Call {
    scope: usize,
    start: usize,
    end: usize,
    line: u32,
    label: String,
    parts: Vec<String>,
    callable: Option<Box<CallableSnapshot>>,
}
#[derive(Clone)]
struct CallableOrigin {
    scope: usize,
    name: String,
    id: Option<String>,
}
struct CallableSnapshot {
    local: (usize, String),
    origin: Option<CallableOrigin>,
}
pub(super) struct Extractor<'a> {
    pub source: &'a str,
    pub facts: FileFacts,
    pub language: &'static str,
    pub scopes: Vec<Scope>,
    calls: Vec<Call>,
    definitions: Vec<(usize, String, String)>,
    writes: Vec<(usize, String, Option<usize>)>,
    callable_locals: HashMap<(usize, String), Option<CallableOrigin>>,
    callable_functions: HashSet<String>,
    callable_escapes: Vec<(usize, Option<String>, Option<usize>)>,
}
impl<'a> Extractor<'a> {
    pub fn new(
        path: &str,
        source: &'a str,
        hash: &str,
        language: &'static str,
        module: String,
    ) -> Self {
        Self {
            source,
            language,
            facts: FileFacts {
                path: path.into(),
                hash: hash.into(),
                module,
                nodes: vec![],
                edges: vec![],
                references: vec![],
                diagnostics: vec![],
            },
            scopes: vec![],
            calls: vec![],
            definitions: vec![],
            writes: vec![],
            callable_locals: HashMap::new(),
            callable_functions: HashSet::new(),
            callable_escapes: vec![],
        }
    }
    pub fn text(&self, node: Syntax<'_>) -> &'a str {
        &self.source[node.byte_range()]
    }
    pub fn root(&mut self, node: Syntax<'_>, key: String) {
        let owner = format!("{}:{}:module", self.language, self.facts.path);
        self.facts.nodes.push(Node {
            id: owner.clone(),
            label: self.facts.module.clone(),
            kind: "module".into(),
            file: self.facts.path.clone(),
            line: Some(1),
            end_line: Some(end_line(node)),
            qualified_name: Some(self.facts.module.clone()),
            binding_key: Some(key),
            metadata: self.range(node),
        });
        self.scopes.push(Scope {
            parent: None,
            owner,
            qualified: String::new(),
            bindings: HashMap::new(),
            uncertain: false,
            class: false,
            function: true,
            callable_capture: false,
            fallback: None,
        });
    }
    pub fn range(&self, node: Syntax<'_>) -> serde_json::Value {
        json!({"language": self.language, "start_byte": node.start_byte(), "end_byte": node.end_byte(), "start_column": node.start_position().column, "end_column": node.end_position().column})
    }
    pub fn scope(
        &mut self,
        parent: usize,
        qualified: String,
        owner: Option<String>,
        class: bool,
    ) -> usize {
        let index = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            owner: owner.unwrap_or_else(|| self.scopes[parent].owner.clone()),
            qualified,
            bindings: HashMap::new(),
            uncertain: false,
            class,
            function: false,
            callable_capture: false,
            fallback: None,
        });
        index
    }
    pub fn block(&mut self, parent: usize, node: Syntax<'_>) -> usize {
        self.scope(
            parent,
            format!(
                "{}.<block@{}>",
                self.scopes[parent].qualified,
                node.start_byte()
            ),
            None,
            false,
        )
    }
    fn binding_name(&self, name: &str) -> String {
        if self.language == "javascript" {
            super::javascript::identifier(name)
        } else {
            name.into()
        }
    }
    pub fn bind(&mut self, scope: usize, name: &str, binding: Binding) {
        let name = self.binding_name(name);
        if self.scopes[scope].bindings.contains_key(&name)
            && self.callable_locals.contains_key(&(scope, name.clone()))
        {
            self.escape_callable_local(scope, Some(&name));
        }
        self.scopes[scope]
            .bindings
            .entry(name)
            .and_modify(|b| *b = Binding::Unknown)
            .or_insert(binding);
    }
    pub fn invalidate(&mut self, scope: usize, name: &str) {
        let name = self.binding_name(name);
        let owner = self.invalidate_known(scope, &name);
        // A later local shadow must not hide an already observed captured write.
        self.writes.push((scope, name.clone(), owner));
        if let Some(owner) = owner
            && self.function_scope(scope) == self.function_scope(owner)
            && let Some(value) = self.callable_locals.get_mut(&(owner, name.clone()))
        {
            *value = None;
        }
    }
    fn write_binding(&self, scope: usize, name: &str) -> Option<usize> {
        let mut current = Some(scope);
        while let Some(i) = current {
            if self.scopes[i].bindings.contains_key(name) {
                return Some(i);
            }
            current = self.scopes[i].parent;
        }
        None
    }
    fn invalidate_known(&mut self, scope: usize, name: &str) -> Option<usize> {
        let owner = self.write_binding(scope, name)?;
        self.scopes[owner]
            .bindings
            .insert(name.into(), Binding::Unknown);
        Some(owner)
    }
    pub fn local_key(&self, scope: usize, name: &str) -> String {
        format!(
            "{}:local:{}:{}:{name}",
            self.language, self.facts.path, self.scopes[scope].qualified
        )
    }
    pub fn define(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        name: &str,
        kind: &str,
        key: Option<String>,
        bind: bool,
    ) -> usize {
        let qualified = if self.scopes[scope].qualified.is_empty() {
            name.into()
        } else {
            format!("{}.{name}", self.scopes[scope].qualified)
        };
        let id = format!(
            "{}:{}:{qualified}@{}",
            self.language,
            self.facts.path,
            node.start_byte()
        );
        if kind == "function"
            && bind
            && matches!(self.language, "javascript" | "go" | "rust")
            && matches!(
                node.kind(),
                "function_declaration" | "generator_function_declaration" | "function_item"
            )
        {
            self.callable_functions.insert(id.clone());
        }
        if bind {
            let binding = key
                .as_ref()
                .map_or(Binding::Unknown, |key| Binding::Symbol {
                    keys: vec![key.clone()],
                    id: Some(id.clone()),
                });
            self.bind(scope, name, binding);
            self.definitions
                .push((scope, self.binding_name(name), id.clone()));
        }
        self.facts.nodes.push(Node {
            id: id.clone(),
            label: name.into(),
            kind: kind.into(),
            file: self.facts.path.clone(),
            line: Some(line(node)),
            end_line: Some(end_line(node)),
            qualified_name: Some(qualified.clone()),
            binding_key: key,
            metadata: self.range(node),
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
            metadata: serde_json::Value::Null,
        });
        let child = self.scope(
            scope,
            qualified,
            Some(id),
            matches!(kind, "class" | "struct" | "trait" | "interface" | "impl"),
        );
        self.scopes[child].function = matches!(kind, "function" | "method");
        child
    }
    pub fn reference(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        label: String,
        relation: &str,
        keys: Vec<String>,
        reason: &str,
    ) {
        self.facts.references.push(Reference {
            id: format!(
                "{relation}:{}:{}-{}:{}",
                self.scopes[scope].owner,
                node.start_byte(),
                node.end_byte(),
                self.facts.references.len()
            ),
            source: self.scopes[scope].owner.clone(),
            label,
            relation: relation.into(),
            file: self.facts.path.clone(),
            line: line(node),
            candidate_keys: keys,
            reason: reason.into(),
        });
    }
    pub fn call(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        target: Syntax<'_>,
        parts: Option<Vec<String>>,
    ) {
        self.calls.push(Call {
            scope,
            start: node.start_byte(),
            end: node.end_byte(),
            line: line(node),
            label: self.text(target).into(),
            parts: parts.unwrap_or_default(),
            callable: None,
        });
    }
    fn function_scope(&self, mut scope: usize) -> usize {
        while !self.scopes[scope].function {
            scope = self.scopes[scope].parent.unwrap_or(0);
        }
        scope
    }
    fn lexical_binding(&self, mut scope: usize, name: &str) -> Option<usize> {
        loop {
            let current = &self.scopes[scope];
            if current.uncertain {
                return None;
            }
            if !current.class && current.bindings.contains_key(name) {
                return Some(scope);
            }
            scope = current.parent?;
        }
    }
    /// Register before ordinary pattern binding; parameters and redeclarations
    /// cannot become new local value identities. The initializer is recorded later.
    pub fn declare_callable_local(&mut self, scope: usize, name: &str) {
        let name = self.binding_name(name);
        if self.function_scope(scope) != 0 && !self.scopes[scope].bindings.contains_key(&name) {
            self.callable_locals.entry((scope, name)).or_insert(None);
        }
    }
    /// Record only after evaluating the RHS. None kills the current proof.
    pub fn assign_callable_local(&mut self, scope: usize, name: &str, rhs: Option<&str>) {
        let name = self.binding_name(name);
        let Some(owner) = self.lexical_binding(scope, &name) else {
            return;
        };
        let local = (owner, name);
        if !self.callable_locals.contains_key(&local)
            || self.function_scope(scope) != self.function_scope(owner)
        {
            return;
        }
        let origin = rhs.and_then(|rhs| {
            let name = self.binding_name(rhs);
            let id = if let Some(binding) = self.lexical_binding(scope, &name) {
                match &self.scopes[binding].bindings[&name] {
                    Binding::Symbol { id: Some(id), .. }
                        if self.callable_functions.contains(id) =>
                    {
                        Some(id.clone())
                    }
                    _ => return None,
                }
            } else {
                // A later ordinary function declaration can be hoisted/an item.
                // Final validation still requires its real lexical definition ID.
                None
            };
            Some(CallableOrigin { scope, name, id })
        });
        self.callable_locals.insert(local, origin);
    }
    pub fn escape_callable_local(&mut self, scope: usize, name: Option<&str>) {
        let name = name.map(|name| self.binding_name(name));
        let owner = name
            .as_deref()
            .and_then(|name| self.write_binding(scope, name));
        if name.is_none() {
            // Opaque closure syntax can write captured values. Observe their
            // identities now, before subsequent declarations can shadow them.
            for (owner, name) in self.callable_locals.keys() {
                if self.function_scope(*owner) != self.function_scope(scope)
                    && self.captures_callable_local(scope, *owner)
                {
                    self.callable_escapes
                        .push((scope, Some(name.clone()), Some(*owner)));
                }
            }
        }
        self.callable_escapes.push((scope, name, owner));
    }
    fn captures_callable_local(&self, mut scope: usize, owner: usize) -> bool {
        // Opaque expansion may name definition-site locals despite a shadow at
        // the invocation. Bound it by scope ancestry and function capture only.
        while scope != owner {
            let current = &self.scopes[scope];
            if current.function && !current.callable_capture {
                return false;
            }
            let Some(parent) = current.parent else {
                return false;
            };
            scope = parent;
        }
        true
    }
    /// Opt in only for a straight-line direct invocation. call() and its probes
    /// deliberately keep their existing final-binding semantics.
    pub fn call_with_callable_local(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        target: Syntax<'_>,
        parts: Option<Vec<String>>,
    ) {
        self.call(node, scope, target, parts);
        if self.callable_locals.is_empty() {
            return;
        }
        let call = self.calls.last().unwrap();
        let snapshot = if let [name] = call.parts.as_slice() {
            let name = self.binding_name(name);
            self.lexical_binding(scope, &name).and_then(|owner| {
                let local = (owner, name);
                self.callable_locals
                    .get(&local)
                    .map(|origin| CallableSnapshot {
                        origin: (self.function_scope(scope) == self.function_scope(owner))
                            .then(|| origin.clone())
                            .flatten(),
                        local,
                    })
            })
        } else {
            None
        };
        self.calls.last_mut().unwrap().callable = snapshot.map(Box::new);
    }
    fn callable_keys(&self, call: &Call, snapshot: &CallableSnapshot) -> Vec<String> {
        let Some(origin) = &snapshot.origin else {
            return vec![];
        };
        let (owner, name) = &snapshot.local;
        let function = self.function_scope(*owner);
        if self.function_scope(call.scope) != function
            || self.lexical_binding(call.scope, name) != Some(*owner)
            || self
                .callable_escapes
                .iter()
                .any(|(scope, escaped, observed)| {
                    escaped.as_ref().map_or_else(
                        || self.function_scope(*scope) == function,
                        |escaped| {
                            escaped == name
                                && observed.or_else(|| self.write_binding(*scope, escaped))
                                    == Some(*owner)
                        },
                    )
                })
            || self.writes.iter().any(|(scope, written, observed)| {
                written == name
                    && self.function_scope(*scope) != function
                    && observed.or_else(|| self.write_binding(*scope, written)) == Some(*owner)
            })
        {
            return vec![];
        }
        let Some(binding) = self.lexical_binding(origin.scope, &origin.name) else {
            return vec![];
        };
        let Some(Binding::Symbol { id: Some(id), .. }) =
            self.scopes[binding].bindings.get(&origin.name)
        else {
            return vec![];
        };
        if !self.callable_functions.contains(id)
            || origin.id.as_ref().is_some_and(|found| found != id)
        {
            return vec![];
        }
        self.resolve_reference(origin.scope, std::slice::from_ref(&origin.name))
    }
    pub fn unbound(&self, scope: usize, name: &str) -> bool {
        let name = self.binding_name(name);
        let mut current = Some(scope);
        while let Some(i) = current {
            let scope = &self.scopes[i];
            if scope.uncertain || (!scope.class && scope.bindings.contains_key(&name)) {
                return false;
            }
            current = scope.parent;
        }
        true
    }
    pub fn resolve(&self, scope: usize, parts: &[String]) -> Vec<String> {
        let canonical;
        let parts = if self.language == "javascript" {
            canonical = parts
                .iter()
                .map(|p| self.binding_name(p))
                .collect::<Vec<_>>();
            &canonical
        } else {
            parts
        };
        let Some(name) = parts.first() else {
            return vec![];
        };
        let mut current = Some(scope);
        while let Some(i) = current {
            let scope = &self.scopes[i];
            if scope.uncertain {
                return vec![];
            }
            if !scope.class {
                if let Some(binding) = scope.bindings.get(name) {
                    return match binding {
                        Binding::Symbol { keys, .. } if parts.len() == 1 => keys.clone(),
                        Binding::Require { modules, imported } => {
                            let symbol = match (imported.as_deref(), parts.len()) {
                                (Some(name), 1) => Some(name),
                                (None, 1) => Some("default"),
                                (None, 2) => Some(parts[1].as_str()),
                                _ => None,
                            };
                            symbol
                                .map(|name| {
                                    modules
                                        .iter()
                                        .map(|m| super::javascript::commonjs_key(m, name))
                                        .collect()
                                })
                                .unwrap_or_default()
                        }
                        Binding::Path(key) => vec![if parts.len() == 1 {
                            key.clone()
                        } else {
                            format!("{key}::{}", parts[1..].join("::"))
                        }],
                        Binding::Namespace {
                            prefixes,
                            separator,
                        } if parts.len() > 1 => prefixes
                            .iter()
                            .map(|p| format!("{p}{}", parts[1..].join(separator)))
                            .collect(),
                        _ => vec![],
                    };
                }
                if let Some(prefix) = &scope.fallback {
                    if parts.len() == 1 {
                        return vec![format!("{prefix}{name}")];
                    }
                    if self.language == "go"
                        && parts.len() == 2
                        && self.facts.nodes[0].metadata["imports"]
                            .as_array()
                            .is_some_and(|imports| imports.iter().any(|i| i["alias"].is_null()))
                    {
                        return vec![format!("go:selector:{}:{}", parts[0], parts[1])];
                    }
                    if self.language == "rust" {
                        return vec![format!("{prefix}{}", parts.join("::"))];
                    }
                }
            }
            current = scope.parent;
        }
        vec![]
    }
    /// Keep a known lexical JavaScript definition distinct from an import search.
    pub fn resolve_reference(&self, scope: usize, parts: &[String]) -> Vec<String> {
        let keys = self.resolve(scope, parts);
        if self.language != "javascript" || parts.len() != 1 || keys.is_empty() {
            return keys;
        }
        let name = self.binding_name(&parts[0]);
        let mut current = Some(scope);
        while let Some(i) = current {
            let scope = &self.scopes[i];
            if !scope.class
                && let Some(binding) = scope.bindings.get(&name)
            {
                if matches!(binding, Binding::Symbol { id: Some(_), .. }) {
                    let prefix = format!("javascript:{}:", self.facts.module);
                    return keys
                        .into_iter()
                        .map(|key| {
                            if key.starts_with(&format!("javascript:local:{}:", self.facts.path)) {
                                return key;
                            }
                            key.strip_prefix(&prefix).map_or_else(
                                || key.clone(),
                                |name| format!("javascript:file:{}:{name}", self.facts.path),
                            )
                        })
                        .collect();
                }
                return keys;
            }
            current = scope.parent;
        }
        keys
    }
    pub fn finish(mut self) -> FileFacts {
        let writes = std::mem::take(&mut self.writes);
        for (scope, name, _) in &writes {
            let _ = self.invalidate_known(*scope, name);
        }
        self.writes = writes;
        let mut invalid = HashSet::new();
        for (scope, name, id) in &self.definitions {
            let valid = !self.scopes[*scope].uncertain
                && matches!(self.scopes[*scope].bindings.get(name), Some(Binding::Symbol { id: Some(found), .. }) if found == id);
            if !valid {
                invalid.insert(id.clone());
            }
        }
        for node in &mut self.facts.nodes {
            if invalid.contains(&node.id) {
                node.binding_key = None;
            }
            if node.binding_key.is_none() || node.metadata["conditional"] == true {
                self.callable_functions.remove(&node.id);
            }
        }
        for call in &self.calls {
            let keys = call.callable.as_ref().map_or_else(
                || self.resolve_reference(call.scope, &call.parts),
                |snapshot| self.callable_keys(call, snapshot),
            );
            let reason = if keys.is_empty() {
                "dynamic, shadowed, or unsupported binding"
            } else if call.callable.is_some() {
                "straight-line local function value; static target is unavailable or ambiguous"
            } else {
                "static target is unavailable or ambiguous"
            };
            self.facts.references.push(Reference {
                id: format!(
                    "call:{}:{}-{}",
                    self.scopes[call.scope].owner, call.start, call.end
                ),
                source: self.scopes[call.scope].owner.clone(),
                label: call.label.clone(),
                relation: "calls".into(),
                file: self.facts.path.clone(),
                line: call.line,
                candidate_keys: keys,
                reason: reason.into(),
            });
        }
        self.facts
    }
}
