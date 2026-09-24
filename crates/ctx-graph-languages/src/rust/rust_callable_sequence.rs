use super::*;

impl Rust<'_> {
    pub(super) fn callable_sequence(mut node: Syntax<'_>) -> bool {
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
    pub(super) fn record_binding(&mut self, scope: usize, name: &str, value_only: bool) {
        self.value_bindings
            .entry(scope)
            .or_default()
            .entry(name.trim_start_matches("r#").into())
            .and_modify(|known| *known &= value_only)
            .or_insert(value_only);
    }
    pub(super) fn bind(&mut self, scope: usize, name: &str, binding: Binding, value_only: bool) {
        let name = name.trim_start_matches("r#");
        self.record_binding(scope, name, value_only);
        self.e.bind(scope, name, binding);
    }
    pub(super) fn resolve_path(&self, scope: usize, parts: &[String], module: &str) -> Vec<String> {
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
    pub(super) fn module_candidates(&self, scope: usize, parts: &[String]) -> Option<Vec<String>> {
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
    pub(super) fn local_use_origin(&self, import: &LocalUse) -> Option<(Vec<String>, bool)> {
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
    pub(super) fn type_refs(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        module: &str,
        relation: &str,
    ) {
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
    pub(super) fn plain_parameters<'n>(&self, node: Syntax<'n>) -> Option<Vec<Syntax<'n>>> {
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
    pub(super) fn impl_owner(&self, node: Syntax<'_>, ty: Syntax<'_>) -> Option<Vec<String>> {
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
    pub(super) fn finish(mut self) -> FileFacts {
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
    pub(super) fn path(&self, node: Syntax<'_>) -> Option<Vec<String>> {
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
    pub(super) fn absolute(
        &self,
        parts: &[String],
        module: &str,
        use_path: bool,
    ) -> Option<String> {
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
    pub(super) fn pattern(&mut self, node: Syntax<'_>, scope: usize, write: bool) {
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
    pub(super) fn use_item(
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
    pub(super) fn function(
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
}
