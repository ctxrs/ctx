use super::*;

impl Javascript<'_> {
    pub(super) fn visit(&mut self, node: Syntax<'_>, scope: usize) {
        match node.kind() {
            "internal_module" | "module" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = self.e.text(name_node).to_owned();
                    let ambient = name_node.kind() == "string"
                        || node
                            .parent()
                            .is_some_and(|p| p.kind() == "ambient_declaration");
                    let key = self.key(scope, &name);
                    let child = self.e.define(
                        node,
                        scope,
                        name.trim_matches(['\'', '"']),
                        "namespace",
                        (!ambient).then(|| key.clone()),
                        false,
                    );
                    if !ambient {
                        let parts = self
                            .type_path(name_node)
                            .unwrap_or_else(|| vec![name.clone()]);
                        let root = parts.first().unwrap();
                        let root_key = self.key(scope, root);
                        let binding = Binding::Namespace {
                            prefixes: vec![format!("{}.", self.exact_key(&root_key))],
                            separator: ".",
                        };
                        self.e.bind(scope, root, binding.clone());
                        self.bind_type(scope, root, binding);
                    } else {
                        self.e.scopes[child].uncertain = true;
                    }
                    if let Some(body) = node.child_by_field_name("body") {
                        let exported = children(body)
                            .into_iter()
                            .filter(|n| n.kind() == "export_statement")
                            .filter_map(|n| n.child_by_field_name("declaration"))
                            .flat_map(|n| {
                                if matches!(
                                    n.kind(),
                                    "lexical_declaration" | "variable_declaration"
                                ) {
                                    children(n)
                                } else {
                                    vec![n]
                                }
                            })
                            .filter_map(|n| n.child_by_field_name("name"))
                            .map(|n| identifier(self.e.text(n)))
                            .collect();
                        self.namespaces.insert(child, (key, exported));
                        for n in children(body) {
                            self.visit(n, child);
                        }
                    }
                }
                return;
            }
            "function_signature" | "method_signature" | "abstract_method_signature" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let child = self.e.define(
                        node,
                        scope,
                        self.e.text(name),
                        if node.kind() == "function_signature" {
                            "function"
                        } else {
                            "method"
                        },
                        None,
                        false,
                    );
                    self.e.facts.nodes.last_mut().unwrap().metadata["interface_signature"] =
                        (node.kind() == "method_signature"
                            && name.kind() == "property_identifier"
                            && !token(node, "static")
                            && !token(node, "?")
                            && !token(node, "get")
                            && !token(node, "set")
                            && node.parent().is_some_and(|body| {
                                children(body)
                                    .iter()
                                    .filter(|member| {
                                        member.child_by_field_name("name").is_some_and(|other| {
                                            identifier(self.e.text(other))
                                                == identifier(self.e.text(name))
                                        })
                                    })
                                    .count()
                                    == 1
                            }))
                        .into();
                    self.type_parameters(node, child);
                    if let Some(params) = node.child_by_field_name("parameters") {
                        self.type_refs(params, child, "parameter_type");
                    }
                    if let Some(result) = node.child_by_field_name("return_type") {
                        self.type_refs(result, child, "return_type");
                    }
                }
                return;
            }
            "import_statement" => {
                self.import(node, scope);
                return;
            }
            "export_statement" => {
                if let Some(source) = node.child_by_field_name("source") {
                    if token(node, "*")
                        && !token(node, "type")
                        && !children(node)
                            .iter()
                            .any(|n| n.kind() == "namespace_export")
                        && let Some(module) = self.string(source)
                    {
                        if !self.e.facts.nodes[0].metadata["star_reexports"].is_array() {
                            self.e.facts.nodes[0].metadata["star_reexports"] =
                                serde_json::json!([]);
                        }
                        self.e.facts.nodes[0].metadata["star_reexports"]
                            .as_array_mut()
                            .unwrap()
                            .push(module.into());
                    }
                    self.e.reference(
                        node,
                        scope,
                        self.e.text(source).into(),
                        "imports",
                        self.string(source)
                            .map(|s| {
                                self.modules(&s)
                                    .into_iter()
                                    .map(|m| module_key(&m))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        "re-export forwarding is not resolved",
                    );
                    return;
                }
                if token(node, "default")
                    && let Some(value) = node
                        .child_by_field_name("value")
                        .or_else(|| node.child_by_field_name("declaration"))
                    && matches!(
                        value.kind(),
                        "function_expression" | "arrow_function" | "generator_function"
                    )
                    && value.child_by_field_name("name").is_none()
                {
                    self.exports
                        .insert("default".into(), vec!["default".into()]);
                    self.function(value, scope, Some("default"));
                    return;
                }
            }
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition" => {
                self.function(node, scope, None);
                return;
            }
            "class_declaration"
            | "class"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => {
                let name =
                    node.child_by_field_name("name")
                        .map(|n| self.e.text(n).to_owned())
                        .unwrap_or_else(|| {
                            if node.parent().is_some_and(|p| {
                                p.kind() == "export_statement" && token(p, "default")
                            }) {
                                "default".into()
                            } else {
                                format!("<class@{}>", node.start_byte())
                            }
                        });
                if name == "default" {
                    self.exports.insert(name.clone(), vec![name.clone()]);
                }
                let kind = match node.kind() {
                    "interface_declaration" => "interface",
                    "type_alias_declaration" => "type",
                    "enum_declaration" => "enum",
                    _ => "class",
                };
                let type_only = matches!(kind, "interface" | "type");
                let key = self.key(scope, &name);
                let child = self
                    .e
                    .define(node, scope, &name, kind, Some(key.clone()), !type_only);
                self.bind_type(scope, &name, Binding::symbol(self.exact_key(&key)));
                self.type_parameters(node, child);
                if kind == "class" {
                    self.classes.insert(
                        child,
                        ClassMembers {
                            id: self.e.scopes[child].owner.clone(),
                            key,
                            dynamic_members: node.child_by_field_name("decorator").is_some()
                                || node.child_by_field_name("body").is_some_and(|body| {
                                    children(body).iter().any(|member| {
                                        member.kind() == "decorator"
                                            || member.child_by_field_name("decorator").is_some()
                                            || member.child_by_field_name("name").is_some_and(
                                                |name| name.kind() == "computed_property_name",
                                            )
                                    })
                                }),
                            methods: HashMap::new(),
                            fields: HashMap::new(),
                        },
                    );
                    if let Some(body) = node.child_by_field_name("body") {
                        self.class_fields(child, body);
                    }
                }
                for n in children(node) {
                    if Some(n) == node.child_by_field_name("body") {
                        if kind != "type" {
                            self.visit(n, child);
                        }
                    } else if Some(n) == node.child_by_field_name("value") {
                        self.type_refs(n, child, "references_type");
                    } else if n.kind() == "extends_type_clause" {
                        self.type_refs(n, child, "inherits");
                    } else if n.kind() == "class_heritage" {
                        for clause in children(n) {
                            let relation = if clause.kind() == "implements_clause" {
                                "implements"
                            } else {
                                "inherits"
                            };
                            // JavaScript has a direct heritage expression; TypeScript
                            // wraps bases in extends/implements clauses.
                            let bases = if matches!(
                                clause.kind(),
                                "extends_clause" | "implements_clause"
                            ) {
                                children(clause)
                            } else {
                                vec![clause]
                            };
                            for base in bases {
                                if let Some(parts) = self.type_path(base) {
                                    let index = self.e.facts.references.len();
                                    self.e.reference(
                                        base,
                                        child,
                                        self.e.text(base).into(),
                                        relation,
                                        vec![],
                                        "explicit heritage type is unavailable or ambiguous",
                                    );
                                    self.type_references.push((index, child, parts));
                                    if let Some(args) = base.child_by_field_name("type_arguments") {
                                        self.type_refs(args, child, "type_argument");
                                    }
                                } else if base.kind() == "type_arguments" {
                                    self.type_refs(base, child, "type_argument");
                                } else {
                                    self.visit(base, scope);
                                }
                            }
                        }
                    } else if n.kind() == "decorator" {
                        self.visit(n, scope);
                    }
                }
                return;
            }
            "variable_declaration" => {
                let mut function_scope = scope;
                while !self.e.scopes[function_scope].function {
                    function_scope = self.e.scopes[function_scope].parent.unwrap_or(0);
                }
                for var in children(node) {
                    self.declare_callable(var, function_scope);
                    if let (Some(name), Some(value)) = (
                        var.child_by_field_name("name"),
                        var.child_by_field_name("value"),
                    ) && self.require_declaration(name, value, function_scope, scope)
                    {
                        self.visit(value, scope);
                        continue;
                    }
                    if let Some(name) = var.child_by_field_name("name")
                        && !self.typed_binding(
                            name,
                            var.child_by_field_name("type"),
                            var.child_by_field_name("value"),
                            function_scope,
                            scope,
                        )
                    {
                        self.pattern(name, function_scope, false);
                    }
                    if let Some(value) = var.child_by_field_name("value") {
                        self.visit(value, scope);
                    }
                    self.assign_callable(var, scope);
                }
                return;
            }
            "variable_declarator" => {
                self.declare_callable(node, scope);
                if let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) && self.require_declaration(name, value, scope, scope)
                {
                    self.visit(value, scope);
                    return;
                }
                if let Some(name) = node.child_by_field_name("name") {
                    if let Some(value) = node.child_by_field_name("value")
                        && name.kind() == "identifier"
                        && matches!(
                            value.kind(),
                            "arrow_function" | "function_expression" | "generator_function"
                        )
                    {
                        self.function(value, scope, Some(self.e.text(name)));
                        return;
                    }
                    if !self.factory_binding(node, scope)
                        && !self.typed_binding(
                            name,
                            node.child_by_field_name("type"),
                            node.child_by_field_name("value"),
                            scope,
                            scope,
                        )
                    {
                        self.pattern(name, scope, false);
                    }
                }
            }
            "assignment_expression" | "augmented_assignment_expression" => {
                if self.commonjs_export(node, scope) {
                    return;
                }
                if let Some(n) = node.child_by_field_name("left") {
                    self.pattern(n, scope, true);
                }
            }
            "update_expression" => {
                if let Some(n) = node.child_by_field_name("argument") {
                    self.pattern(n, scope, true);
                }
            }
            "call_expression" | "new_expression" => {
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    for argument in children(arguments)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                    {
                        self.callback_arguments.insert(format!(
                            "call:{}:{}-{}",
                            self.e.scopes[scope].owner,
                            argument.start_byte(),
                            argument.end_byte()
                        ));
                        // Reuse the final lexical/write resolver without treating
                        // the argument as an invocation in the returned facts.
                        self.e.call(
                            argument,
                            scope,
                            argument,
                            Some(vec![self.e.text(argument).into()]),
                        );
                    }
                }
                if let Some(target) = node
                    .child_by_field_name("function")
                    .or_else(|| node.child_by_field_name("constructor"))
                {
                    let parts = self.dotted(target);
                    if let Some((_, _, module)) = self
                        .require_target(node)
                        .filter(|_| target.kind() == "identifier")
                    {
                        let index = self.e.facts.references.len();
                        let keys = module
                            .as_deref()
                            .map(|m| self.modules(m).iter().map(|m| module_key(m)).collect())
                            .unwrap_or_default();
                        self.e.reference(
                            node,
                            scope,
                            module.unwrap_or_else(|| self.e.text(node).into()),
                            "imports",
                            keys,
                            "CommonJS require target is unavailable or dynamic",
                        );
                        self.require_references.push((scope, index));
                    }
                    if let Some((_, imported, module)) = self.require_target(target) {
                        let index = self.e.facts.references.len();
                        let symbol = imported.as_deref().unwrap_or("default");
                        let keys = module
                            .as_deref()
                            .map(|m| {
                                self.modules(m)
                                    .iter()
                                    .map(|m| commonjs_key(m, symbol))
                                    .collect()
                            })
                            .unwrap_or_default();
                        self.e.reference(
                            node,
                            scope,
                            self.e.text(target).into(),
                            "calls",
                            keys,
                            "CommonJS target is unavailable or dynamic",
                        );
                        self.require_references.push((scope, index));
                    } else {
                        self.call(node, scope, target, parts.clone());
                    }
                    if target.kind() == "import" {
                        let argument = node
                            .child_by_field_name("arguments")
                            .and_then(|n| n.named_child(0));
                        let module = argument
                            .filter(|n| n.kind() == "string")
                            .and_then(|n| self.string(n));
                        self.e.reference(
                            node,
                            scope,
                            module.clone().unwrap_or_else(|| self.e.text(node).into()),
                            "imports",
                            module
                                .map(|m| self.modules(&m).iter().map(|m| module_key(m)).collect())
                                .unwrap_or_default(),
                            "dynamic import path is unavailable or not a static string",
                        );
                    }
                    if parts
                        .as_ref()
                        .is_some_and(|p| p.len() == 1 && p[0] == "eval")
                    {
                        let mut ancestor = Some(scope);
                        while let Some(i) = ancestor {
                            self.e.scopes[i].uncertain = true;
                            ancestor = self.e.scopes[i].parent;
                        }
                    }
                }
            }
            "statement_block" | "for_statement" | "for_in_statement" | "catch_clause"
            | "switch_statement" => {
                let child = self.e.block(scope, node);
                if let Some(p) = node.child_by_field_name("parameter") {
                    self.pattern(p, child, false);
                }
                if node.kind() == "for_in_statement"
                    && let Some(p) = node.child_by_field_name("left")
                {
                    let kind = node.child_by_field_name("kind").map(|n| n.kind());
                    let mut binding_scope = child;
                    if kind == Some("var") {
                        while !self.e.scopes[binding_scope].function {
                            binding_scope = self.e.scopes[binding_scope].parent.unwrap_or(0);
                        }
                    }
                    self.pattern(p, binding_scope, kind.is_none());
                }
                for n in children(node) {
                    self.visit(n, child);
                }
                return;
            }
            "with_statement" => {
                let child = self.e.block(scope, node);
                self.e.scopes[child].uncertain = true;
                for n in children(node) {
                    self.visit(n, child);
                }
                return;
            }
            "type_annotation" | "type_arguments" => {
                self.type_refs(node, scope, "references_type");
                return;
            }
            "type_parameters" | "export_clause" => return,
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if let Some(name) = node.child_by_field_name("name")
                    && let Some(parts) = self.dotted(name)
                    && (parts.len() > 1 || parts[0].chars().next().is_some_and(char::is_uppercase))
                {
                    self.component_references.insert(format!(
                        "call:{}:{}-{}",
                        self.e.scopes[scope].owner,
                        node.start_byte(),
                        node.end_byte()
                    ));
                    self.call(node, scope, name, Some(parts));
                }
            }
            _ => {}
        }
        for n in children(node) {
            self.visit(n, scope);
        }
        if matches!(node.kind(), "variable_declarator" | "assignment_expression") {
            self.assign_callable(node, scope);
        }
    }
}
