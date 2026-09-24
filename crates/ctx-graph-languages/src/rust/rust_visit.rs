use super::*;

impl Rust<'_> {
    pub(super) fn visit(
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
