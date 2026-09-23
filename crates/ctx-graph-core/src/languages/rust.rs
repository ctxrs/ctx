use super::common::*;
use crate::model::FileFacts;
use anyhow::Result;
use std::collections::HashMap;
use tree_sitter::Node as Syntax;

pub(super) fn parse(path: &str, source: &str, hash: &str) -> Result<FileFacts> {
    let (root, relative) = if let Some(rest) = path.strip_prefix("src/") {
        (".".to_owned(), rest)
    } else if let Some((root, rest)) = path.rsplit_once("/src/") {
        (root.to_owned(), rest)
    } else {
        (format!("file/{}", module_path(path)), "lib.rs")
    };
    let stem = module_path(relative);
    let module = if stem == "lib" || stem == "main" {
        String::new()
    } else {
        stem.strip_suffix("/mod")
            .unwrap_or(&stem)
            .replace('/', "::")
    };
    let mut e = Extractor::new(path, source, hash, "rust", module.clone());
    let Some(tree) = tree(tree_sitter_rust::LANGUAGE.into(), source, &mut e.facts)? else {
        return Ok(e.facts);
    };
    e.root(tree.root_node(), format!("rust:module:{root}:{module}"));
    e.scopes[0].fallback = Some(format!("rust:{root}:{}", prefix(&module)));
    let mut r = Rust {
        e,
        root,
        modules: HashMap::from([(0, module.clone())]),
        module_bindings: HashMap::new(),
        module_aliases: HashMap::new(),
        value_bindings: HashMap::new(),
        local_uses: vec![],
        paths: vec![],
        implementations: vec![],
        receivers: vec![],
    };
    r.visit(tree.root_node(), 0, &module, None);
    Ok(r.finish())
}
fn public(node: Syntax<'_>, source: &str) -> bool {
    children(node)
        .iter()
        .any(|n| n.kind() == "visibility_modifier" && &source[n.byte_range()] == "pub")
}
fn prefix(module: &str) -> String {
    if module.is_empty() {
        String::new()
    } else {
        format!("{module}::")
    }
}
struct LocalUse {
    scope: usize,
    name: String,
    parts: Vec<String>,
    key: String,
    references: std::ops::Range<usize>,
}
struct Rust<'a> {
    e: Extractor<'a>,
    root: String,
    modules: HashMap<usize, String>,
    // Modules occupy the type namespace; a same-named function is a value.
    module_bindings: HashMap<usize, HashMap<String, Option<String>>>,
    // Only aliases of proven module heads belong here, never imported members.
    module_aliases: HashMap<usize, HashMap<String, String>>,
    // Unknown call targets can still be proven to occupy only the value namespace.
    value_bindings: HashMap<usize, HashMap<String, bool>>,
    local_uses: Vec<LocalUse>,
    paths: Vec<(usize, usize, Vec<String>, String)>,
    implementations: Vec<(String, usize, Vec<String>, String)>,
    receivers: Vec<(String, usize, Vec<String>, String)>,
}
impl Rust<'_> {
    fn callable_sequence(mut node: Syntax<'_>) -> bool {
        while let Some(parent) = node.parent() {
            match parent.kind() {
                "block" if !children(parent).iter().any(|n| n.kind() == "label") => {}
                "expression_statement" | "return_expression" => {}
                "function_item" | "closure_expression" => {
                    return parent.child_by_field_name("body") == Some(node);
                }
                _ => return false,
            }
            node = parent;
        }
        false
    }
    fn record_binding(&mut self, scope: usize, name: &str, value_only: bool) {
        self.value_bindings
            .entry(scope)
            .or_default()
            .entry(name.trim_start_matches("r#").into())
            .and_modify(|known| *known &= value_only)
            .or_insert(value_only);
    }
    fn bind(&mut self, scope: usize, name: &str, binding: Binding, value_only: bool) {
        let name = name.trim_start_matches("r#");
        self.record_binding(scope, name, value_only);
        self.e.bind(scope, name, binding);
    }
    fn resolve_path(&self, scope: usize, parts: &[String], module: &str) -> Vec<String> {
        if parts.first().is_some_and(|p| {
            matches!(p.as_str(), "crate" | "super") || p == "self" && self.e.unbound(scope, "self")
        }) {
            self.absolute(parts, module, false).into_iter().collect()
        } else {
            self.module_candidates(scope, parts)
                .unwrap_or_else(|| self.e.resolve(scope, parts))
        }
    }
    // Some means a known module, blocked name, or uncertain scope was found.
    // None leaves ordinary paths to shared resolution, never proving a use.
    fn module_candidates(&self, scope: usize, parts: &[String]) -> Option<Vec<String>> {
        let name = parts.first()?;
        let mut current = Some(scope);
        while let Some(index) = current {
            let lexical = &self.e.scopes[index];
            if lexical.uncertain {
                return Some(vec![]);
            }
            if !lexical.class {
                let value_only =
                    self.value_bindings.get(&index).and_then(|m| m.get(name)) == Some(&true);
                if let Some(key) = self.module_bindings.get(&index).and_then(|m| m.get(name)) {
                    // Imports and other type bindings can conflict with a module.
                    // Value-only bindings cannot, even with unknown call targets.
                    if lexical.bindings.contains_key(name) && !value_only {
                        return Some(vec![]);
                    }
                    return Some(
                        key.iter()
                            .map(|key| {
                                if parts.len() == 1 {
                                    key.clone()
                                } else {
                                    format!("{key}::{}", parts[1..].join("::"))
                                }
                            })
                            .collect(),
                    );
                }
                if let Some(key) = self.module_aliases.get(&index).and_then(|m| m.get(name)) {
                    return Some(
                        matches!(lexical.bindings.get(name), Some(Binding::Path(bound)) if bound == key)
                            .then(|| {
                                if parts.len() == 1 {
                                    key.clone()
                                } else {
                                    format!("{key}::{}", parts[1..].join("::"))
                                }
                            })
                            .into_iter()
                            .collect(),
                    );
                }
                if !value_only && matches!(lexical.bindings.get(name), Some(Binding::Unknown)) {
                    return Some(vec![]);
                }
                if lexical.bindings.contains_key(name) && !value_only || lexical.fallback.is_some()
                {
                    break;
                }
            }
            current = lexical.parent;
        }
        None
    }
    fn local_use_origin(&self, import: &LocalUse) -> Option<(Vec<String>, bool)> {
        let mut scope = import.scope;
        let mut parts = import.parts.as_slice();
        if parts.first()?.as_str() == "self" {
            // self:: starts at the containing module, not a block-local binding.
            loop {
                if self.e.scopes[scope].uncertain {
                    return Some((vec![], false));
                }
                if self.modules.contains_key(&scope) {
                    break;
                }
                scope = self.e.scopes[scope].parent?;
            }
            parts = &parts[1..];
        }
        let keys = self.module_candidates(scope, parts)?;
        // A member path can name a type or value; its module shape is unknown.
        Some((keys, parts.len() == 1))
    }
    fn type_refs(&mut self, node: Syntax<'_>, scope: usize, module: &str, relation: &str) {
        if matches!(node.kind(), "type_identifier" | "scoped_type_identifier") {
            if let Some(parts) = self.path(node) {
                let index = self.e.facts.references.len();
                self.e.reference(
                    node,
                    scope,
                    self.e.text(node).into(),
                    relation,
                    vec![],
                    "explicit Rust type is unavailable or ambiguous",
                );
                self.paths.push((index, scope, parts, module.into()));
            }
            return;
        }
        for n in children(node) {
            if !matches!(
                n.kind(),
                "attribute_item" | "visibility_modifier" | "macro_invocation" | "lifetime"
            ) {
                self.type_refs(
                    n,
                    scope,
                    module,
                    if node.kind() == "type_arguments" {
                        "type_argument"
                    } else {
                        relation
                    },
                );
            }
        }
    }
    // Only independent, unbounded type parameters can be renamed without
    // proving trait bounds, substitutions, lifetimes, or const expressions.
    fn plain_parameters<'n>(&self, node: Syntax<'n>) -> Option<Vec<Syntax<'n>>> {
        if children(node).iter().any(|n| n.kind() == "where_clause") {
            return None;
        }
        let params = node.child_by_field_name("type_parameters")?;
        let mut names = vec![];
        for param in children(params)
            .into_iter()
            .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
        {
            let name = param.child_by_field_name("name")?;
            if param.kind() != "type_parameter"
                || children(param)
                    .iter()
                    .any(|n| *n != name && !matches!(n.kind(), "line_comment" | "block_comment"))
                || names.iter().any(|n| self.e.text(*n) == self.e.text(name))
            {
                return None;
            }
            names.push(name);
        }
        (!names.is_empty()).then_some(names)
    }
    fn impl_owner(&self, node: Syntax<'_>, ty: Syntax<'_>) -> Option<Vec<String>> {
        if node.child_by_field_name("trait").is_some()
            || children(node).iter().any(|n| n.kind() == "where_clause")
        {
            return None;
        }
        let owner = if ty.kind() == "generic_type" {
            let params = self.plain_parameters(node)?;
            let args: Vec<_> = children(ty.child_by_field_name("type_arguments")?)
                .into_iter()
                .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
                .collect();
            if params.len() != args.len()
                || params.iter().zip(args).any(|(p, a)| {
                    a.kind() != "type_identifier" || self.e.text(*p) != self.e.text(a)
                })
            {
                return None;
            }
            ty.child_by_field_name("type")?
        } else {
            if node.child_by_field_name("type_parameters").is_some() {
                return None;
            }
            ty
        };
        let mut pending = vec![owner];
        while let Some(part) = pending.pop() {
            if !matches!(
                part.kind(),
                "type_identifier"
                    | "identifier"
                    | "scoped_type_identifier"
                    | "scoped_identifier"
                    | "crate"
                    | "self"
                    | "super"
            ) {
                return None;
            }
            pending.extend(children(part));
        }
        self.path(owner)
    }
    fn finish(mut self) -> FileFacts {
        // Prove origins only after later declarations, attributes and imports
        // have had a chance to invalidate them. Updating the binding here also
        // carries the native path into deferred calls, types and impl owners.
        let mut pending = std::mem::take(&mut self.local_uses);
        while !pending.is_empty() {
            let count = pending.len();
            let mut unresolved = vec![];
            for import in pending {
                let origin = if !matches!(self.e.scopes[import.scope].bindings.get(&import.name),
                    Some(Binding::Path(key)) if key == &import.key)
                    || self
                        .module_bindings
                        .get(&import.scope)
                        .is_some_and(|m| m.contains_key(&import.name))
                {
                    Some((vec![], false))
                } else {
                    self.local_use_origin(&import)
                };
                let Some((keys, is_module)) = origin else {
                    unresolved.push(import);
                    continue;
                };
                let [key] = keys.as_slice() else {
                    // A blocked local origin must not fall through to a same-named
                    // dependency. Unknown also blocks dependent named uses.
                    self.e.scopes[import.scope]
                        .bindings
                        .insert(import.name, Binding::Unknown);
                    for index in import.references {
                        self.e.facts.references[index].candidate_keys.clear();
                    }
                    continue;
                };
                self.e.scopes[import.scope]
                    .bindings
                    .insert(import.name.clone(), Binding::Path(key.clone()));
                if is_module {
                    self.module_aliases
                        .entry(import.scope)
                        .or_default()
                        .insert(import.name, key.clone());
                }
                for index in import.references {
                    for candidate in &mut self.e.facts.references[index].candidate_keys {
                        if candidate == &import.key {
                            *candidate = key.clone();
                        }
                    }
                }
            }
            // Each advancing pass consumes at least one use. Cycles and unknown
            // origins stop without turning arbitrary imported paths into proof.
            if unresolved.len() == count {
                break;
            }
            pending = unresolved;
        }
        for (index, scope, parts, module) in &self.paths {
            self.e.facts.references[*index].candidate_keys =
                self.resolve_path(*scope, parts, module);
        }
        let implementations: Vec<_> = self
            .implementations
            .iter()
            .map(|(marker, scope, parts, module)| {
                (marker.clone(), self.resolve_path(*scope, parts, module))
            })
            .collect();
        let receivers: HashMap<_, _> = self
            .receivers
            .iter()
            .map(|(marker, scope, parts, module)| {
                (marker.clone(), self.resolve_path(*scope, parts, module))
            })
            .collect();
        let mut facts = self.e.finish();
        for reference in &mut facts.references {
            let mut declared = false;
            reference.candidate_keys = reference
                .candidate_keys
                .iter()
                .flat_map(|key| {
                    if let Some((marker, member)) = key.rsplit_once(':')
                        && let Some(types) = receivers.get(marker)
                    {
                        declared = true;
                        if member.contains('.') {
                            return vec![];
                        }
                        types
                            .iter()
                            .map(|ty| format!("{ty}#declared.{member}"))
                            .collect()
                    } else {
                        vec![key.clone()]
                    }
                })
                .collect();
            if declared {
                reference.relation = "declared_member".into();
                reference.reason =
                    "written dyn trait member; runtime dispatch is unresolved".into();
            }
        }
        let owners: HashMap<_, _> = facts
            .nodes
            .iter()
            .filter(|n| n.kind == "trait" && n.binding_key.is_some())
            .filter(|n| {
                facts
                    .nodes
                    .iter()
                    .filter(|other| other.binding_key == n.binding_key)
                    .count()
                    == 1
            })
            .filter_map(|n| {
                n.binding_key
                    .as_ref()
                    .map(|key| (n.id.clone(), key.clone()))
            })
            .collect();
        let parents: HashMap<_, _> = facts
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .map(|e| (e.target.clone(), e.source.clone()))
            .collect();
        for node in &mut facts.nodes {
            if node.kind == "method"
                && node.metadata["conditional"] != true
                && node.metadata["trait_receiver"] == true
                && let Some(owner) = parents.get(&node.id).and_then(|id| owners.get(id))
            {
                node.metadata["binding_aliases"] =
                    serde_json::json!([format!("{owner}#declared.{}", node.label)]);
            }
        }
        for (marker, keys) in implementations {
            let rewrite = |key: &str| -> Option<String> {
                if key == marker || key.starts_with(&format!("{marker}::")) {
                    (keys.len() == 1).then(|| format!("{}{}", keys[0], &key[marker.len()..]))
                } else {
                    Some(key.into())
                }
            };
            for node in &mut facts.nodes {
                node.binding_key = node.binding_key.as_deref().and_then(&rewrite);
                for field in ["impl_type", "generic_impl_type"] {
                    if let Some(target) = node.metadata[field].as_str() {
                        node.metadata[field] =
                            rewrite(target).map_or(serde_json::Value::Null, Into::into);
                    }
                }
            }
            for reference in &mut facts.references {
                reference.candidate_keys = reference
                    .candidate_keys
                    .iter()
                    .filter_map(|k| rewrite(k))
                    .collect();
            }
        }
        facts
    }
    fn path(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        match node.kind() {
            "identifier" | "type_identifier" | "crate" | "self" | "super" => {
                Some(vec![self.e.text(node).trim_start_matches("r#").into()])
            }
            "scoped_identifier" | "scoped_type_identifier" => {
                let mut parts = node
                    .child_by_field_name("path")
                    .map(|n| self.path(n))
                    .unwrap_or(Some(vec![]))?;
                parts.extend(self.path(node.child_by_field_name("name")?)?);
                Some(parts)
            }
            "field_expression" => {
                let mut parts = self.path(node.child_by_field_name("value")?)?;
                let field = node.child_by_field_name("field")?;
                if field.kind() != "field_identifier" {
                    return None;
                }
                parts.push(self.e.text(field).trim_start_matches("r#").into());
                Some(parts)
            }
            "generic_function" | "generic_type" => self.path(
                node.child_by_field_name("function")
                    .or_else(|| node.child_by_field_name("type"))?,
            ),
            "parenthesized_expression" => self.path(node.named_child(0)?),
            _ => None,
        }
    }
    fn absolute(&self, parts: &[String], module: &str, use_path: bool) -> Option<String> {
        let first = parts.first()?;
        let mut tail = parts;
        let mut base: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
        if first == "crate" {
            base.clear();
            tail = &parts[1..];
        } else if first == "self" {
            tail = &parts[1..];
        } else if first == "super" {
            while tail.first().is_some_and(|p| p == "super") {
                base.pop()?;
                tail = &tail[1..];
            }
        } else if use_path {
            return Some(format!("rust:external:{}", parts.join("::")));
        }
        base.extend(tail.iter().map(String::as_str));
        Some(format!("rust:{}:{}", self.root, base.join("::")))
    }
    fn pattern(&mut self, node: Syntax<'_>, scope: usize, write: bool) {
        match node.kind() {
            "identifier" | "shorthand_field_identifier" | "self" => {
                let name = self.e.text(node).trim_start_matches("r#");
                if write {
                    self.e.invalidate(scope, name);
                } else {
                    self.bind(scope, name, Binding::Unknown, true);
                }
            }
            "type_parameter" | "const_parameter" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.bind(scope, self.e.text(name), Binding::Unknown, false);
                }
            }
            "parameter" | "let_declaration" => {
                if let Some(p) = node.child_by_field_name("pattern") {
                    let ty = node.child_by_field_name("type").and_then(|ty| {
                        let ty = if ty.kind() == "reference_type" {
                            ty.child_by_field_name("type")?
                        } else {
                            ty
                        };
                        (ty.kind() == "dynamic_type")
                            .then_some(ty)
                            .and_then(|ty| ty.child_by_field_name("trait"))
                            .filter(|ty| {
                                matches!(ty.kind(), "type_identifier" | "scoped_type_identifier")
                            })
                            .and_then(|ty| self.path(ty))
                    });
                    if !write
                        && p.kind() == "identifier"
                        && let Some(parts) = ty
                    {
                        let mut owner = scope;
                        while !self.modules.contains_key(&owner) {
                            let Some(parent) = self.e.scopes[owner].parent else {
                                break;
                            };
                            owner = parent;
                        }
                        let module = self.modules.get(&owner).cloned().unwrap_or_default();
                        let marker = format!(
                            "rust:receiver:{}:{scope}:{}",
                            self.e.facts.path,
                            p.start_byte()
                        );
                        self.receivers.push((marker.clone(), scope, parts, module));
                        self.bind(
                            scope,
                            self.e.text(p).trim_start_matches("r#"),
                            Binding::Namespace {
                                prefixes: vec![format!("{marker}:")],
                                separator: ".",
                            },
                            true,
                        );
                    } else {
                        self.pattern(p, scope, write);
                    }
                }
            }
            "field_pattern" => {
                if let Some(p) = node
                    .child_by_field_name("pattern")
                    .or_else(|| node.child_by_field_name("name"))
                {
                    self.pattern(p, scope, write);
                }
            }
            "tuple_struct_pattern" => {
                for n in children(node) {
                    if Some(n) != node.child_by_field_name("type") {
                        self.pattern(n, scope, write);
                    }
                }
            }
            "parameters" | "type_parameters" | "match_pattern" | "closure_parameters"
            | "tuple_pattern" | "slice_pattern" | "struct_pattern" | "reference_pattern"
            | "mut_pattern" | "captured_pattern" | "or_pattern" | "self_parameter" => {
                for n in children(node) {
                    self.pattern(n, scope, write);
                }
            }
            _ => {}
        }
    }
    fn use_item(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        module: &str,
        before: &[String],
        exported: bool,
        conditional: bool,
    ) {
        match node.kind() {
            "use_list" => {
                for child in children(node) {
                    self.use_item(child, scope, module, before, exported, conditional);
                }
            }
            "scoped_use_list" => {
                let mut p = before.to_vec();
                if let Some(path) = node.child_by_field_name("path").and_then(|n| self.path(n)) {
                    p.extend(path);
                }
                if let Some(list) = node.child_by_field_name("list") {
                    self.use_item(list, scope, module, &p, exported, conditional);
                }
            }
            "use_wildcard" => {
                self.e.scopes[scope].uncertain = true;
                self.e.reference(
                    node,
                    scope,
                    self.e.text(node).into(),
                    "imports",
                    vec![],
                    "glob import requires name resolution and remains unresolved",
                );
            }
            _ => {
                let path = node
                    .child_by_field_name("path")
                    .filter(|_| node.kind() == "use_as_clause")
                    .unwrap_or(node);
                let mut parts = before.to_vec();
                if let Some(p) = self.path(path) {
                    parts.extend(p);
                } else {
                    return;
                }
                if parts.len() > 1 && parts.last().is_some_and(|p| p == "self") {
                    parts.pop();
                }
                let local = node
                    .child_by_field_name("alias")
                    .map(|n| self.e.text(n).trim_start_matches("r#").to_owned())
                    .or_else(|| parts.last().cloned())
                    .unwrap_or_default();
                let key = self.absolute(&parts, module, true);
                let start = self.e.facts.references.len();
                self.bind(
                    scope,
                    &local,
                    if conditional {
                        Binding::Unknown
                    } else {
                        key.clone().map_or(Binding::Unknown, Binding::Path)
                    },
                    false,
                );
                if exported && local != "_" {
                    let child = self.e.define(node, scope, &local, "reexport", None, false);
                    let alias = format!("rust:{}:{}{local}", self.root, prefix(module));
                    let item = self.e.facts.nodes.last_mut().unwrap();
                    item.metadata["public"] = true.into();
                    item.metadata["reexport_key"] = alias.into();
                    self.e.reference(
                        node,
                        child,
                        self.e.text(node).into(),
                        "reexports",
                        key.clone().into_iter().collect(),
                        "public use target is unavailable or ambiguous",
                    );
                }
                self.e.reference(
                    node,
                    scope,
                    self.e.text(node).into(),
                    "imports",
                    key.clone().into_iter().collect(),
                    "use target is external, unavailable, or ambiguous",
                );
                if !conditional
                    && !matches!(parts.first().map(String::as_str), Some("crate" | "super"))
                    && !std::iter::successors(Some(node), |n| n.parent())
                        .take_while(|n| n.kind() != "use_declaration")
                        .any(|n| self.e.text(n).starts_with("::"))
                    && let Some(key) = key
                {
                    self.local_uses.push(LocalUse {
                        scope,
                        name: local,
                        parts,
                        key,
                        references: start..self.e.facts.references.len(),
                    });
                }
            }
        }
    }
    fn function(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        module: &str,
        implementation: Option<&str>,
    ) {
        let name = node
            .child_by_field_name("name")
            .map(|n| self.e.text(n).trim_start_matches("r#").to_owned())
            .unwrap_or_else(|| format!("<closure@{}>", node.start_byte()));
        let closure = node.kind() == "closure_expression";
        let method = implementation.is_some();
        let key = if closure {
            None
        } else if let Some(ty) = implementation {
            if ty.is_empty() {
                None
            } else {
                Some(format!("{ty}::{name}"))
            }
        } else if self.modules.contains_key(&scope) {
            Some(format!("rust:{}:{}{name}", self.root, prefix(module)))
        } else {
            Some(self.e.local_key(scope, &name))
        };
        let child = self.e.define(
            node,
            scope,
            &name,
            if method { "method" } else { "function" },
            key,
            !method && !closure,
        );
        self.e.scopes[child].callable_capture = closure;
        if !method && !closure {
            // define registers the shared symbol; retain its namespace after invalidation.
            self.record_binding(scope, &name, true);
        }
        self.e.facts.nodes.last_mut().unwrap().metadata["public"] =
            public(node, self.e.source).into();
        self.e.facts.nodes.last_mut().unwrap().metadata["trait_receiver"] = node
            .child_by_field_name("parameters")
            .is_some_and(|p| children(p).iter().any(|n| n.kind() == "self_parameter"))
            .into();
        if implementation == Some("")
            && node.parent().is_some_and(|body| {
                children(body)
                    .iter()
                    .filter(|member| {
                        member.child_by_field_name("name").is_some_and(|other| {
                            self.e.text(other).trim_start_matches("r#") == name
                        })
                    })
                    .count()
                    != 1
            })
        {
            self.e.facts.nodes.last_mut().unwrap().metadata["trait_receiver"] = false.into();
        }
        if let Some(ty) = implementation.filter(|t| !t.is_empty()) {
            self.e.facts.nodes.last_mut().unwrap().metadata["impl_type"] = ty.into();
            self.bind(child, "Self", Binding::Path(ty.into()), false);
        } else if method {
            self.bind(child, "Self", Binding::Unknown, false);
        }
        for field in ["type_parameters", "parameters"] {
            if let Some(params) = node.child_by_field_name(field) {
                self.pattern(params, child, false);
                if field == "type_parameters" {
                    self.type_refs(params, child, module, "references_type");
                }
            }
        }
        if let Some(ty) = implementation.filter(|t| !t.is_empty())
            && node
                .child_by_field_name("parameters")
                .is_some_and(|p| children(p).iter().any(|n| n.kind() == "self_parameter"))
        {
            // `self` has the impl's declared concrete type.
            self.e.scopes[child]
                .bindings
                .insert("self".into(), Binding::Path(ty.into()));
        }
        if let Some(params) = node.child_by_field_name("parameters") {
            self.type_refs(params, child, module, "parameter_type");
        }
        if let Some(result) = node.child_by_field_name("return_type") {
            self.type_refs(result, child, module, "return_type");
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, child, module, None);
        }
    }
    fn visit(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        module: &str,
        implementation: Option<&str>,
    ) {
        match node.kind() {
            "source_file" | "declaration_list" | "block" => {
                let scope = if node.kind() == "block" {
                    self.e.block(scope, node)
                } else {
                    scope
                };
                let mut conditional = false;
                let mut cfg_only = true;
                for item in children(node) {
                    if matches!(item.kind(), "line_comment" | "block_comment") {
                        continue;
                    }
                    if item.kind() == "attribute_item" {
                        if let Some(attribute) = item.named_child(0).and_then(|n| n.named_child(0))
                        {
                            // Attribute macros and cfg can remove or replace declarations.
                            let changes_item = !matches!(
                                self.e.text(attribute),
                                "allow"
                                    | "warn"
                                    | "deny"
                                    | "forbid"
                                    | "doc"
                                    | "inline"
                                    | "cold"
                                    | "must_use"
                                    | "derive"
                                    | "repr"
                                    | "test"
                                    | "no_mangle"
                                    | "link_name"
                            );
                            conditional |= changes_item;
                            cfg_only &= !changes_item || self.e.text(attribute) == "cfg";
                        }
                        continue;
                    }
                    let start = self.e.facts.nodes.len();
                    let import_start = self.local_uses.len();
                    if conditional && item.kind() == "use_declaration" {
                        // A named conditional import affects only its imported names.
                        // Wildcards still make the whole scope uncertain in use_item.
                        if !cfg_only {
                            // An attribute macro may replace the entire import.
                            self.e.scopes[scope].uncertain = true;
                        }
                        if let Some(arg) = item.child_by_field_name("argument") {
                            self.use_item(
                                arg,
                                scope,
                                module,
                                &[],
                                public(item, self.e.source),
                                true,
                            );
                        }
                    } else {
                        self.visit(item, scope, module, implementation);
                    }
                    if conditional {
                        self.local_uses.truncate(import_start);
                        // An attributed statement can remove/replace local writes.
                        // Keep ordinary extraction intact; reject only value flow.
                        self.e.escape_callable_local(scope, None);
                        if let Some(name) = item.child_by_field_name("name") {
                            let name = self.e.text(name).trim_start_matches("r#");
                            if item.kind() == "mod_item" {
                                self.module_bindings
                                    .entry(scope)
                                    .or_default()
                                    .insert(name.into(), None);
                            } else {
                                if !cfg_only {
                                    // An attribute macro may replace a value declaration with a type.
                                    self.record_binding(scope, name, false);
                                }
                                self.e.invalidate(scope, name);
                            }
                        }
                        for definition in &mut self.e.facts.nodes[start..] {
                            definition.binding_key = None;
                            definition.metadata["conditional"] = true.into();
                        }
                    }
                    conditional = false;
                    cfg_only = true;
                }
                return;
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    self.use_item(arg, scope, module, &[], public(node, self.e.source), false);
                }
                return;
            }
            "function_item" | "function_signature_item" | "closure_expression" => {
                self.function(node, scope, module, implementation);
                return;
            }
            "mod_item" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return;
                };
                let name = self.e.text(name).trim_start_matches("r#");
                let next = format!("{}{name}", prefix(module));
                let key = format!("rust:module:{}:{next}", self.root);
                self.module_bindings
                    .entry(scope)
                    .or_default()
                    .entry(name.into())
                    .and_modify(|key| *key = None)
                    .or_insert_with(|| Some(format!("rust:{}:{next}", self.root)));
                if let Some(body) = node.child_by_field_name("body") {
                    let child = self.e.define(node, scope, name, "module", Some(key), false);
                    self.e.facts.nodes.last_mut().unwrap().metadata["public"] =
                        public(node, self.e.source).into();
                    self.modules.insert(child, next.clone());
                    self.e.scopes[child].fallback =
                        Some(format!("rust:{}:{}", self.root, prefix(&next)));
                    self.visit(body, child, &next, None);
                } else {
                    self.e
                        .define(node, scope, name, "module_declaration", None, false);
                    self.e.reference(
                        node,
                        scope,
                        name.into(),
                        "imports",
                        vec![key],
                        "module file is unavailable or ambiguous",
                    );
                }
                return;
            }
            "impl_item" => {
                let Some(ty) = node.child_by_field_name("type") else {
                    return;
                };
                let target = self.impl_owner(node, ty);
                let eligible = target.is_some();
                let generic_arity = eligible
                    .then(|| self.plain_parameters(node))
                    .flatten()
                    .map(|p| p.len());
                let marker = format!("rust:impl:{}:{}", self.e.facts.path, node.start_byte());
                let start = self.e.facts.nodes.len();
                let name = format!("impl {}", self.e.text(ty));
                let child = self.e.define(node, scope, &name, "impl", None, false);
                // Impl generics are lexical types even though method names are not lexical bindings.
                self.e.scopes[child].class = false;
                if let Some(params) = node.child_by_field_name("type_parameters") {
                    self.pattern(params, child, false);
                }
                self.type_refs(ty, child, module, "impl_type");
                if let Some(trait_type) = node.child_by_field_name("trait") {
                    self.type_refs(trait_type, child, module, "implements");
                }
                self.implementations.push((
                    marker.clone(),
                    child,
                    target.unwrap_or_default(),
                    module.into(),
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    self.visit(
                        body,
                        child,
                        module,
                        Some(if eligible { &marker } else { "" }),
                    );
                }
                if let Some(arity) = generic_arity {
                    for node in &mut self.e.facts.nodes[start..] {
                        node.metadata["generic_impl_type"] = marker.clone().into();
                        node.metadata["generic_impl_arity"] = arity.into();
                    }
                }
                return;
            }
            "struct_item" | "enum_item" | "trait_item" | "type_item" | "union_item" => {
                let Some(n) = node.child_by_field_name("name") else {
                    return;
                };
                let name = self.e.text(n).trim_start_matches("r#");
                let kind = match node.kind() {
                    "struct_item" => "struct",
                    "enum_item" => "enum",
                    "trait_item" => "trait",
                    "union_item" => "union",
                    _ => "type",
                };
                let key = if self.modules.contains_key(&scope) {
                    format!("rust:{}:{}{name}", self.root, prefix(module))
                } else {
                    self.e.local_key(scope, name)
                };
                let child = self
                    .e
                    .define(node, scope, name, kind, Some(key.clone()), false);
                self.e.facts.nodes.last_mut().unwrap().metadata["public"] =
                    public(node, self.e.source).into();
                if matches!(kind, "struct" | "enum" | "union")
                    && let Some(params) = self.plain_parameters(node)
                {
                    self.e.facts.nodes.last_mut().unwrap().metadata["generic_type_arity"] =
                        params.len().into();
                }
                self.bind(scope, name, Binding::Path(key.clone()), false);
                self.e.scopes[child].class = false;
                self.bind(child, "Self", Binding::Path(key), false);
                if let Some(params) = node.child_by_field_name("type_parameters") {
                    self.pattern(params, child, false);
                }
                if let Some(bounds) = node.child_by_field_name("bounds") {
                    self.type_refs(bounds, child, module, "inherits");
                }
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, child, module, "references_type");
                }
                if node.kind() != "trait_item"
                    && let Some(body) = node.child_by_field_name("body")
                {
                    self.type_refs(body, child, module, "field_type");
                }
                if node.kind() == "trait_item"
                    && let Some(body) = node.child_by_field_name("body")
                {
                    self.visit(body, child, module, Some(""));
                }
                return;
            }
            "const_item" | "static_item" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return;
                };
                let name = self.e.text(name).trim_start_matches("r#");
                let key = if name == "_" {
                    None
                } else if let Some(ty) = implementation {
                    (!ty.is_empty()).then(|| format!("{ty}::{name}"))
                } else if self.modules.contains_key(&scope) {
                    Some(format!("rust:{}:{}{name}", self.root, prefix(module)))
                } else {
                    Some(self.e.local_key(scope, name))
                };
                let child = self.e.define(
                    node,
                    scope,
                    name,
                    if node.kind() == "const_item" {
                        "constant"
                    } else {
                        "static"
                    },
                    key,
                    false,
                );
                let item = self.e.facts.nodes.last_mut().unwrap();
                item.metadata["public"] = public(node, self.e.source).into();
                item.metadata["mutable"] = children(node)
                    .iter()
                    .any(|n| n.kind() == "mutable_specifier")
                    .into();
                if let Some(ty) = implementation.filter(|t| !t.is_empty()) {
                    item.metadata["impl_type"] = ty.into();
                    self.bind(child, "Self", Binding::Path(ty.into()), false);
                }
                // Keep the declaration navigable without evaluating its stored value.
                if name != "_" {
                    self.bind(scope, name, Binding::Unknown, true);
                }
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, child, module, "references_type");
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.visit(value, child, module, None);
                }
                return;
            }
            "let_declaration" => {
                if let Some(name) = node
                    .child_by_field_name("pattern")
                    .filter(|n| n.kind() == "identifier")
                    && node
                        .child_by_field_name("value")
                        .is_some_and(|n| n.kind() == "identifier")
                    && node.child_by_field_name("alternative").is_none()
                {
                    self.e
                        .declare_callable_local(scope, self.e.text(name).trim_start_matches("r#"));
                }
                self.pattern(node, scope, false);
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, scope, module, "references_type");
                }
            }
            "type_arguments" => {
                self.type_refs(node, scope, module, "type_argument");
                return;
            }
            "assignment_expression" | "compound_assignment_expr" => {
                if let Some(n) = node.child_by_field_name("left") {
                    self.pattern(n, scope, true);
                }
            }
            "call_expression" => {
                if let Some(target) = node.child_by_field_name("function") {
                    let parts = self.path(target);
                    if parts.as_ref().is_some_and(|p| {
                        p.first().is_some_and(|s| {
                            matches!(s.as_str(), "crate" | "super")
                                || s == "self" && self.e.text(target).starts_with("self::")
                        })
                    }) {
                        let keys = self
                            .absolute(parts.as_ref().unwrap(), module, false)
                            .into_iter()
                            .collect();
                        self.e.reference(
                            node,
                            scope,
                            self.e.text(target).into(),
                            "calls",
                            keys,
                            "static path is unavailable or ambiguous",
                        );
                    } else {
                        let mut path = target;
                        while matches!(path.kind(), "generic_function" | "parenthesized_expression")
                        {
                            let Some(inner) = path
                                .child_by_field_name("function")
                                .or_else(|| path.named_child(0))
                            else {
                                break;
                            };
                            path = inner;
                        }
                        if path.kind() == "scoped_identifier"
                            && let Some(parts) = parts
                        {
                            let index = self.e.facts.references.len();
                            self.e.reference(
                                node,
                                scope,
                                self.e.text(target).into(),
                                "calls",
                                vec![],
                                "static path is unavailable or ambiguous",
                            );
                            self.paths.push((index, scope, parts, module.into()));
                        } else if target.kind() == "identifier"
                            && Self::callable_sequence(node)
                            && node.child_by_field_name("arguments").is_some_and(|args| {
                                children(args)
                                    .iter()
                                    .all(|n| matches!(n.kind(), "line_comment" | "block_comment"))
                            })
                        {
                            self.e.call_with_callable_local(node, scope, target, parts);
                        } else {
                            self.e.call(node, scope, target, parts);
                        }
                    }
                }
            }
            "macro_invocation" => {
                self.e.escape_callable_local(scope, None);
                self.e.reference(
                    node,
                    scope,
                    self.e.text(node).into(),
                    "calls",
                    vec![],
                    "macro expansion is not executed",
                );
                return;
            }
            "reference_expression" => {
                let parts = node.child_by_field_name("value").and_then(|n| self.path(n));
                self.e.escape_callable_local(
                    scope,
                    parts.as_ref().and_then(|p| p.first()).map(String::as_str),
                );
            }
            "macro_definition" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.e
                        .define(node, scope, self.e.text(name), "macro", None, false);
                }
                return;
            }
            "for_expression" | "match_arm" | "if_expression" | "while_expression" => {
                let child = self.e.block(scope, node);
                if let Some(p) = node.child_by_field_name("pattern") {
                    self.pattern(p, child, false);
                }
                if let Some(condition) = node
                    .child_by_field_name("condition")
                    .filter(|n| n.kind() == "let_condition")
                    && let Some(p) = condition.child_by_field_name("pattern")
                {
                    self.pattern(p, child, false);
                }
                for n in children(node) {
                    self.visit(n, child, module, implementation);
                }
                return;
            }
            "attribute_item" | "inner_attribute_item" | "type_parameters" => return,
            _ => {}
        }
        for n in children(node) {
            self.visit(n, scope, module, implementation);
        }
        if matches!(node.kind(), "let_declaration" | "assignment_expression") {
            let declaration = node.kind() == "let_declaration";
            if let Some(name) = node
                .child_by_field_name(if declaration { "pattern" } else { "left" })
                .filter(|n| n.kind() == "identifier")
            {
                let rhs = node
                    .child_by_field_name(if declaration { "value" } else { "right" })
                    .filter(|n| n.kind() == "identifier" && Self::callable_sequence(node))
                    .map(|n| self.e.text(n).trim_start_matches("r#"));
                self.e.assign_callable_local(
                    scope,
                    self.e.text(name).trim_start_matches("r#"),
                    rhs,
                );
            }
        }
    }
}
