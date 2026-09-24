use super::*;

impl<'s, 't> Extended<'s, 't> {
    pub(super) fn norm(&self, text: &str) -> String {
        if self.e.language == "commonlisp" && (text.contains(['|', '\\']) || text.starts_with('"'))
        {
            text.into()
        } else if matches!(self.e.language, "fortran" | "pascal" | "commonlisp") {
            text.to_lowercase()
        } else {
            text.into()
        }
    }
    pub(super) fn key(&self, q: &str) -> String {
        format!("{}:symbol:{q}", self.e.language)
    }
    pub(super) fn text(&self, n: Syntax<'_>) -> String {
        self.norm(self.e.text(n))
    }
    pub(super) fn parts(&self, n: Syntax<'_>) -> Vec<String> {
        if names(n) {
            return vec![self.text(n)];
        }
        match n.kind() {
            "field_expression" | "member_expression" | "scoped_identifier" | "genericDot"
            | "exprDot" => {
                let Some(a) = field(n, &["object", "value", "scope", "lhs"]) else {
                    return vec![];
                };
                let Some(b) = field(n, &["field", "member", "property", "name", "rhs"]) else {
                    return vec![];
                };
                let mut p = self.parts(a);
                if p.is_empty() {
                    return p;
                }
                p.extend(self.parts(b));
                p
            }
            "value_path"
            | "module_path"
            | "package_identifier"
            | "moduleName"
            | "import_path"
            | "stable_type_identifier" => {
                let mut p = vec![];
                for c in children(n) {
                    if c.kind() != "kDot" {
                        let s = self.parts(c);
                        if s.is_empty() {
                            return vec![];
                        }
                        p.extend(s);
                    }
                }
                p
            }
            "type"
            | "generic_type"
            | "generic_function"
            | "instantiation_expression"
            | "type_name"
            | "typeref"
            | "class_identifier"
            | "function_identifier"
            | "task_identifier"
            | "module_name"
            | "class_type" => n
                .child_by_field_name("function")
                .or_else(|| n.named_child(0))
                .map_or_else(Vec::new, |c| self.parts(c)),
            _ => vec![],
        }
    }
    pub(super) fn define(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        name: String,
        kind: &str,
        global: bool,
    ) -> usize {
        let prefix = if global {
            name.clone()
        } else {
            format!("{}.{}", self.prefixes[scope], name)
        };
        let key = self.key(&prefix);
        let index = self.e.define(
            n,
            scope,
            &name,
            kind,
            Some(key.clone()),
            !matches!(kind, "module" | "namespace"),
        );
        self.prefixes.push(prefix);
        if matches!(kind, "module" | "namespace") {
            self.e.bind(
                scope,
                &name,
                Binding::Namespace {
                    prefixes: vec![format!("{key}.")],
                    separator: ".",
                },
            );
        }
        index
    }
    pub(super) fn unresolved(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        label: String,
        relation: &str,
    ) {
        self.e.reference(
            n,
            scope,
            label,
            relation,
            vec![],
            "receiver, namespace, or binding context is unknown",
        );
    }
    pub(super) fn target(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        target: Syntax<'t>,
        relation: &'static str,
    ) {
        self.pending
            .push((n, scope, self.text(target), relation, self.parts(target)));
    }
    pub(super) fn call(&mut self, n: Syntax<'t>, scope: usize, target: Syntax<'t>, dynamic: bool) {
        let mut parts = if dynamic { vec![] } else { self.parts(target) };
        if parts.len() == 1
            && matches!(
                self.e.language,
                "scala" | "dart" | "groovy" | "apex" | "objc" | "dm"
            )
        {
            let mut parent = Some(scope);
            while let Some(s) = parent {
                if self.e.scopes[s].class {
                    parts.clear();
                    break;
                }
                parent = self.e.scopes[s].parent;
            }
        }
        if self.e.language == "ocaml" {
            let mut keys = self.e.resolve(scope, &parts);
            if keys.is_empty() {
                keys = self.ocaml_external(scope, &parts, false);
            }
            self.e.reference(
                n,
                scope,
                self.text(target),
                "calls",
                keys,
                "target is unavailable, dynamic, or not yet bound",
            );
        } else {
            self.e.call(n, scope, target, Some(parts));
        }
    }
    pub(super) fn walk(&mut self, n: Syntax<'t>, scope: usize) {
        for c in children(n) {
            self.visit(c, scope);
        }
    }
    pub(super) fn mask(&mut self, n: Syntax<'t>, scope: usize) {
        if self.e.language == "pascal"
            && matches!(n.kind(), "declArg" | "declVar" | "declField" | "declProp")
        {
            let mut cursor = n.walk();
            for name in n
                .children_by_field_name("name", &mut cursor)
                .filter(|n| names(*n))
            {
                self.mask(name, scope);
            }
            return;
        }
        if names(n) {
            let name = self.text(n);
            self.e.bind(scope, &name, Binding::Unknown);
            if matches!(self.e.language, "dart" | "apex") {
                self.value_types.insert((scope, name), None);
            }
        } else if let Some(name) = field(n, &["name", "pattern", "declarator"]) {
            self.mask(name, scope);
        } else {
            for c in children(n) {
                self.mask(c, scope);
            }
        }
    }
    pub(super) fn function(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        name: Syntax<'t>,
        header: Syntax<'t>,
        body: Option<Syntax<'t>>,
    ) {
        let s = self.define(
            n,
            scope,
            self.text(name),
            if self.e.scopes[scope].class {
                "method"
            } else {
                "function"
            },
            false,
        );
        if self.e.language == "pascal" {
            let parts = self.parts(name);
            let owner = if parts.len() > 1 {
                Some(parts[..parts.len() - 1].join("."))
            } else {
                self.owner_metadata(scope, "pascal_class")
            };
            if let (Some(owner), Some(method)) = (owner, parts.last()) {
                self.annotate(s, serde_json::json!({"pascal_owner":owner, "pascal_method":method, "pascal_unit":self.owner_metadata(scope, "pascal_unit"), "body":n.kind() == "defProc"}));
            }
        }
        if let Some(params) = field(header, &["parameters", "args", "lambda_list"]).or_else(|| {
            child(
                header,
                &[
                    "parameters",
                    "proc_parameters",
                    "formal_parameters",
                    "argument_list",
                    "tf_port_list",
                ],
            )
        }) {
            self.mask(params, s);
        }
        for p in children(header)
            .into_iter()
            .filter(|c| c.kind() == "parameter")
        {
            self.mask(p, s);
        }
        if let Some(parameters) = self.groovy_parameters.get(&name.start_byte()) {
            for parameter in parameters {
                self.e.bind(s, parameter, Binding::Unknown);
            }
        }
        if self.e.language == "ocaml" && n.kind() == "let_binding" {
            let recursive = n.parent().is_some_and(|p| {
                let mut cursor = p.walk();
                p.children(&mut cursor).any(|c| c.kind() == "rec")
            });
            if !recursive {
                let name = self.text(name);
                self.e.bind(s, &name, Binding::Unknown);
            }
        }
        self.declaration_annotations(n, s);
        if matches!(self.e.language, "dart" | "apex") {
            self.parameter_types(header, s);
        }
        for f in ["return_type", "type"] {
            if let Some(ty) = header.child_by_field_name(f) {
                self.heritage(ty, s, "uses_type");
            }
        }
        if self.e.language == "pascal" && n.kind() == "defProc" {
            let mut cursor = n.walk();
            for local in n.children_by_field_name("local", &mut cursor) {
                self.visit(local, s);
            }
        }
        if let Some(body) = body {
            self.visit(body, s);
        } else {
            for c in children(n) {
                if c.id() != header.id() && c.id() != name.id() && !c.kind().ends_with("statement")
                {
                    self.visit(c, s);
                }
            }
        }
    }
    pub(super) fn heritage(&mut self, n: Syntax<'t>, scope: usize, relation: &'static str) {
        if self.e.language == "scala" && n.kind() == "extends_clause" {
            let mut relation = "inherits";
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                if c.kind() == "with" {
                    relation = "mixes_in";
                } else if c.is_named() && !matches!(c.kind(), "arguments" | "type_arguments") {
                    self.heritage(c, scope, relation);
                }
            }
            return;
        }
        let relation = if self.e.language == "dart" && n.kind() == "mixins" {
            "mixes_in"
        } else {
            relation
        };
        if !self.parts(n).is_empty() {
            self.target(n, scope, n, relation);
        } else {
            for c in children(n) {
                self.heritage(c, scope, relation);
            }
        }
    }
    pub(super) fn import(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        label: String,
        key: Option<String>,
    ) {
        self.e.reference(
            n,
            scope,
            label,
            "imports",
            key.into_iter().collect(),
            "import target is unavailable or ambiguous",
        );
    }
    pub(super) fn file_import(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        value: &str,
        alias: Option<String>,
    ) {
        let value = value.trim_matches(['\'', '"']);
        let key = if !value.contains(':') && !value.starts_with('/') {
            relative_path(
                self.e.facts.path.rsplit_once('/').map_or("", |p| p.0),
                value,
            )
        } else {
            None
        };
        self.import(
            n,
            scope,
            value.into(),
            key.as_ref()
                .map(|p| format!("{}:file:{p}", self.e.language)),
        );
        if let (Some(alias), Some(path)) = (alias, key) {
            self.e.bind(
                scope,
                &alias,
                Binding::Namespace {
                    prefixes: vec![self.key(&format!("@{path}."))],
                    separator: ".",
                },
            );
        }
    }
}
