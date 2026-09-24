use super::*;

impl<'s, 't> Extended<'s, 't> {
    pub(super) fn fact_node(
        &mut self,
        n: Syntax<'t>,
        label: &str,
        kind: &str,
        context: &str,
    ) -> String {
        let id = format!(
            "{}:{}:{context}:{label}",
            self.e.language, self.e.facts.path
        );
        if !self.e.facts.nodes.iter().any(|v| v.id == id) {
            self.e.facts.nodes.push(crate::model::Node {
                id: id.clone(),
                label: label.into(),
                kind: kind.into(),
                file: self.e.facts.path.clone(),
                line: Some(super::super::common::line(n)),
                end_line: Some(super::super::common::end_line(n)),
                qualified_name: None,
                binding_key: None,
                metadata: serde_json::json!({"context": context}),
            });
        }
        id
    }
    pub(super) fn fact_edge(
        &mut self,
        n: Syntax<'t>,
        source: String,
        target: String,
        relation: &str,
        context: &str,
    ) {
        self.e.facts.edges.push(crate::model::Edge {
            id: format!("{relation}:{source}:{target}:{}", n.start_byte()),
            source,
            target,
            relation: relation.into(),
            directed: true,
            file: Some(self.e.facts.path.clone()),
            line: Some(super::super::common::line(n)),
            confidence: "static".into(),
            metadata: serde_json::json!({"context": context}),
        });
    }
    pub(super) fn contextual_target(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        target: Syntax<'t>,
        relation: &'static str,
        context: &str,
    ) {
        self.contextual.push((
            n,
            scope,
            self.text(target),
            relation,
            self.parts(target),
            context.into(),
        ));
    }
    pub(super) fn lisp_rationale(&mut self, n: Syntax<'t>, scope: usize) {
        let raw = self.e.text(n);
        let Some(raw) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            return;
        };
        let mut chars = raw.chars();
        let mut doc = String::new();
        while let Some(c) = chars.next() {
            doc.push(if c == '\\' {
                chars.next().unwrap_or(c)
            } else {
                c
            });
        }
        let label: String = doc.chars().take(120).collect();
        let id = self.fact_node(
            n,
            &label,
            "rationale",
            &format!("docstring:{}", n.start_byte()),
        );
        if let Some(node) = self.e.facts.nodes.iter_mut().find(|v| v.id == id) {
            node.metadata["text"] = doc.into();
        }
        self.fact_edge(
            n,
            id,
            self.e.scopes[scope].owner.clone(),
            "rationale_for",
            "docstring",
        );
    }
    // OCaml compilation units use the capitalized source basename. Keep sibling
    // candidates qualified by directory; a compiler load path is not available.
    pub(super) fn ocaml_external(
        &self,
        scope: usize,
        parts: &[String],
        module: bool,
    ) -> Vec<String> {
        let Some(first) = parts.first() else {
            return vec![];
        };
        if parts.len() == 1 && module {
            let mut current = Some(scope);
            while let Some(s) = current {
                if self.e.scopes[s].uncertain {
                    return vec![];
                }
                if let Some(b) = self.e.scopes[s].bindings.get(first) {
                    return match b {
                        Binding::Namespace { prefixes, .. } => prefixes
                            .iter()
                            .map(|p| p.trim_end_matches('.').into())
                            .collect(),
                        _ => vec![],
                    };
                }
                current = self.e.scopes[s].parent;
            }
        }
        if (!module && parts.len() < 2)
            || !self.e.unbound(scope, first)
            || !first.starts_with(|c: char| c.is_ascii_uppercase())
        {
            return vec![];
        }
        let base = format!("{}{}", first[..1].to_ascii_lowercase(), &first[1..]);
        let dir = self.e.facts.path.rsplit_once('/').map_or("", |p| p.0);
        ["ml", "mli"]
            .into_iter()
            .filter_map(|ext| {
                let path = relative_path(dir, &format!("{base}.{ext}"))?;
                Some(if module && parts.len() == 1 {
                    format!("ocaml:file:{path}")
                } else {
                    self.key(&format!("@{path}.{}", parts[1..].join(".")))
                })
            })
            .collect()
    }
    pub(super) fn fortran_directive(&mut self, n: Syntax<'t>, scope: usize) -> bool {
        match n.kind() {
            "preproc_def" => {
                if let Some(name) = n.child_by_field_name("name") {
                    self.cpp_defines.insert(
                        self.e.text(name).into(),
                        Some(
                            n.child_by_field_name("value")
                                .map_or("", |v| self.e.text(v))
                                .trim()
                                .into(),
                        ),
                    );
                }
            }
            "preproc_function_def" => {}
            "preproc_call" => {
                if field(n, &["directive"]).is_some_and(|d| self.e.text(d).trim() == "#undef")
                    && let Some(name) = field(n, &["argument"])
                {
                    self.cpp_defines
                        .insert(self.e.text(name).trim().into(), None);
                }
            }
            "preproc_include" => {
                if let Some(path) = n.child_by_field_name("path") {
                    if path.kind() == "string_literal" {
                        self.file_import(n, scope, self.e.text(path), None);
                    } else {
                        self.unresolved(n, scope, self.text(path), "imports");
                    }
                }
            }
            "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_elifdef" => {
                let condition = field(n, &["condition", "name"]);
                let known = condition.and_then(|c| {
                    let text = self.e.text(c).trim();
                    if n.kind().ends_with("ifdef") || n.kind().ends_with("elifdef") {
                        self.cpp_defines.get(text).map(|v| {
                            v.is_some() != self.e.text(n).trim_start().starts_with("#ifndef")
                        })
                    } else {
                        let value = self
                            .cpp_defines
                            .get(text)
                            .and_then(|v| v.as_deref())
                            .unwrap_or(text);
                        match value {
                            "0" => Some(false),
                            "1" => Some(true),
                            _ => None,
                        }
                    }
                });
                let alternative = n.child_by_field_name("alternative");
                if known != Some(false) {
                    self.fortran_branch(n, scope, known.is_none(), condition, alternative);
                }
                if known != Some(true)
                    && let Some(other) = alternative
                {
                    if known.is_none() {
                        self.fortran_branch(other, scope, true, None, None);
                    } else {
                        self.visit(other, scope);
                    }
                }
            }
            "preproc_else" => self.walk(n, scope),
            _ => return false,
        }
        true
    }
    pub(super) fn fortran_branch(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        conditional: bool,
        condition: Option<Syntax<'t>>,
        alternative: Option<Syntax<'t>>,
    ) {
        let condition = condition.or_else(|| field(n, &["condition", "name"]));
        let first_node = self.e.facts.nodes.len();
        let first_ref = self.e.facts.references.len();
        let saved = self.cpp_defines.clone();
        let s = if conditional {
            let s = self.e.block(scope, n);
            self.e.scopes[s].uncertain = true;
            self.prefixes.push(self.prefixes[scope].clone());
            s
        } else {
            scope
        };
        for c in children(n) {
            if condition.is_none_or(|v| v.id() != c.id())
                && alternative.is_none_or(|v| v.id() != c.id())
            {
                self.visit(c, s);
            }
        }
        if conditional {
            self.cpp_defines = saved;
            for node in &mut self.e.facts.nodes[first_node..] {
                node.binding_key = None;
                node.metadata["conditional_compilation"] = true.into();
            }
            for reference in &mut self.e.facts.references[first_ref..] {
                reference.candidate_keys.clear();
            }
        }
    }
    pub(super) fn type_name(&self, n: Syntax<'t>) -> Option<String> {
        let mut parts = self.parts(n);
        if self.e.language == "apex"
            && parts
                .first()
                .is_some_and(|p| p.eq_ignore_ascii_case("List") || p.eq_ignore_ascii_case("Set"))
        {
            parts = child(n, &["type_arguments"])
                .and_then(|v| v.named_child(0))
                .map_or_else(Vec::new, |v| self.parts(v));
        }
        (!parts.is_empty()).then(|| parts.join("."))
    }
    pub(super) fn value_type(&self, scope: usize, name: &str) -> Option<String> {
        let mut current = Some(scope);
        while let Some(s) = current {
            if self.e.scopes[s].uncertain {
                return None;
            }
            if let Some(ty) = self.value_types.get(&(s, name.into())) {
                return ty.clone();
            }
            if self.e.scopes[s].bindings.contains_key(name) {
                return None;
            }
            current = self.e.scopes[s].parent;
        }
        None
    }
    pub(super) fn parameter_types(&mut self, header: Syntax<'t>, scope: usize) {
        let mut stack = vec![header];
        while let Some(n) = stack.pop() {
            if matches!(
                n.kind(),
                "formal_parameter" | "catch_formal_parameter" | "enhanced_for_statement"
            ) {
                let name = n
                    .child_by_field_name("name")
                    .or_else(|| child(n, &["identifier"]));
                if let Some(name) = name {
                    let ty = n
                        .child_by_field_name("type")
                        .or_else(|| child(n, &["type"]))
                        .and_then(|v| self.type_name(v));
                    let name = self.text(name);
                    self.e.bind(scope, &name, Binding::Unknown);
                    self.value_types.insert((scope, name), ty);
                }
            } else {
                stack.extend(children(n).into_iter().filter(|c| {
                    !matches!(
                        c.kind(),
                        "block" | "function_body" | "function_expression_body" | "class_body"
                    )
                }));
            }
        }
    }
    pub(super) fn typed_declaration(&mut self, n: Syntax<'t>, scope: usize) -> bool {
        let apex = self.e.language == "apex";
        if !(apex && matches!(n.kind(), "local_variable_declaration" | "field_declaration"))
            && !(!apex
                && matches!(
                    n.kind(),
                    "initialized_variable_definition"
                        | "top_level_variable_declaration"
                        | "declaration"
                ))
        {
            return false;
        }
        if n.kind() == "declaration"
            && child(
                n,
                &[
                    "initialized_identifier_list",
                    "static_final_declaration_list",
                ],
            )
            .is_none()
        {
            return false;
        }
        let ty = n
            .child_by_field_name("type")
            .or_else(|| child(n, &["type"]))
            .and_then(|v| self.type_name(v));
        let mut stack = vec![n];
        while let Some(d) = stack.pop() {
            if let Some(name) = d.child_by_field_name("name") {
                let name = self.text(name);
                let value = d.child_by_field_name("value");
                let provider = !apex
                    && self.dart_has(&["riverpod", "flutter_riverpod", "hooks_riverpod"])
                    && value
                        .and_then(|v| v.child_by_field_name("function"))
                        .is_some_and(|function| {
                            let parts = self.parts(function);
                            parts.len() == 1
                                && self.dart_framework_name(scope, &parts[0])
                                && matches!(
                                    parts[0].as_str(),
                                    "Provider"
                                        | "StateProvider"
                                        | "FutureProvider"
                                        | "StreamProvider"
                                        | "NotifierProvider"
                                        | "StateNotifierProvider"
                                )
                        });
                if provider {
                    // A real provider declaration installs its symbol exactly once.
                    self.define(d, scope, name.clone(), "variable", false);
                } else {
                    self.e.bind(scope, &name, Binding::Unknown);
                }
                self.value_types.insert((scope, name), ty.clone());
                if let Some(value) = value {
                    self.visit(value, scope);
                }
                for c in children(d)
                    .into_iter()
                    .filter(|c| c.kind() == "initialized_identifier")
                {
                    stack.push(c);
                }
            } else {
                stack.extend(
                    children(d)
                        .into_iter()
                        .filter(|c| !matches!(c.kind(), "type" | "generic_type" | "annotation")),
                );
            }
        }
        true
    }
    pub(super) fn declaration_annotations(&mut self, n: Syntax<'t>, scope: usize) {
        if !matches!(self.e.language, "apex" | "dart") {
            return;
        }
        let mut annotations = children(n);
        if let Some(modifiers) = child(n, &["modifiers"]) {
            annotations.extend(children(modifiers));
        }
        for annotation in annotations
            .into_iter()
            .filter(|c| matches!(c.kind(), "annotation" | "marker_annotation"))
        {
            let Some(name) = annotation.child_by_field_name("name") else {
                continue;
            };
            let label = self.text(name);
            let id = self.fact_node(annotation, &label, "annotation", "annotation");
            let owner = self.e.scopes[scope].owner.clone();
            self.fact_edge(annotation, id, owner.clone(), "configures", "annotation");
            if self.e.language == "apex"
                && matches!(
                    label.to_ascii_lowercase().as_str(),
                    "auraenabled" | "invocablemethod"
                )
            {
                self.fact_edge(
                    annotation,
                    self.e.scopes[0].owner.clone(),
                    owner,
                    "exposes",
                    "apex_entrypoint",
                );
            }
            if self.e.language == "dart"
                && label == "riverpod"
                && self.dart_has(&["riverpod_annotation"])
                && self.dart_framework_name(scope, &label)
            {
                let Some(parent) = self.e.scopes[scope].parent else {
                    continue;
                };
                let Some(definition) = self
                    .e
                    .facts
                    .nodes
                    .iter()
                    .find(|v| v.id == self.e.scopes[scope].owner)
                else {
                    continue;
                };
                let mut chars = definition.label.chars();
                let Some(first) = chars.next() else { continue };
                let generated = format!("{}{}Provider", first.to_lowercase(), chars.as_str());
                let s = self.define(annotation, parent, generated, "variable", false);
                let generated_id = self.e.scopes[s].owner.clone();
                self.fact_edge(
                    annotation,
                    self.e.scopes[scope].owner.clone(),
                    generated_id.clone(),
                    "defines",
                    "riverpod_generator",
                );
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|v| v.id == generated_id) {
                    node.metadata["generated"] = true.into();
                }
            }
        }
    }
    pub(super) fn apex_dml(&mut self, n: Syntax<'t>, scope: usize) {
        let Some(operation) = child(n, &["dml_type"]) else {
            return;
        };
        let op = self.text(operation).to_ascii_lowercase();
        let id = self.fact_node(operation, &op, "operation", "dml");
        self.fact_edge(
            n,
            self.e.scopes[scope].owner.clone(),
            id,
            "uses",
            "dml_operation",
        );
        for target in ["target", "merge_with"]
            .into_iter()
            .filter_map(|f| n.child_by_field_name(f))
        {
            let ty = if target.kind() == "object_creation_expression" {
                target
                    .child_by_field_name("type")
                    .and_then(|v| self.type_name(v))
            } else if target.kind() == "identifier" {
                self.value_type(scope, self.e.text(target))
            } else {
                None
            };
            let ty = ty.filter(|v| {
                !matches!(
                    v.to_ascii_lowercase().as_str(),
                    "sobject" | "object" | "string" | "integer" | "list" | "set"
                )
            });
            if let Some(ty) = ty {
                self.e.reference(
                    target,
                    scope,
                    ty.clone(),
                    "uses",
                    vec![self.key(&ty)],
                    &format!("dml_{op}: explicitly declared operand type"),
                );
            } else {
                self.e.reference(
                    target,
                    scope,
                    self.text(target),
                    "uses",
                    vec![],
                    &format!("dml_{op}: operand type is unknown"),
                );
            }
        }
    }
    pub(super) fn dart_has(&self, packages: &[&str]) -> bool {
        packages.iter().any(|p| self.dart_packages.contains(*p))
    }
    pub(super) fn dart_framework_name(&self, scope: usize, name: &str) -> bool {
        !self.dart_local_types.contains(name) && self.e.unbound(scope, name)
    }
    pub(super) fn dart_environment(&mut self, root: Syntax<'t>) {
        for n in children(root).into_iter().filter(|n| {
            matches!(
                n.kind(),
                "class_declaration" | "mixin_declaration" | "enum_declaration" | "type_alias"
            )
        }) {
            if let Some(name) = n.child_by_field_name("name") {
                self.dart_local_types.insert(self.text(name));
            }
        }
        for n in children(root) {
            if n.kind() == "import_or_export" || n.kind() == "import_specification" {
                let import = if n.kind() == "import_specification" {
                    Some(n)
                } else {
                    find(n, &["import_specification"])
                };
                if let Some(import) = import
                    && import.child_by_field_name("alias").is_none()
                    && child(import, &["combinator"]).is_none()
                    && let Some(literal) = find(import, &["string_literal"])
                    && let Some(uri) = dart_string(self.e.text(literal))
                    && let Some(package) = uri
                        .strip_prefix("package:")
                        .and_then(|s| s.split('/').next())
                {
                    self.dart_packages.insert(package.into());
                }
            }
        }
        if self.dart_has(&["bloc", "flutter_bloc"]) {
            for n in children(root)
                .into_iter()
                .filter(|n| n.kind() == "class_declaration")
            {
                if let (Some(name), Some(base)) = (
                    n.child_by_field_name("name"),
                    n.child_by_field_name("superclass"),
                ) && let Some(base) = find(base, &["type_identifier"])
                    && matches!(self.e.text(base), "Bloc" | "Cubit")
                    && !self.dart_local_types.contains(self.e.text(base))
                {
                    self.dart_bloc_types
                        .insert(self.text(name), self.text(base));
                }
            }
        }
    }
    pub(super) fn dart_bloc_scope(&self, scope: usize) -> bool {
        let mut current = Some(scope);
        while let Some(s) = current {
            if self.e.scopes[s].class {
                return self
                    .e
                    .facts
                    .nodes
                    .iter()
                    .find(|n| n.id == self.e.scopes[s].owner)
                    .is_some_and(|n| self.dart_bloc_types.contains_key(&n.label));
            }
            current = self.e.scopes[s].parent;
        }
        false
    }
    pub(super) fn dart_call(&mut self, n: Syntax<'t>, scope: usize) {
        let Some(function) = n.child_by_field_name("function") else {
            return;
        };
        let parts = self.parts(function);
        let Some(method) = parts.last().map(String::as_str) else {
            return;
        };
        let args = n
            .child_by_field_name("arguments")
            .map(children)
            .unwrap_or_default();
        let types = function
            .child_by_field_name("type_arguments")
            .map(children)
            .unwrap_or_default();
        let receiver = (parts.len() == 2)
            .then(|| self.value_type(scope, &parts[0]))
            .flatten();
        let bloc = self.dart_has(&["bloc", "flutter_bloc"]);
        let riverpod = self.dart_has(&[
            "riverpod",
            "flutter_riverpod",
            "hooks_riverpod",
            "riverpod_annotation",
        ]);
        if bloc
            && parts.len() == 1
            && method == "on"
            && self.e.unbound(scope, "on")
            && self.dart_bloc_scope(scope)
            && let Some(event) = types.first()
        {
            self.contextual_target(n, scope, *event, "calls", "bloc_event");
        }
        if bloc
            && method == "emit"
            && parts.len() == 1
            && ((self.dart_bloc_scope(scope) && self.e.unbound(scope, method))
                || self.value_type(scope, method).as_deref() == Some("Emitter"))
            && let Some(arg) = args.first().and_then(|v| v.child_by_field_name("function"))
        {
            self.contextual_target(n, scope, arg, "calls", "emit_state");
        }
        if bloc
            && method == "add"
            && receiver
                .as_ref()
                .is_some_and(|t| self.dart_bloc_types.contains_key(t) || t == "Bloc")
            && let Some(arg) = args.first().and_then(|v| v.child_by_field_name("function"))
        {
            self.contextual_target(n, scope, arg, "calls", "bloc_add_event");
        }
        if bloc
            && parts.len() == 1
            && matches!(
                method,
                "BlocBuilder" | "BlocListener" | "BlocConsumer" | "BlocProvider" | "BlocSelector"
            )
            && self.dart_framework_name(scope, method)
            && let Some(ty) = types.first()
        {
            self.contextual_target(n, scope, *ty, "references", "bloc_widget_binding");
        }
        if bloc
            && ((receiver.as_deref() == Some("BuildContext")
                && self.dart_framework_name(scope, "BuildContext")
                && matches!(method, "read" | "watch" | "select"))
                || (parts == ["BlocProvider", "of"]
                    && self.dart_framework_name(scope, "BlocProvider")))
            && let Some(ty) = types.first()
        {
            self.contextual_target(n, scope, *ty, "references", "bloc_lookup");
        }
        if riverpod
            && matches!(receiver.as_deref(), Some("WidgetRef" | "Ref"))
            && receiver
                .as_ref()
                .is_some_and(|name| self.dart_framework_name(scope, name))
            && matches!(method, "watch" | "read" | "listen")
            && let Some(provider) = args.first().filter(|a| !self.parts(**a).is_empty())
        {
            self.contextual_target(n, scope, *provider, "references", "riverpod_reference");
        }
        let navigator = parts.len() == 2
            && parts[0] == "Navigator"
            && self.dart_has(&["flutter"])
            && self.dart_framework_name(scope, "Navigator");
        let router = self.dart_has(&["go_router"])
            && matches!(receiver.as_deref(), Some("BuildContext" | "GoRouter"))
            && receiver
                .as_ref()
                .is_some_and(|name| self.dart_framework_name(scope, name));
        if ((router
            && matches!(
                method,
                "go" | "push" | "goNamed" | "pushNamed" | "replace" | "replaceNamed"
            ))
            || (navigator
                && matches!(
                    method,
                    "pushNamed" | "pushReplacementNamed" | "popAndPushNamed"
                )))
            && let Some(target) = args.get(usize::from(navigator))
        {
            let context = if method.contains("Named") {
                "route_name"
            } else {
                "route_path"
            };
            if target.kind() == "string_literal"
                && let Some(value) = dart_string(self.e.text(*target))
            {
                let id = self.fact_node(*target, &value, "route", context);
                self.fact_edge(
                    n,
                    self.e.scopes[scope].owner.clone(),
                    id,
                    "navigates",
                    context,
                );
            } else if !self.parts(*target).is_empty() {
                self.contextual_target(n, scope, *target, "navigates", "route_const");
            }
        }
        if self.dart_has(&["go_router"])
            && parts == ["GoRoute"]
            && self.dart_framework_name(scope, "GoRoute")
        {
            for arg in args.iter().filter(|a| a.kind() == "named_argument") {
                let cs = children(*arg);
                if let [label, value] = cs.as_slice()
                    && matches!(self.e.text(*label).trim_end_matches(':'), "path" | "name")
                    && value.kind() == "string_literal"
                    && let Some(value) = dart_string(self.e.text(*value))
                {
                    let context = if self.e.text(*label).trim_end_matches(':') == "name" {
                        "route_name"
                    } else {
                        "route_path"
                    };
                    let id = self.fact_node(*arg, &value, "route", context);
                    self.fact_edge(
                        n,
                        self.e.scopes[scope].owner.clone(),
                        id,
                        "defines",
                        context,
                    );
                }
            }
        }
    }
}
