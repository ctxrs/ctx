use super::*;

impl<'s, 't> Compiled<'s, 't> {
    pub(super) fn visit(&mut self, n: Syntax<'t>, mut scope: usize) {
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
    pub(super) fn modifier(&self, n: Syntax<'_>, word: &str) -> bool {
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
    pub(super) fn function(&mut self, n: Syntax<'t>, scope: usize) {
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
    pub(super) fn type_evidence(&mut self, n: Syntax<'t>, scope: usize, context: &'static str) {
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
    pub(super) fn property(&mut self, n: Syntax<'t>, scope: usize) -> bool {
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
}
