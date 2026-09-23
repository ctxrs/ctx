use super::common::*;
use crate::model::FileFacts;
use anyhow::Result;
use std::collections::HashMap;
use tree_sitter::Node as Syntax;

pub(super) fn parse(path: &str, source: &str, hash: &str) -> Result<FileFacts> {
    let directory = path.rsplit_once('/').map_or(".", |(dir, _)| dir);
    let mut e = Extractor::new(path, source, hash, "go", directory.into());
    let Some(tree) = tree(tree_sitter_go::LANGUAGE.into(), source, &mut e.facts)? else {
        return Ok(e.facts);
    };
    let root = tree.root_node();
    let package = children(root)
        .into_iter()
        .find(|n| n.kind() == "package_clause")
        .and_then(|n| n.named_child(0))
        .map(|n| e.text(n).to_owned())
        .unwrap_or_default();
    e.root(root, format!("go:file:{path}"));
    e.facts.nodes[0].metadata["package_name"] = package.clone().into();
    let prefix = format!("go:{directory}:{package}:");
    e.scopes[0].fallback = Some(prefix.clone());
    let mut go = Go {
        e,
        prefix,
        types: vec![],
        receivers: vec![],
    };
    go.visit(root, 0);
    for (index, scope, parts) in &go.types {
        go.e.facts.references[*index].candidate_keys = go.type_keys(*scope, parts);
    }
    let receivers: HashMap<_, _> = go
        .receivers
        .iter()
        .map(|(marker, scope, parts, pointer)| {
            (
                marker.trim_end_matches(':').to_owned(),
                (go.type_keys(*scope, parts), *pointer),
            )
        })
        .collect();
    let mut facts = go.e.finish();
    let mut declarations = vec![];
    for reference in &mut facts.references {
        let mut declared_keys = vec![];
        reference.candidate_keys = reference
            .candidate_keys
            .iter()
            .flat_map(|key| {
                if let Some((marker, member)) = key.rsplit_once(':')
                    && let Some((types, pointer)) = receivers.get(marker)
                {
                    if reference.relation == "calls" && !pointer && !member.contains('.') {
                        declared_keys
                            .extend(types.iter().map(|ty| format!("{ty}#declared.{member}")));
                    }
                    types.iter().map(|ty| format!("{ty}.{member}")).collect()
                } else {
                    vec![key.clone()]
                }
            })
            .collect();
        if !declared_keys.is_empty() {
            let mut declaration = reference.clone();
            declaration.id.push_str(":declared_member");
            declaration.relation = "declared_member".into();
            declaration.candidate_keys = declared_keys;
            declaration.reason = "written interface member; runtime dispatch is unresolved".into();
            declarations.push(declaration);
        }
    }
    facts.references.extend(declarations);
    // Interface signatures have declaration-only aliases, never callable keys.
    let owners: HashMap<_, _> = facts
        .nodes
        .iter()
        .filter(|n| n.kind == "interface")
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
            && let Some(owner) = parents.get(&node.id).and_then(|id| owners.get(id))
        {
            node.metadata["binding_aliases"] =
                serde_json::json!([format!("{owner}#declared.{}", node.label)]);
        }
    }
    Ok(facts)
}
struct Go<'a> {
    e: Extractor<'a>,
    prefix: String,
    types: Vec<(usize, usize, Vec<String>)>,
    receivers: Vec<(String, usize, Vec<String>, bool)>,
}
impl Go<'_> {
    fn callable_sequence(mut node: Syntax<'_>) -> bool {
        while let Some(parent) = node.parent() {
            match parent.kind() {
                "block"
                | "statement_list"
                | "expression_statement"
                | "return_statement"
                | "var_declaration"
                | "var_spec_list" => {}
                "expression_list" if children(parent).len() == 1 => {}
                "function_declaration" | "method_declaration" | "func_literal" => {
                    return parent.child_by_field_name("body") == Some(node);
                }
                _ => return false,
            }
            node = parent;
        }
        false
    }
    fn callable_pair(node: Syntax<'_>) -> Option<(Syntax<'_>, Syntax<'_>)> {
        let (names, value) = if node.kind() == "var_spec" {
            let mut cursor = node.walk();
            (
                node.children_by_field_name("name", &mut cursor)
                    .collect::<Vec<_>>(),
                node.child_by_field_name("value")?,
            )
        } else {
            if node.kind() == "assignment_statement"
                && node
                    .child_by_field_name("operator")
                    .is_none_or(|n| n.kind() != "=")
            {
                return None;
            }
            (
                children(node.child_by_field_name("left")?),
                node.child_by_field_name("right")?,
            )
        };
        let values = children(value);
        match (names.as_slice(), values.as_slice()) {
            ([name], [value]) if name.kind() == "identifier" && value.kind() == "identifier" => {
                Some((*name, *value))
            }
            _ => None,
        }
    }
    fn receiver_binding(binding: &Binding) -> bool {
        matches!(binding, Binding::Namespace { prefixes, .. } if prefixes.iter().all(|p| p.starts_with("go:receiver:")))
    }
    fn type_keys(&self, mut scope: usize, parts: &[String]) -> Vec<String> {
        let Some(name) = parts.first() else {
            return vec![];
        };
        loop {
            let environment = &self.e.scopes[scope];
            if environment.uncertain {
                return vec![];
            }
            if environment
                .bindings
                .get(name)
                .is_some_and(|b| !Self::receiver_binding(b))
                || environment.fallback.is_some()
            {
                return self.e.resolve(scope, parts);
            }
            let Some(parent) = environment.parent else {
                return vec![];
            };
            scope = parent;
        }
    }
    fn type_refs(&mut self, node: Syntax<'_>, scope: usize, relation: &str) {
        if matches!(node.kind(), "type_identifier" | "qualified_type") {
            let Some(parts) = self.dotted(node) else {
                return;
            };
            if parts.len() == 1
                && matches!(
                    parts[0].as_str(),
                    "any"
                        | "comparable"
                        | "bool"
                        | "byte"
                        | "complex64"
                        | "complex128"
                        | "error"
                        | "float32"
                        | "float64"
                        | "int"
                        | "int8"
                        | "int16"
                        | "int32"
                        | "int64"
                        | "rune"
                        | "string"
                        | "uint"
                        | "uint8"
                        | "uint16"
                        | "uint32"
                        | "uint64"
                        | "uintptr"
                )
                && self.e.unbound(scope, &parts[0])
            {
                return;
            }
            let index = self.e.facts.references.len();
            self.e.reference(
                node,
                scope,
                self.e.text(node).into(),
                relation,
                vec![],
                "explicit Go type is unavailable or ambiguous",
            );
            self.types.push((index, scope, parts));
            return;
        }
        for n in children(node) {
            if n.kind() != "field_identifier" {
                self.type_refs(
                    n,
                    scope,
                    if node.kind() == "type_arguments" {
                        "type_argument"
                    } else {
                        relation
                    },
                );
            }
        }
    }
    fn typed_name(&mut self, name: Syntax<'_>, ty: Option<Syntax<'_>>, scope: usize) {
        let parts = ty.and_then(|mut n| {
            if n.kind() == "pointer_type" {
                n = n.named_child(0)?;
            }
            self.dotted(n)
        });
        let keys = parts
            .as_ref()
            .map(|p| self.type_keys(scope, p))
            .unwrap_or_default();
        if name.kind() == "identifier" && self.e.text(name) != "_" && !keys.is_empty() {
            let parts = parts.unwrap();
            // Remember the type's lexical scope before the variable can shadow it.
            let mut owner = scope;
            while !self.e.scopes[owner]
                .bindings
                .get(&parts[0])
                .is_some_and(|b| !Self::receiver_binding(b))
            {
                let Some(parent) = self.e.scopes[owner].parent else {
                    break;
                };
                owner = parent;
            }
            let marker = format!(
                "go:receiver:{}:{scope}:{}:",
                self.e.facts.path,
                name.start_byte()
            );
            self.receivers.push((
                marker.clone(),
                owner,
                parts,
                ty.is_some_and(|n| n.kind() == "pointer_type"),
            ));

            self.e.bind(
                scope,
                self.e.text(name),
                Binding::Namespace {
                    prefixes: vec![marker],
                    separator: ".",
                },
            );
        } else {
            self.pattern(name, scope, false);
        }
    }
    fn pattern(&mut self, node: Syntax<'_>, scope: usize, write: bool) {
        match node.kind() {
            "identifier" => {
                let name = self.e.text(node);
                if name != "_" {
                    if write {
                        self.e.invalidate(scope, name);
                    } else {
                        self.e.bind(scope, name, Binding::Unknown);
                    }
                }
            }
            "expression_list" | "parameter_list" | "type_parameter_list" => {
                for n in children(node) {
                    self.pattern(n, scope, write);
                }
            }
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "type_parameter_declaration" => {
                let mut cursor = node.walk();
                for n in node.children_by_field_name("name", &mut cursor) {
                    if node.kind() == "parameter_declaration" && !write {
                        self.typed_name(n, node.child_by_field_name("type"), scope);
                    } else {
                        self.pattern(n, scope, write);
                    }
                }
            }
            "selector_expression" | "index_expression" if write => {
                if let Some(n) = node.child_by_field_name("operand") {
                    self.pattern(n, scope, true);
                }
            }
            _ => {}
        }
    }
    fn dotted(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        match node.kind() {
            "identifier" | "type_identifier" => Some(vec![self.e.text(node).into()]),
            "qualified_type" => Some(vec![
                self.e.text(node.child_by_field_name("package")?).into(),
                self.e.text(node.child_by_field_name("name")?).into(),
            ]),
            "selector_expression" => {
                let mut parts = self.dotted(node.child_by_field_name("operand")?)?;
                parts.push(self.e.text(node.child_by_field_name("field")?).into());
                Some(parts)
            }
            "parenthesized_expression" => self.dotted(node.named_child(0)?),
            "generic_type" => self.dotted(node.child_by_field_name("type")?),
            _ => None,
        }
    }
    fn import(&mut self, node: Syntax<'_>, scope: usize) {
        let Some(path) = node.child_by_field_name("path") else {
            return;
        };
        let literal = self.e.text(path);
        let path = if literal.starts_with('`') {
            Some(literal[1..literal.len() - 1].to_owned())
        } else {
            serde_json::from_str::<String>(literal).ok()
        };
        let Some(path) = path else {
            self.e.reference(
                node,
                scope,
                literal.into(),
                "imports",
                vec![],
                "import string encoding is unsupported",
            );
            return;
        };
        let explicit = node.child_by_field_name("name");
        let local = explicit
            .map(|n| self.e.text(n))
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(""));
        let key = format!("go:import-module:{path}");
        self.e.reference(
            node,
            scope,
            format!("{path} as {local}"),
            "imports",
            vec![key],
            "import path needs module context or is external",
        );
        if local == "." {
            self.e.scopes[scope].uncertain = true;
        } else if local != "_" && explicit.is_some() {
            self.e.bind(
                scope,
                local,
                Binding::Namespace {
                    prefixes: vec![format!("go:import:{path}:")],
                    separator: ".",
                },
            );
        }
        let owner = &mut self.e.facts.nodes[0];
        if !owner.metadata["imports"].is_array() {
            owner.metadata["imports"] = serde_json::json!([]);
        }
        owner.metadata["imports"].as_array_mut().unwrap().push(serde_json::json!({"path": path, "alias": explicit.map(|n| self.e.source[n.byte_range()].to_owned())}));
        if !owner.metadata["go_imports"].is_array() {
            owner.metadata["go_imports"] = serde_json::json!([]);
        }
        owner.metadata["go_imports"].as_array_mut().unwrap().push(
            serde_json::json!({"path": path, "local": local, "explicit_alias": explicit.is_some()}),
        );
    }
    fn function(&mut self, node: Syntax<'_>, scope: usize) {
        let name = node
            .child_by_field_name("name")
            .map(|n| self.e.text(n).to_owned())
            .unwrap_or_else(|| format!("<anonymous@{}>", node.start_byte()));
        let receiver = node.child_by_field_name("receiver");
        let receiver_type = receiver
            .and_then(|r| r.named_child(0))
            .and_then(|r| r.child_by_field_name("type"))
            .and_then(|mut n| {
                if n.kind() == "pointer_type" {
                    n = n.named_child(0)?;
                }
                if n.kind() == "generic_type" {
                    n = n.child_by_field_name("type")?;
                }
                (n.kind() == "type_identifier").then(|| self.e.text(n).to_owned())
            });
        let anonymous = node.kind() == "func_literal";
        let key = if anonymous || name == "init" {
            None
        } else if let Some(ty) = &receiver_type {
            Some(format!("{}{ty}.{name}", self.prefix))
        } else if scope == 0 {
            Some(format!("{}{name}", self.prefix))
        } else {
            Some(self.e.local_key(scope, &name))
        };
        let child = self.e.define(
            node,
            scope,
            &name,
            if receiver.is_some() {
                "method"
            } else {
                "function"
            },
            key,
            !anonymous && receiver.is_none() && name != "init",
        );
        if let Some(p) = node.child_by_field_name("type_parameters") {
            self.pattern(p, child, false);
            self.type_refs(p, child, "type_constraint");
        }
        if let Some(mut ty) = receiver
            .and_then(|r| r.named_child(0))
            .and_then(|r| r.child_by_field_name("type"))
        {
            if ty.kind() == "pointer_type"
                && let Some(inner) = ty.named_child(0)
            {
                ty = inner;
            }
            if ty.kind() == "generic_type"
                && let Some(args) = ty.child_by_field_name("type_arguments")
            {
                for arg in children(args)
                    .into_iter()
                    .filter(|n| n.kind() == "type_identifier")
                {
                    self.e.bind(child, self.e.text(arg), Binding::Unknown);
                }
            }
        }
        for field in ["parameters", "result", "receiver"] {
            if let Some(p) = node.child_by_field_name(field) {
                self.pattern(p, child, false);
                self.type_refs(
                    p,
                    child,
                    match field {
                        "parameters" => "parameter_type",
                        "result" => "return_type",
                        _ => "receiver_type",
                    },
                );
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.visit(body, child);
        }
    }
    fn visit(&mut self, node: Syntax<'_>, scope: usize) {
        if matches!(node.kind(), "var_spec" | "short_var_declaration")
            && let Some((name, _)) = Self::callable_pair(node)
        {
            self.e.declare_callable_local(scope, self.e.text(name));
        }
        match node.kind() {
            "source_file" => {
                // Imports are file scoped even when written after declarations.
                for declaration in children(node)
                    .into_iter()
                    .filter(|n| n.kind() == "import_declaration")
                {
                    self.visit(declaration, scope);
                }
                for declaration in children(node)
                    .into_iter()
                    .filter(|n| n.kind() != "import_declaration")
                {
                    self.visit(declaration, scope);
                }
                return;
            }
            "import_spec" => {
                self.import(node, scope);
                return;
            }
            "function_declaration" | "method_declaration" | "func_literal" => {
                self.function(node, scope);
                return;
            }
            "type_spec" | "type_alias" => {
                if let Some(n) = node.child_by_field_name("name") {
                    let name = self.e.text(n);
                    let kind = node
                        .child_by_field_name("type")
                        .map(|n| match n.kind() {
                            "struct_type" => "struct",
                            "interface_type" => "interface",
                            _ => "type",
                        })
                        .unwrap_or("type");
                    let key = if scope == 0 {
                        format!("{}{name}", self.prefix)
                    } else {
                        self.e.local_key(scope, name)
                    };
                    let child = self.e.define(node, scope, name, kind, Some(key), true);
                    self.e.scopes[child].class = false;
                    if let Some(params) = node.child_by_field_name("type_parameters") {
                        self.pattern(params, child, false);
                        self.type_refs(params, child, "type_constraint");
                    }
                    if let Some(body) = node.child_by_field_name("type") {
                        if kind == "interface" {
                            for member in children(body) {
                                if member.kind() == "method_elem" {
                                    if let Some(name) = member.child_by_field_name("name") {
                                        let method = self.e.define(
                                            member,
                                            child,
                                            self.e.text(name),
                                            "method",
                                            None,
                                            false,
                                        );
                                        self.type_refs(member, method, "references_type");
                                    }
                                } else if member.kind() == "type_elem" {
                                    let simple = children(member);
                                    let embedded = simple.len() == 1
                                        && matches!(
                                            simple[0].kind(),
                                            "type_identifier" | "qualified_type" | "generic_type"
                                        );
                                    self.type_refs(
                                        member,
                                        child,
                                        if embedded {
                                            "embeds"
                                        } else {
                                            "references_type"
                                        },
                                    );
                                }
                            }
                        } else if kind == "struct" {
                            for field in children(body)
                                .into_iter()
                                .flat_map(children)
                                .filter(|n| n.kind() == "field_declaration")
                            {
                                if let Some(ty) = field.child_by_field_name("type") {
                                    self.type_refs(
                                        ty,
                                        child,
                                        if field.child_by_field_name("name").is_none() {
                                            "embeds"
                                        } else {
                                            "field_type"
                                        },
                                    );
                                }
                            }
                        } else {
                            self.type_refs(body, child, "references_type");
                        }
                    }
                }
                return;
            }
            "var_spec" | "const_spec" => {
                let mut cursor = node.walk();
                for name in node.children_by_field_name("name", &mut cursor) {
                    self.typed_name(name, node.child_by_field_name("type"), scope);
                }
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, scope, "references_type");
                }
            }
            "short_var_declaration" | "range_clause" | "receive_statement" => {
                if let Some(p) = node.child_by_field_name("left") {
                    let mut cursor = node.walk();
                    let write = node.children(&mut cursor).any(|n| n.kind() == "=");
                    self.pattern(p, scope, write);
                }
            }
            "assignment_statement" => {
                if let Some(p) = node.child_by_field_name("left") {
                    self.pattern(p, scope, true);
                }
            }
            "inc_statement" | "dec_statement" => {
                if let Some(p) = node.named_child(0) {
                    self.pattern(p, scope, true);
                }
            }
            "call_expression" => {
                if let Some(target) = node.child_by_field_name("function") {
                    if target.kind() == "identifier"
                        && Self::callable_sequence(node)
                        && node.child_by_field_name("arguments").is_some_and(|args| {
                            children(args).iter().all(|n| n.kind() == "comment")
                        })
                    {
                        self.e
                            .call_with_callable_local(node, scope, target, self.dotted(target));
                    } else {
                        self.e.call(node, scope, target, self.dotted(target));
                    }
                }
            }
            "unary_expression"
                if node
                    .child_by_field_name("operator")
                    .is_some_and(|n| n.kind() == "&") =>
            {
                let parts = node
                    .child_by_field_name("operand")
                    .and_then(|n| self.dotted(n));
                self.e.escape_callable_local(
                    scope,
                    parts.as_ref().and_then(|p| p.first()).map(String::as_str),
                );
            }
            "block"
            | "for_statement"
            | "if_statement"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "expression_case"
            | "type_case"
            | "communication_case" => {
                let child = self.e.block(scope, node);
                if node.kind() == "type_switch_statement"
                    && let Some(alias) = node.child_by_field_name("alias")
                {
                    self.pattern(alias, child, false);
                }
                for n in children(node) {
                    self.visit(n, child);
                }
                return;
            }
            _ => {}
        }
        for n in children(node) {
            self.visit(n, scope);
        }
        if matches!(
            node.kind(),
            "var_spec" | "short_var_declaration" | "assignment_statement"
        ) && let Some((name, rhs)) = Self::callable_pair(node)
        {
            self.e.assign_callable_local(
                scope,
                self.e.text(name),
                Self::callable_sequence(node).then(|| self.e.text(rhs)),
            );
        }
    }
}
