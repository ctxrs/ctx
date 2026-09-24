use super::*;

impl<'s, 't> Extended<'s, 't> {
    pub(super) fn imports(&mut self, n: Syntax<'t>, scope: usize) -> bool {
        let k = n.kind();
        match (self.e.language, k) {
            ("scala" | "groovy", "import_declaration") => {
                let parts: Vec<_> = children(n)
                    .into_iter()
                    .filter(|c| matches!(c.kind(), "identifier" | "scoped_identifier"))
                    .flat_map(|c| self.parts(c))
                    .collect();
                if !parts.is_empty() {
                    let full = parts.join(".");
                    let key = self.key(&full);
                    self.import(n, scope, full, Some(key.clone()));
                    if !children(n).iter().any(|c| {
                        matches!(
                            c.kind(),
                            "namespace_selectors"
                                | "namespace_wildcard"
                                | "asterisk"
                                | "as_renamed_identifier"
                        )
                    }) {
                        self.e.bind(
                            scope,
                            parts.last().unwrap(),
                            Binding::Namespace {
                                prefixes: vec![format!("{key}.")],
                                separator: ".",
                            },
                        );
                    }
                }
                true
            }
            ("dart", "import_specification" | "part_directive" | "part_of_directive") => {
                if let Some(uri) = find(n, &["string_literal"]) {
                    self.file_import(
                        n,
                        scope,
                        self.e.text(uri),
                        n.child_by_field_name("alias").map(|n| self.text(n)),
                    );
                } else {
                    self.unresolved(n, scope, self.text(n), "imports");
                }
                true
            }
            ("objc", "preproc_include") => {
                if let Some(p) = n.child_by_field_name("path") {
                    if p.kind() == "string_literal" {
                        self.file_import(n, scope, self.e.text(p), None);
                    } else {
                        self.import(n, scope, self.text(p), None);
                    }
                }
                true
            }
            ("objc", "module_import") => {
                if let Some(p) = n.child_by_field_name("path") {
                    self.import(n, scope, self.text(p), None);
                }
                true
            }
            ("dm", "preproc_include") => {
                if let Some(p) = n.child_by_field_name("file") {
                    self.file_import(n, scope, self.e.text(p), None);
                }
                true
            }
            ("fortran", "use_statement") => {
                if let Some(module) = child(n, &["module_name"]) {
                    let module = self.text(module);
                    self.import(n, scope, module.clone(), Some(self.key(&module)));
                    if let Some(items) = child(n, &["included_items"]) {
                        for item in children(items) {
                            if item.kind() == "identifier" {
                                let name = self.text(item);
                                self.e.bind(
                                    scope,
                                    &name,
                                    Binding::symbol(self.key(&format!("{module}.{name}"))),
                                );
                            } else if item.kind() == "use_alias" {
                                let ids: Vec<_> =
                                    children(item).into_iter().filter(|c| names(*c)).collect();
                                if ids.len() == 2 {
                                    let alias = self.text(ids[0]);
                                    let name = self.text(ids[1]);
                                    self.e.bind(
                                        scope,
                                        &alias,
                                        Binding::symbol(self.key(&format!("{module}.{name}"))),
                                    );
                                }
                            }
                        }
                    }
                }
                true
            }
            ("julia", "using_statement" | "import_statement") => {
                for item in children(n) {
                    if item.kind() == "selected_import" {
                        let items = children(item);
                        if let Some(module) = items.first() {
                            let module = self.text(*module);
                            self.import(n, scope, module.clone(), Some(self.key(&module)));
                            if !module.starts_with('.') {
                                for item in items.iter().skip(1) {
                                    if names(*item) {
                                        let name = self.text(*item);
                                        self.e.bind(
                                            scope,
                                            &name,
                                            Binding::symbol(self.key(&format!("{module}.{name}"))),
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        let parts = self.parts(item);
                        let name = parts.join(".");
                        if !name.is_empty() {
                            self.import(n, scope, self.text(item), Some(self.key(&name)));
                            if !self.e.text(item).starts_with('.') {
                                self.e.bind(
                                    scope,
                                    parts.last().unwrap(),
                                    Binding::Namespace {
                                        prefixes: vec![format!("{}.", self.key(&name))],
                                        separator: ".",
                                    },
                                );
                            }
                        }
                    }
                }
                true
            }
            ("ocaml", "open_module" | "open_module_signature" | "include_module") => {
                if let Some(m) = n.child_by_field_name("module") {
                    let parts = self.parts(m);
                    let mut keys = self.e.resolve(scope, &parts);
                    if keys.is_empty() {
                        keys = self.ocaml_external(scope, &parts, true);
                    }
                    self.e.reference(n, scope, self.text(m), "imports", keys, "explicit sibling compilation-unit candidate; load-path overrides are not inferred");
                }
                true
            }
            ("pascal", "declUses") => {
                for m in children(n).into_iter().filter(|c| c.kind() == "moduleName") {
                    let name = self.text(m);
                    self.import(m, scope, name.clone(), Some(self.key(&name)));
                    self.e.bind(
                        scope,
                        &name,
                        Binding::Namespace {
                            prefixes: vec![format!("{}.", self.key(&name))],
                            separator: ".",
                        },
                    );
                }
                true
            }
            ("verilog", "package_import_declaration") => {
                for item in children(n) {
                    if let Some(p) = child(item, &["package_identifier"]) {
                        let package = self.text(p);
                        self.import(item, scope, package.clone(), Some(self.key(&package)));
                        if let Some(name) =
                            child(item, &["simple_identifier", "escaped_identifier"])
                        {
                            let name = self.text(name);
                            self.e.bind(
                                scope,
                                &name,
                                Binding::symbol(self.key(&format!("{package}.{name}"))),
                            );
                        }
                    }
                }
                true
            }
            ("zig", "builtin_function") => {
                if let Some(name) = child(n, &["builtin_identifier"])
                    && matches!(self.e.text(name), "@import" | "@cImport" | "@cInclude")
                {
                    if let Some(value) = find(n, &["string"]) {
                        self.file_import(n, scope, self.e.text(value), None);
                    } else {
                        self.unresolved(n, scope, self.text(name), "imports");
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }
    pub(super) fn selector(&self, n: Syntax<'_>, message: bool) -> String {
        let mut result = String::new();
        let mut cursor = n.walk();
        for (i, c) in n.children(&mut cursor).enumerate() {
            let is_method = if message {
                n.field_name_for_child(i as u32) == Some("method")
            } else {
                c.kind() == "identifier"
            };
            if is_method {
                result.push_str(self.e.text(c));
                if self.e.source[c.end_byte()..n.end_byte()]
                    .trim_start()
                    .starts_with(':')
                    || (!message
                        && c.next_named_sibling()
                            .is_some_and(|s| s.kind() == "method_parameter"))
                {
                    result.push(':');
                }
            }
        }
        result
    }
    pub(super) fn lisp(&mut self, n: Syntax<'t>, scope: usize) {
        if n.kind() == "include_reader_macro" {
            // Retain the written form without evaluating the reader feature.
            // An uncertain scope prevents conditional definitions leaking into
            // unconditional bindings or calls to an arbitrary active branch.
            let first = self.e.facts.nodes.len();
            let first_reference = self.e.facts.references.len();
            let s = self.e.block(scope, n);
            self.e.scopes[s].uncertain = true;
            self.prefixes.push(self.prefixes[scope].clone());
            let mut cursor = n.walk();
            for target in n
                .children_by_field_name("target", &mut cursor)
                .filter(|n| n.is_named())
            {
                self.visit(target, s);
            }
            let condition = serde_json::json!({
                "marker": n.child_by_field_name("marker").map(|v| self.e.text(v)),
                "feature": n.child_by_field_name("condition").map(|v| self.e.text(v)),
            });
            for node in &mut self.e.facts.nodes[first..] {
                node.binding_key = None;
                if !node.metadata["reader_conditions"].is_array() {
                    node.metadata["reader_conditions"] = serde_json::json!([]);
                }
                node.metadata["reader_conditions"]
                    .as_array_mut()
                    .unwrap()
                    .push(condition.clone());
            }
            for reference in &mut self.e.facts.references[first_reference..] {
                reference.candidate_keys.clear();
            }
            return;
        }
        if n.kind() == "defun" {
            if let Some(h) = child(n, &["defun_header"])
                && let Some(name) = h.child_by_field_name("function_name")
            {
                let keyword = h
                    .child_by_field_name("keyword")
                    .map(|k| self.text(k))
                    .unwrap_or_default();
                let kind = if keyword == "defmacro" {
                    "macro"
                } else if keyword == "defmethod" {
                    "method"
                } else {
                    "function"
                };
                let s = self.define(n, scope, self.text(name), kind, false);
                if let Some(p) = h.child_by_field_name("lambda_list") {
                    self.mask(p, s);
                }
                if let Some(doc) = children(n)
                    .into_iter()
                    .find(|c| c.id() != h.id() && c.kind() != "comment")
                    && doc.kind() == "str_lit"
                {
                    self.lisp_rationale(doc, s);
                }
                for c in children(n) {
                    if c.id() != h.id() {
                        self.visit(c, s);
                    }
                }
            }
            return;
        }
        if n.kind() != "list_lit" {
            self.walk(n, scope);
            return;
        }
        let cs = children(n);
        let Some(head) = cs.first() else { return };
        if head.kind() == "defun" {
            self.visit(*head, scope);
            return;
        }
        let op = self.text(*head);
        if matches!(op.as_str(), "quote" | "function") {
            return;
        }
        if matches!(op.as_str(), "in-package" | "defpackage") {
            if let Some(name) = cs.get(1) {
                let package = self.text(*name).trim_matches([':', '"']).to_string();
                if op == "in-package" {
                    self.prefixes[scope] = package;
                } else {
                    let s = self.define(n, scope, package, "module", true);
                    for option in cs.iter().skip(2) {
                        let values = children(*option);
                        if values.first().is_some_and(|v| self.text(*v) == ":use") {
                            for value in values.iter().skip(1) {
                                let name = self.text(*value).trim_start_matches(':').to_string();
                                self.import(*value, s, name.clone(), Some(self.key(&name)));
                            }
                        }
                    }
                }
            }
            return;
        }
        if matches!(
            op.as_str(),
            "defclass" | "defstruct" | "defgeneric" | "defvar" | "defparameter" | "defconstant"
        ) || (op.starts_with("def")
            && !op.starts_with("default")
            && op != "define"
            && head.kind() == "sym_lit")
        {
            if let Some(name) = cs.get(1).filter(|n| names(**n)) {
                let kind = match op.as_str() {
                    "defclass" | "define-condition" => "class",
                    "defstruct" => "struct",
                    "deftype" => "type",
                    "defgeneric" => "function",
                    "defvar" | "defparameter" | "defconstant" => "variable",
                    _ if cs.get(2).is_some_and(|n| n.kind() == "list_lit") => "function",
                    _ => "variable",
                };
                let s = self.define(n, scope, self.text(*name), kind, false);
                if matches!(op.as_str(), "defvar" | "defparameter" | "defconstant") {
                    if let Some(doc) = cs.get(3).filter(|c| c.kind() == "str_lit") {
                        self.lisp_rationale(*doc, s);
                    }
                } else if op == "defstruct" {
                    if let Some(doc) = cs.get(2).filter(|c| c.kind() == "str_lit") {
                        self.lisp_rationale(*doc, s);
                    }
                } else if op == "deftype"
                    && let Some(doc) = cs.get(3).filter(|c| c.kind() == "str_lit")
                {
                    self.lisp_rationale(*doc, s);
                }
                for option in cs.iter().skip(2).filter(|c| c.kind() == "list_lit") {
                    let values = children(*option);
                    if values
                        .first()
                        .is_some_and(|v| self.text(*v) == ":documentation")
                        && let Some(doc) = values.get(1).filter(|c| c.kind() == "str_lit")
                    {
                        self.lisp_rationale(*doc, s);
                    }
                }
                if matches!(op.as_str(), "defclass" | "define-condition")
                    && let Some(bases) = cs.get(2)
                {
                    for base in children(*bases) {
                        self.target(base, s, base, "inherits");
                    }
                }
                if kind == "function" {
                    if let Some(params) = cs.get(2) {
                        self.mask(*params, s);
                    }
                    for c in cs.iter().skip(3) {
                        self.visit(*c, s);
                    }
                } else if kind == "variable" {
                    for c in cs.iter().skip(2) {
                        self.visit(*c, s);
                    }
                }
            }
            return;
        }
        if matches!(
            op.as_str(),
            "let" | "let*" | "flet" | "labels" | "lambda" | "macrolet" | "symbol-macrolet"
        ) {
            let index = self.e.block(scope, n);
            self.prefixes
                .push(format!("{}.<{}>", self.prefixes[scope], n.start_byte()));
            // Binding initializers and local macros require evaluation order and a
            // separate function namespace. Retain calls, but do not guess targets.
            self.e.scopes[index].uncertain = true;
            for c in cs.iter().skip(2) {
                self.visit(*c, index);
            }
            return;
        }
        if !matches!(
            op.as_str(),
            "if" | "when"
                | "unless"
                | "cond"
                | "case"
                | "progn"
                | "prog1"
                | "prog2"
                | "and"
                | "or"
                | "setq"
                | "setf"
                | "return"
                | "return-from"
                | "block"
                | "catch"
                | "throw"
                | "unwind-protect"
                | "tagbody"
                | "go"
                | "the"
                | "declare"
                | "declaim"
                | "eval-when"
                | "multiple-value-bind"
                | "dolist"
                | "dotimes"
                | "loop"
        ) {
            if head.kind() == "package_lit" {
                let text = self.text(*head);
                let p: Vec<_> = text.split(':').filter(|s| !s.is_empty()).collect();
                if p.len() == 2 {
                    self.e.reference(
                        n,
                        scope,
                        text.clone(),
                        "calls",
                        vec![self.key(&format!("{}.{}", p[0], p[1]))],
                        "package target is unavailable or ambiguous",
                    );
                }
            } else if head.kind() == "sym_lit" {
                self.call(n, scope, *head, false);
            }
        }
        for c in cs.iter().skip(1) {
            self.visit(*c, scope);
        }
    }
}
