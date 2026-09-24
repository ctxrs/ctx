use super::*;

impl<'s, 't> Extended<'s, 't> {
    pub(super) fn visit(&mut self, n: Syntax<'t>, scope: usize) {
        let k = n.kind();
        let lang = self.e.language;
        if matches!(
            k,
            "comment"
                | "line_comment"
                | "block_comment"
                | "string"
                | "string_literal"
                | "str_lit"
                | "syn_quoting_lit"
                | "quote_expression"
                | "quoting_lit"
                | "quasiquoting_lit"
                | "dis_expr"
        ) {
            return;
        }
        if lang == "objc" && k == "declaration" {
            let mut cursor = n.walk();
            for declarator in n.children_by_field_name("declarator", &mut cursor) {
                self.mask(declarator, scope);
            }
        }
        if lang == "pascal" && matches!(k, "declVar" | "declField" | "declProp") {
            self.mask(n, scope);
        }
        if lang == "pascal" && k == "statement" {
            let expressions: Vec<_> = children(n)
                .into_iter()
                .filter(|c| !c.kind().starts_with('k') && c.kind() != "comment")
                .collect();
            if let [target] = expressions.as_slice()
                && matches!(target.kind(), "identifier" | "inherited")
            {
                self.pascal_call(n, scope, *target);
                return;
            }
        }
        if lang == "commonlisp" {
            self.lisp(n, scope);
            return;
        }
        if lang == "fortran" && self.fortran_directive(n, scope) {
            return;
        }
        if matches!(lang, "apex" | "dart")
            && matches!(
                k,
                "block" | "for_statement" | "enhanced_for_statement" | "catch_clause"
            )
        {
            let s = self.e.block(scope, n);
            self.prefixes
                .push(format!("{}.<{}>", self.prefixes[scope], n.start_byte()));
            if matches!(k, "enhanced_for_statement" | "catch_clause") {
                self.parameter_types(n, s);
            }
            self.walk(n, s);
            return;
        }
        if matches!(lang, "dart" | "apex") && self.typed_declaration(n, scope) {
            return;
        }
        if matches!(
            k,
            "function_expression" | "arrow_function_expression" | "fun_expression" | "lambda"
        ) {
            let s = self.e.block(scope, n);
            self.prefixes
                .push(format!("{}.<{}>", self.prefixes[scope], n.start_byte()));
            if let Some(p) = field(n, &["parameters", "args"]) {
                self.mask(p, s);
            }
            if lang == "dart" {
                self.parameter_types(n, s);
                if self.dart_bloc_scope(scope)
                    && let Some(call) = n
                        .parent()
                        .filter(|p| p.kind() == "arguments")
                        .and_then(|p| p.parent())
                    && let Some(function) = call.child_by_field_name("function")
                    && self.parts(function) == ["on"]
                    && self.e.unbound(scope, "on")
                    && let Some(params) = n.child_by_field_name("parameters")
                    && let Some(parameter) = params.named_child(1)
                    && let Some(name) = parameter
                        .child_by_field_name("name")
                        .or_else(|| child(parameter, &["identifier"]))
                {
                    self.value_types
                        .insert((s, self.text(name)), Some("Emitter".into()));
                }
            }
            for p in children(n).into_iter().filter(|p| p.kind() == "parameter") {
                self.mask(p, s);
            }
            if let Some(body) = n.child_by_field_name("body") {
                self.visit(body, s);
            } else {
                self.e.scopes[s].uncertain = true;
                self.walk(n, s);
            }
            return;
        }
        if lang == "julia" && matches!(k, "function_definition" | "macro_definition" | "assignment")
        {
            let header = if k == "assignment" {
                n.named_child(0)
            } else {
                child(n, &["signature"])
            };
            if let Some(header) = header
                && let Some(call) = if k == "assignment" {
                    (header.kind() == "call_expression").then_some(header)
                } else {
                    find(header, &["call_expression"])
                }
                && let Some(name) = call.named_child(0)
            {
                let s = self.define(
                    n,
                    scope,
                    self.text(name),
                    if k == "macro_definition" {
                        "macro"
                    } else {
                        "function"
                    },
                    false,
                );
                if let Some(args) = child(call, &["argument_list"]) {
                    self.mask(args, s);
                }
                for c in children(n) {
                    if c.id() != header.id() {
                        self.visit(c, s);
                    }
                }
                return;
            }
        }
        if lang == "julia"
            && matches!(
                k,
                "struct_definition" | "abstract_definition" | "primitive_definition"
            )
            && let Some(head) = child(n, &["type_head"])
            && let Some(name) = find(head, &["identifier"])
        {
            let s = self.define(
                n,
                scope,
                self.text(name),
                if k == "abstract_definition" {
                    "type"
                } else {
                    "struct"
                },
                false,
            );
            if let Some(binary) = child(head, &["binary_expression"])
                && self.e.text(binary).contains("<:")
                && let Some(base) = binary.named_child(2)
            {
                self.target(binary, s, base, "inherits");
            }
            for c in children(n) {
                if c.id() != head.id() {
                    self.visit(c, s);
                }
            }
            return;
        }
        if lang == "scala"
            && k == "package_clause"
            && let (Some(name), Some(body)) =
                (n.child_by_field_name("name"), n.child_by_field_name("body"))
        {
            let s = self.define(
                n,
                scope,
                self.text(name),
                "module",
                scope == 0 && self.prefixes[scope].starts_with('@'),
            );
            self.visit(body, s);
            return;
        }
        if lang == "fortran"
            && matches!(
                k,
                "module" | "program" | "subroutine" | "function" | "derived_type_definition"
            )
        {
            let header = child(
                n,
                &[
                    "module_statement",
                    "program_statement",
                    "subroutine_statement",
                    "function_statement",
                    "derived_type_statement",
                ],
            );
            if let Some(header) = header
                && let Some(name) =
                    field(header, &["name"]).or_else(|| child(header, &["name", "type_name"]))
            {
                let kind = match k {
                    "module" | "program" => "module",
                    "derived_type_definition" => "struct",
                    _ => "function",
                };
                let s = self.define(n, scope, self.text(name), kind, k == "module");
                if let Some(base) = header.child_by_field_name("base") {
                    self.heritage(base, s, "inherits");
                }
                if let Some(p) = header.child_by_field_name("parameters") {
                    self.mask(p, s);
                }
                for c in children(n) {
                    if c.id() != header.id() {
                        self.visit(c, s);
                    }
                }
                return;
            }
        }
        if lang == "pascal"
            && k == "defProc"
            && let Some(h) = n.child_by_field_name("header")
            && let Some(name) = h.child_by_field_name("name")
        {
            self.function(n, scope, name, h, n.child_by_field_name("body"));
            return;
        }
        if lang == "pascal"
            && k == "declType"
            && let Some(name) = n.child_by_field_name("name")
        {
            let body = n.child_by_field_name("type");
            let kind = match body.map(|b| b.kind()) {
                Some("declClass") => "class",
                Some("declIntf") => "interface",
                Some("declEnum") => "enum",
                _ => "type",
            };
            let s = self.define(n, scope, self.text(name), kind, false);
            if kind == "class" {
                let bases: Vec<String> = body
                    .map(children)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| c.kind() == "typeref")
                    .map(|c| self.text(c))
                    .collect();
                self.annotate(s, serde_json::json!({"pascal_class":self.text(name), "pascal_unit":self.owner_metadata(scope, "pascal_unit"), "pascal_bases":bases}));
            }
            if let Some(b) = body {
                self.walk(b, s);
                if kind == "class" {
                    let mut masked: Vec<_> = self.e.scopes[s]
                        .bindings
                        .iter()
                        .filter(|(_, binding)| matches!(binding, Binding::Unknown))
                        .map(|(name, _)| name.clone())
                        .collect();
                    masked.sort();
                    self.annotate(s, serde_json::json!({"pascal_shadowed_members":masked}));
                }
                if let Some(base) = b.child_by_field_name("parent") {
                    self.target(base, s, base, "inherits");
                }
            }
            return;
        }
        if lang == "zig"
            && k == "variable_declaration"
            && let Some(name) = child(n, &["identifier"])
        {
            if let Some(body) = child(
                n,
                &[
                    "struct_declaration",
                    "enum_declaration",
                    "union_declaration",
                    "opaque_declaration",
                ],
            ) {
                let s = self.define(
                    n,
                    scope,
                    self.text(name),
                    if body.kind() == "enum_declaration" {
                        "enum"
                    } else {
                        "struct"
                    },
                    false,
                );
                self.walk(body, s);
                return;
            }
            if let Some(builtin) = child(n, &["builtin_function"])
                && child(builtin, &["builtin_identifier"])
                    .is_some_and(|b| self.e.text(b) == "@import")
                && let Some(value) = find(builtin, &["string"])
            {
                self.file_import(builtin, scope, self.e.text(value), Some(self.text(name)));
                return;
            }
            let value = self.text(name);
            self.e.bind(scope, &value, Binding::Unknown);
        }
        if lang == "objc" && matches!(k, "method_definition" | "method_declaration") {
            let selector = self.selector(n, false);
            if !selector.is_empty() {
                let class_method = self.e.text(n).trim_start().starts_with('+');
                let s = self.define(
                    n,
                    scope,
                    format!("{}{selector}", if class_method { "+" } else { "-" }),
                    "method",
                    false,
                );
                if self.owner_metadata(scope, "objc_class").is_some() {
                    let owner = self.e.scopes[scope].owner.clone();
                    self.annotate(s, serde_json::json!({"objc_owner":owner, "objc_method":self.e.scopes[s].qualified.rsplit('.').next(), "objc_method_kind":if class_method { "+" } else { "-" }, "body":k == "method_definition"}));
                }
                for p in children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "method_parameter")
                {
                    if let Some(name) = child(p, &["identifier"]) {
                        self.mask(name, s);
                    }
                }
                if let Some(body) = child(n, &["compound_statement"]) {
                    self.visit(body, s);
                }
            }
            return;
        }
        if lang == "dm"
            && k == "type_definition"
            && let Some(name) = child(n, &["type_path"])
        {
            let raw = self.e.text(name);
            let name = if raw.starts_with('/') {
                raw.into()
            } else if self.prefixes[scope].starts_with('/') {
                format!("{}/{raw}", self.prefixes[scope])
            } else {
                format!("/{raw}")
            };
            let s = self.define(n, scope, name.clone(), "class", true);
            if let Some((base, _)) = name.rsplit_once('/')
                && !base.is_empty()
            {
                self.e.reference(
                    n,
                    s,
                    base.into(),
                    "inherits",
                    vec![self.key(base)],
                    "parent type is unavailable or ambiguous",
                );
            }
            for c in children(n) {
                if c.kind() == "type_body" {
                    self.visit(c, s);
                }
            }
            return;
        }
        if lang == "dm"
            && matches!(
                k,
                "proc_definition" | "proc_override" | "type_proc_definition" | "type_proc_override"
            )
            && let Some(name) = n.child_by_field_name("name")
        {
            let owner = child(n, &["type_path"]).map(|p| self.text(p));
            let prefix = owner.unwrap_or_else(|| {
                if self.prefixes[scope].starts_with('/') {
                    self.prefixes[scope].clone()
                } else {
                    "/proc".into()
                }
            });
            let full = format!("{prefix}/{}", self.text(name));
            let s = self.define(n, scope, full.clone(), "function", true);
            if prefix == "/proc" {
                let short = self.text(name);
                self.e.bind(scope, &short, Binding::symbol(self.key(&full)));
            }
            if let Some(params) = child(n, &["proc_parameters"]) {
                self.mask(params, s);
            }
            if let Some(body) = child(n, &["block"]) {
                self.visit(body, s);
            }
            return;
        }

        let definition = match (lang, k) {
            (
                "scala",
                "class_definition" | "object_definition" | "trait_definition" | "enum_definition",
            ) => Some((
                n.child_by_field_name("name"),
                match k {
                    "trait_definition" => "trait",
                    "enum_definition" => "enum",
                    "object_definition" => "module",
                    _ => "class",
                },
                false,
            )),
            (
                "dart" | "groovy" | "apex",
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "mixin_declaration"
                | "extension_declaration"
                | "extension_type_declaration",
            ) => Some((
                n.child_by_field_name("name"),
                match k {
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    "mixin_declaration" => "trait",
                    _ => "class",
                },
                lang == "apex" && scope == 0,
            )),
            ("apex", "trigger_declaration") => {
                Some((n.child_by_field_name("name"), "trigger", true))
            }
            ("julia", "module_definition") => {
                Some((n.child_by_field_name("name"), "module", scope == 0))
            }
            ("ocaml", "module_binding") => Some((child(n, &["module_name"]), "module", false)),
            ("ocaml", "type_binding") => Some((n.child_by_field_name("name"), "type", false)),
            ("ocaml", "constructor_declaration") => {
                Some((child(n, &["constructor_name"]), "variant", false))
            }
            ("ocaml", "value_specification") => {
                Some((child(n, &["value_name"]), "function", false))
            }
            ("pascal", "unit" | "program" | "library") => {
                Some((child(n, &["moduleName"]), "module", true))
            }
            ("objc", "class_interface" | "class_implementation" | "protocol_declaration") => {
                Some((
                    child(n, &["identifier"]),
                    if k == "protocol_declaration" {
                        "interface"
                    } else {
                        "class"
                    },
                    true,
                ))
            }
            ("verilog", "module_declaration") => Some((
                child(n, &["module_header"])
                    .and_then(|h| child(h, &["simple_identifier", "escaped_identifier"])),
                "module",
                true,
            )),
            (
                "verilog",
                "package_declaration" | "class_declaration" | "interface_class_declaration",
            ) => Some((
                child(n, &["package_identifier", "class_identifier"]),
                if k == "package_declaration" {
                    "module"
                } else {
                    "class"
                },
                k == "package_declaration",
            )),
            _ => None,
        };
        if let Some((Some(name), kind, global)) = definition {
            let s = self.define(n, scope, self.text(name), kind, global);
            if lang == "objc" && kind == "class" {
                let mut cursor = n.walk();
                let category = n.children(&mut cursor).any(|c| c.kind() == "(");
                self.annotate(s, serde_json::json!({"objc_class":self.text(name), "objc_category":category, "objc_role":k}));
            }
            if lang == "pascal" && kind == "module" {
                self.annotate(s, serde_json::json!({"pascal_unit":self.text(name)}));
            }
            self.declaration_annotations(n, s);
            if lang == "ocaml" {
                for parameter in children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "module_parameter")
                {
                    if let Some(name) = child(parameter, &["module_name"]) {
                        self.mask(name, s);
                    }
                }
                if k == "module_binding"
                    && let Some(body) = n.child_by_field_name("body")
                {
                    let parts = self.parts(body);
                    if !parts.is_empty() {
                        let mut prefixes = self.e.resolve(scope, &parts);
                        if prefixes.is_empty() {
                            prefixes = self.ocaml_external(scope, &parts, true);
                        }
                        for prefix in &mut prefixes {
                            if let Some(path) = prefix.strip_prefix("ocaml:file:") {
                                *prefix = self.key(&format!("@{path}"));
                            }
                        }
                        if !prefixes.is_empty() {
                            let alias = self.text(name);
                            self.e.scopes[scope].bindings.insert(
                                alias,
                                Binding::Namespace {
                                    prefixes: prefixes.iter().map(|p| format!("{p}.")).collect(),
                                    separator: ".",
                                },
                            );
                        }
                    }
                }
            }
            for f in ["extend", "superclass"] {
                if let Some(h) = n.child_by_field_name(f) {
                    self.heritage(h, s, "inherits");
                }
            }
            if let Some(h) = n.child_by_field_name("interfaces") {
                self.heritage(h, s, "implements");
            }
            if lang == "verilog" {
                for h in children(n).into_iter().filter(|c| c.kind() == "class_type") {
                    self.heritage(h, s, "inherits");
                }
            }
            if lang == "objc" {
                for h in children(n).into_iter().filter(|c| {
                    matches!(
                        c.kind(),
                        "parameterized_arguments" | "protocol_reference_list"
                    )
                }) {
                    self.heritage(h, s, "implements");
                }
            }
            if lang == "apex"
                && k == "trigger_declaration"
                && let Some(object) = n.child_by_field_name("object")
            {
                self.unresolved(object, s, self.text(object), "uses");
            }
            for c in children(n) {
                if c.id() != name.id() {
                    self.visit(c, s);
                }
            }
            return;
        }
        if lang == "ocaml"
            && k == "let_binding"
            && child(n, &["parameter"]).is_none()
            && let (Some(name), Some(body)) = (
                n.child_by_field_name("pattern"),
                n.child_by_field_name("body"),
            )
            && name.kind() == "value_name"
        {
            let kind = if matches!(body.kind(), "fun_expression" | "function_expression") {
                "function"
            } else {
                "variable"
            };
            let s = self.define(n, scope, self.text(name), kind, false);
            if kind == "variable" {
                let name = self.text(name);
                self.e.invalidate(scope, &name);
            }
            self.visit(body, s);
            return;
        }
        let function = match (lang, k) {
            ("scala" | "zig" | "groovy", "function_definition" | "function_declaration")
            | ("apex" | "groovy", "method_declaration" | "constructor_declaration") => n
                .child_by_field_name("name")
                .map(|name| (name, n, n.child_by_field_name("body"))),
            (
                "dart",
                "function_declaration"
                | "method_declaration"
                | "local_function_declaration"
                | "getter_declaration"
                | "setter_declaration",
            ) => find(
                n.child_by_field_name("signature").unwrap_or(n),
                &[
                    "function_signature",
                    "getter_signature",
                    "setter_signature",
                    "constructor_signature",
                ],
            )
            .and_then(|h| {
                h.child_by_field_name("name").map(|name| {
                    (
                        name,
                        h,
                        n.child_by_field_name("body")
                            .or_else(|| child(n, &["function_body"])),
                    )
                })
            }),
            ("ocaml", "let_binding") => n
                .child_by_field_name("pattern")
                .filter(|p| p.kind() == "value_name")
                .map(|name| (name, n, n.child_by_field_name("body"))),
            ("pascal", "declProc") => n.child_by_field_name("name").map(|name| (name, n, None)),
            ("verilog", "function_declaration" | "task_declaration") => {
                child(n, &["function_body_declaration", "task_body_declaration"]).and_then(|h| {
                    child(h, &["function_identifier", "task_identifier"])
                        .map(|name| (name, h, Some(h)))
                })
            }
            _ => None,
        };
        if let Some((name, h, body)) = function {
            self.function(n, scope, name, h, body);
            return;
        }
        if self.imports(n, scope) {
            return;
        }
        if matches!(k, "preproc_if" | "preproc_ifdef" | "preproc_ifndef") {
            diagnostic(
                &mut self.e.facts,
                Some(super::super::common::line(n)),
                "Conditional compilation requires a build environment; branch omitted",
            );
            return;
        }
        if matches!(k, "annotation" | "marker_annotation")
            && let Some(name) = field(n, &["name"]).or_else(|| child(n, &["identifier"]))
        {
            self.target(n, scope, name, "annotated_by");
        }
        if lang == "scala"
            && matches!(k, "class_parameter" | "val_definition" | "var_definition")
            && self.e.scopes[scope].class
            && let Some(name) = field(n, &["name", "pattern"]).filter(|n| names(*n))
        {
            let s = self.define(n, scope, self.text(name), "field", false);
            if let Some(ty) = n.child_by_field_name("type") {
                self.heritage(ty, s, "uses_type");
            }
            if let Some(value) = n.child_by_field_name("value") {
                self.visit(value, s);
            }
            return;
        }
        if lang == "apex" && k == "from_clause" {
            for storage in children(n)
                .into_iter()
                .filter(|n| n.kind() == "storage_identifier")
            {
                self.unresolved(storage, scope, self.text(storage), "uses");
            }
        }
        if lang == "apex" && k == "dml_expression" {
            self.apex_dml(n, scope);
        }

        if lang == "julia"
            && k == "typed_expression"
            && self.e.scopes[scope].class
            && let (Some(name), Some(ty)) = (n.named_child(0), n.named_child(1))
            && names(name)
        {
            let s = self.define(n, scope, self.text(name), "field", false);
            self.target(ty, s, ty, "uses_type");
            return;
        }
        if lang == "zig"
            && k == "container_field"
            && let Some(name) = n.child_by_field_name("name")
        {
            let s = self.define(n, scope, self.text(name), "field", false);
            if let Some(ty) = n.child_by_field_name("type") {
                self.target(ty, s, ty, "uses_type");
            }
        }
        if lang == "objc" && k == "property_declaration" {
            if let Some(declaration) = child(n, &["struct_declaration"])
                && let Some(declarator) = child(declaration, &["struct_declarator"])
                && let Some(name) = find(declarator, &["identifier"])
            {
                let s = self.define(n, scope, self.text(name), "property", false);
                if let Some(ty) = child(declaration, &["type_identifier"]) {
                    self.target(ty, s, ty, "uses_type");
                }
            }
            return;
        }
        if matches!(k, "extends_interfaces" | "inheritance_definition") {
            self.heritage(n, scope, "inherits");
        }
        if matches!(
            k,
            "parameter"
                | "formal_parameter"
                | "field_declaration"
                | "variable_declaration"
                | "declField"
                | "declArg"
        ) && let Some(ty) = n.child_by_field_name("type")
        {
            self.heritage(ty, scope, "uses_type");
        }
        match (lang, k) {
            (_, "call_expression") => {
                if lang == "dart" {
                    self.dart_call(n, scope);
                }
                let target = field(n, &["function", "name"]).or_else(|| {
                    if matches!(lang, "julia" | "fortran") {
                        n.named_child(0)
                    } else {
                        None
                    }
                });
                if let Some(target) = target {
                    self.call(n, scope, target, false);
                }
            }
            ("fortran", "subroutine_call") => {
                if let Some(t) = n.child_by_field_name("subroutine") {
                    self.call(n, scope, t, false);
                }
            }
            ("ocaml", "application_expression") => {
                if !self.e.facts.path.ends_with(".mli")
                    && let Some(t) = n.child_by_field_name("function")
                    && t.kind() != "application_expression"
                {
                    self.call(n, scope, t, false);
                }
            }
            ("pascal", "exprCall") => {
                if let Some(t) = n.child_by_field_name("entity") {
                    self.pascal_call(n, scope, t);
                }
            }
            ("apex" | "groovy", "method_invocation") => {
                if let Some(t) = n.child_by_field_name("name") {
                    self.call(n, scope, t, n.child_by_field_name("object").is_some());
                }
            }
            ("groovy", "juxt_function_call") => {
                if let Some(t) = n.child_by_field_name("name") {
                    self.call(n, scope, t, false);
                }
            }
            ("objc", "message_expression") => self.objc_message(n, scope),
            ("dm", "field_proc_expression") => {
                if let Some(t) = n.child_by_field_name("proc") {
                    self.call(n, scope, t, true);
                }
            }
            ("dm", "new_expression") => {
                if let Some(t) = child(n, &["type_path"]) {
                    let name = self.text(t);
                    self.e.reference(
                        n,
                        scope,
                        name.clone(),
                        "instantiates",
                        vec![self.key(&name)],
                        "type is unavailable or ambiguous",
                    );
                }
            }
            ("verilog", "tf_call") => {
                if let Some(t) = child(n, &["simple_identifier", "escaped_identifier"]) {
                    self.call(n, scope, t, false);
                }
            }
            ("verilog", "method_call") => {
                self.unresolved(n, scope, self.text(n), "calls");
                return;
            }
            ("verilog", "module_instantiation" | "checker_instantiation") => {
                if let Some(t) = child(
                    n,
                    &[
                        "simple_identifier",
                        "escaped_identifier",
                        "checker_identifier",
                    ],
                ) {
                    let name = self.text(t);
                    self.e.reference(
                        n,
                        scope,
                        name.clone(),
                        "instantiates",
                        vec![self.key(&name)],
                        "module is unavailable or ambiguous",
                    );
                }
            }
            _ => {}
        }
        if lang == "fortran" && k == "variable_declaration" {
            let mut cursor = n.walk();
            for d in n.children_by_field_name("declarator", &mut cursor) {
                if let Some(name) = find(d, &["identifier"]) {
                    self.mask(name, scope);
                }
            }
        }
        // Writes and parameters mask lexical functions; no name-only fallback.
        if matches!(
            k,
            "parameter"
                | "formal_parameter"
                | "proc_parameter"
                | "variable_declarator"
                | "initialized_variable_definition"
                | "binding"
                | "declVar"
                | "declArg"
        ) && let Some(name) = field(n, &["name", "pattern"])
        {
            self.mask(name, scope);
        }
        if matches!(k, "val_definition" | "var_definition")
            && lang == "scala"
            && let Some(p) = n.child_by_field_name("pattern")
        {
            self.mask(p, scope);
        }
        if matches!(
            k,
            "assignment_expression" | "assignment_statement" | "assignment"
        ) && let Some(lhs) = field(n, &["left", "lhs"]).or_else(|| n.named_child(0))
            && names(lhs)
        {
            let name = self.text(lhs);
            self.e.invalidate(scope, &name);
        }
        self.walk(n, scope);
    }
}
