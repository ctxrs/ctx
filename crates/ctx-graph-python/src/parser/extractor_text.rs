use super::*;

impl Extractor<'_> {
    pub(super) fn text(&self, node: Syntax<'_>) -> &str {
        &self.source[node.byte_range()]
    }

    pub(super) fn reference(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        owner: &str,
        relation: &'static str,
        context: &'static str,
    ) {
        if let Some(parts) = self.dotted(node) {
            self.evidence.push(PendingReference {
                site: PendingCall {
                    scope,
                    start: node.start_byte(),
                    end: node.end_byte(),
                    line: line(node),
                    parts,
                },
                owner: owner.into(),
                relation,
                context,
            });
        }
    }

    pub(super) fn type_references(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        owner: &str,
        context: &'static str,
    ) {
        if self.dotted(node).is_some() {
            self.reference(node, scope, owner, "references", context);
            return;
        }
        if node.kind() == "string" {
            // Forward references are syntax evidence only. Parse a plain quoted
            // name with the same grammar; escaped or computed strings stay opaque.
            if let Some(text) = self.string_text(node).filter(|s| !s.contains('\\')) {
                let mut parser = Parser::new();
                if parser
                    .set_language(&tree_sitter_python::LANGUAGE.into())
                    .is_ok()
                    && let Some(tree) = parser
                        .parse(&text, None)
                        .filter(|t| !t.root_node().has_error())
                {
                    let root = tree.root_node();
                    if root.named_child_count() == 1
                        && let Some(expression) = root.named_child(0).and_then(|n| n.named_child(0))
                        && let Some(parts) = dotted_text(expression, &text)
                    {
                        self.evidence.push(PendingReference {
                            site: PendingCall {
                                scope,
                                start: node.start_byte(),
                                end: node.end_byte(),
                                line: line(node),
                                parts,
                            },
                            owner: owner.into(),
                            relation: "references",
                            context,
                        });
                    }
                }
            }
            return;
        }
        // These expressions contain values or bind names; traversing them as types
        // would invent type dependencies (and annotations are never CALLS here).
        if matches!(
            node.kind(),
            "call"
                | "lambda"
                | "string"
                | "concatenated_string"
                | "list_comprehension"
                | "dictionary_comprehension"
                | "constrained_type"
        ) {
            return;
        }
        if matches!(node.kind(), "subscript" | "generic_type") {
            let head = node
                .child_by_field_name("value")
                .or_else(|| node.named_child(0));
            if let Some(head) = head {
                self.type_references(head, scope, owner, context);
                // Literal arguments are values; Annotated arguments after the
                // first are metadata. Avoid interpreting either as type names.
                let head_parts = self.dotted(head).unwrap_or_default();
                let special = head_parts.last().map(String::as_str);
                if special == Some("Literal") {
                    return;
                }
                let mut cursor = node.walk();
                let arguments: Vec<_> = node
                    .named_children(&mut cursor)
                    .filter(|n| n.id() != head.id())
                    .collect();
                for argument in arguments {
                    if argument.kind() == "type_parameter" {
                        let mut cursor = argument.walk();
                        for (index, item) in argument.named_children(&mut cursor).enumerate() {
                            if special == Some("Annotated") && index > 0 {
                                break;
                            }
                            self.type_references(item, scope, owner, "generic_arg");
                        }
                    } else {
                        self.type_references(argument, scope, owner, "generic_arg");
                        if special == Some("Annotated") {
                            break;
                        }
                    }
                }
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.type_references(child, scope, owner, context);
        }
    }

    pub(super) fn string_text(&self, node: Syntax<'_>) -> Option<String> {
        if node.kind() == "parenthesized_expression" && node.named_child_count() == 1 {
            return self.string_text(node.named_child(0)?);
        }
        if node.kind() == "concatenated_string" {
            let mut text = String::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                text.push_str(&self.string_text(child)?);
            }
            return Some(text);
        }
        if node.kind() != "string" {
            return None;
        }
        let raw = self.text(node);
        let quote = raw.find(['\'', '"'])?;
        if raw[..quote]
            .bytes()
            .any(|b| matches!(b, b'b' | b'B' | b'f' | b'F'))
        {
            return None;
        }
        let width = if raw[quote..].starts_with("\"\"\"") || raw[quote..].starts_with("\'\'\'") {
            3
        } else {
            1
        };
        Some(raw[quote + width..raw.len() - width].to_owned())
    }

    pub(super) fn docstring(&mut self, body: Syntax<'_>, scope: usize) {
        let mut cursor = body.walk();
        let first = body
            .named_children(&mut cursor)
            .find(|n| n.kind() != "comment");
        if let Some(statement) = first.filter(|n| n.kind() == "expression_statement")
            && let Some(string) = statement.named_child(0)
            && let Some(text) = self
                .string_text(string)
                .filter(|s| s.trim().chars().count() > 20)
        {
            self.rationale(string, text.trim().into(), scope, "docstring");
        }
    }

    pub(super) fn rationale(&mut self, node: Syntax<'_>, text: String, scope: usize, kind: &str) {
        let id = format!("python:{}:rationale@{}", self.facts.path, node.start_byte());
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let label = if flat.chars().count() > 80 {
            let prefix: String = flat.chars().take(79).collect();
            let cut = if flat.chars().nth(79) == Some(' ') {
                prefix.as_str()
            } else {
                prefix
                    .rsplit_once(' ')
                    .map_or(prefix.as_str(), |(words, _)| words)
            };
            format!("{}…", cut.trim_end())
        } else {
            flat
        };
        self.facts.nodes.push(Node {
            id: id.clone(),
            label,
            kind: "rationale".into(),
            file: self.facts.path.clone(),
            line: Some(line(node)),
            end_line: Some(line_end(node)),
            qualified_name: None,
            binding_key: None,
            metadata: json!({"kind": kind, "text": text, "source_text": self.text(node)}),
        });
        self.facts.edges.push(Edge {
            id: format!("rationale_for:{id}"),
            source: id,
            target: self.scopes[scope].owner.clone(),
            relation: "rationale_for".into(),
            directed: true,
            file: Some(self.facts.path.clone()),
            line: Some(line(node)),
            confidence: "static".into(),
            metadata: json!({"context": kind}),
        });
    }

    pub(super) fn generated_module(&self, root: Syntax<'_>) -> bool {
        let head: String = self.source.chars().take(2048).collect();
        if [
            "DO NOT EDIT",
            "@generated",
            "Generated by the protocol buffer",
        ]
        .iter()
        .any(|marker| head.contains(marker))
        {
            return true;
        }
        let mut revision = false;
        let mut down_revision = false;
        let mut upgrade = false;
        let mut cursor = root.walk();
        for statement in root.named_children(&mut cursor) {
            if statement.kind() == "function_definition" {
                upgrade |= statement
                    .child_by_field_name("name")
                    .is_some_and(|n| self.text(n) == "upgrade");
            }
            if statement.kind() == "expression_statement"
                && let Some(assignment) = statement
                    .named_child(0)
                    .filter(|n| n.kind() == "assignment")
                && let Some(name) = assignment.child_by_field_name("left")
            {
                revision |= self.text(name) == "revision";
                down_revision |= self.text(name) == "down_revision";
            }
            if statement.kind() == "class_definition"
                && statement
                    .child_by_field_name("name")
                    .is_some_and(|n| self.text(n) == "Migration")
                && statement
                    .child_by_field_name("superclasses")
                    .is_some_and(|n| self.text(n).contains("migrations.Migration"))
                && statement
                    .child_by_field_name("body")
                    .is_some_and(|n| self.text(n).contains("operations"))
            {
                return true;
            }
        }
        revision && down_revision && upgrade
    }

    pub(super) fn decorator_noise(&self, site: &PendingCall, key: Option<&str>) -> bool {
        if matches!(
            key,
            Some(
                "python:dataclasses:dataclass"
                    | "python:functools:wraps"
                    | "python:functools:lru_cache"
                    | "python:functools:cache"
                    | "python:abc:abstractmethod"
            )
        ) {
            return true;
        }
        if site.parts.len() != 1
            || !matches!(
                site.parts[0].as_str(),
                "property" | "staticmethod" | "classmethod"
            )
        {
            return false;
        }
        let mut current = Some(site.scope);
        while let Some(index) = current {
            if self.scopes[index].uncertain
                || self.scopes[index].bindings.contains_key(&site.parts[0])
            {
                return false;
            }
            current = self.scopes[index].parent;
        }
        true
    }

    pub(super) fn bind(&mut self, scope: usize, name: String, binding: Binding) {
        self.scopes[scope]
            .bindings
            .entry(identifier(&name))
            .and_modify(|b| *b = Binding::Unknown)
            .or_insert(binding);
    }

    pub(super) fn target(&mut self, node: Syntax<'_>, scope: usize) {
        match node.kind() {
            "identifier" => self.bind(scope, self.text(node).into(), Binding::Unknown),
            "attribute" => {
                if let Some(parts) = self.dotted(node)
                    && let Some((class, _)) = self.receiver(scope, &identifier(&parts[0]))
                {
                    self.receiver_writes.entry(class).or_default().insert(
                        if parts[1] == "__dict__" {
                            "*".into()
                        } else {
                            identifier(&parts[1])
                        },
                    );
                    return;
                }
                if let Some(object) = node.child_by_field_name("object") {
                    self.target(object, scope);
                }
            }
            "subscript" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.target(value, scope);
                }
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.target(child, scope);
                }
            }
        }
    }

    pub(super) fn parameters(&mut self, node: Syntax<'_>, scope: usize) {
        for name in parameter_names(node) {
            self.target(name, scope);
        }
    }

    pub(super) fn visit(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        if node.kind() == "type" {
            self.type_references(
                node,
                scope,
                &self.scopes[scope].owner.clone(),
                "variable_type",
            );
            return;
        }
        match node.kind() {
            "comment" => {
                if [
                    "# NOTE:",
                    "# IMPORTANT:",
                    "# HACK:",
                    "# WHY:",
                    "# RATIONALE:",
                    "# TODO:",
                    "# FIXME:",
                ]
                .iter()
                .any(|prefix| self.text(node).starts_with(prefix))
                {
                    self.rationale(node, self.text(node).to_owned(), scope, "comment");
                }
                return;
            }
            "decorated_definition" => {
                if let Some(definition) = node.child_by_field_name("definition") {
                    let mut cursor = node.walk();
                    let decorators: Vec<_> = node
                        .named_children(&mut cursor)
                        .filter(|n| n.kind() == "decorator")
                        .collect();
                    let builtin = decorators.len() == 1
                        && self.scopes[scope].kind == ScopeKind::Class
                        && decorators[0].named_child(0).is_some_and(|n| {
                            n.kind() == "identifier"
                                && matches!(self.text(n), "staticmethod" | "classmethod")
                        });
                    let owner = self.definition_id(definition, scope);
                    if builtin && !conditional {
                        self.builtin_methods.push((
                            scope,
                            self.text(decorators[0].named_child(0).unwrap()).into(),
                            owner.clone(),
                        ));
                    }
                    for decorator in decorators {
                        if let Some(expression) = decorator.named_child(0) {
                            let head = expression
                                .child_by_field_name("function")
                                .unwrap_or(expression);
                            self.reference(head, scope, &owner, "references", "decorator");
                            self.visit(expression, scope, conditional);
                        }
                    }
                    self.definition(definition, scope, conditional || !builtin);
                }
                return;
            }
            "function_definition" | "class_definition" => {
                self.definition(node, scope, conditional);
                return;
            }
            "import_statement" | "import_from_statement" => {
                self.import(node, scope, conditional);
                return;
            }
            "lambda"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => {
                // ponytail: anonymous scopes retain call sites, but do not infer their bindings.
                let child = self.scopes.len();
                self.scopes.push(Scope {
                    parent: Some(scope),
                    kind: ScopeKind::Opaque,
                    owner: self.scopes[scope].owner.clone(),
                    qualified: self.scopes[scope].qualified.clone(),
                    bindings: HashMap::new(),
                    uncertain: true,
                });
                let mut cursor = node.walk();
                for n in node.named_children(&mut cursor) {
                    self.visit(n, child, true);
                }
                return;
            }
            "assignment"
            | "augmented_assignment"
            | "type_alias_statement"
            | "for_statement"
            | "for_in_clause" => {
                if let Some(target) = node.child_by_field_name("left") {
                    self.target(target, scope);
                }
            }
            "named_expression" => {
                if let Some(target) = node.child_by_field_name("name") {
                    self.target(target, scope);
                    if self.scopes[scope].kind == ScopeKind::Opaque {
                        let mut parent = self.scopes[scope].parent;
                        while let Some(index) = parent {
                            self.target(target, index);
                            if self.scopes[index].kind != ScopeKind::Opaque {
                                break;
                            }
                            parent = self.scopes[index].parent;
                        }
                    }
                }
            }
            "as_pattern" | "except_clause" => {
                if let Some(target) = node.child_by_field_name("alias") {
                    self.target(target, scope);
                }
            }
            "delete_statement" | "global_statement" | "nonlocal_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.target(child, scope);
                }
                // A declaration can allow writes to an enclosing binding. Do not export a guessed target.
                if node.kind() != "delete_statement" {
                    let mut parent = self.scopes[scope].parent;
                    while let Some(index) = parent {
                        let mut cursor = node.walk();
                        for child in node.named_children(&mut cursor) {
                            self.target(child, index);
                        }
                        parent = self.scopes[index].parent;
                    }
                }
            }
            "case_clause" => self.scopes[scope].uncertain = true,
            "exec_statement" => self.scopes[scope].uncertain = true,
            "call" => {
                let parts = node
                    .child_by_field_name("function")
                    .and_then(|n| self.call_parts(n))
                    .unwrap_or_default();
                let function = parts.first().map(|name| identifier(name));
                if parts.len() == 1
                    && matches!(function.as_deref(), Some("exec" | "globals" | "locals"))
                {
                    self.scopes[scope].uncertain = true;
                    if function.as_deref() == Some("globals") {
                        self.scopes[0].uncertain = true;
                    }
                }
                if parts.len() == 1
                    && matches!(function.as_deref(), Some("setattr" | "delattr"))
                    && let Some(arguments) = node.child_by_field_name("arguments")
                    && let Some(receiver) = arguments.named_child(0)
                    && receiver.kind() == "identifier"
                    && let Some((class, _)) = self.receiver(scope, &identifier(self.text(receiver)))
                {
                    // A computed name can replace any member. Even a shadowed
                    // setter is not evidence that the receiver stays unchanged.
                    let name = arguments.named_child(1).and_then(|n| self.string_text(n));
                    self.receiver_writes.entry(class).or_default().insert(
                        name.filter(|n| !n.contains('\\'))
                            .unwrap_or_else(|| "*".into()),
                    );
                }
                self.calls.push(PendingCall {
                    scope,
                    start: node.start_byte(),
                    end: node.end_byte(),
                    line: line(node),
                    parts,
                });
            }
            _ => {}
        }
        let conditional = conditional
            || matches!(
                node.kind(),
                "if_statement"
                    | "for_statement"
                    | "while_statement"
                    | "try_statement"
                    | "with_statement"
                    | "match_statement"
                    | "decorated_definition"
            );
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child, scope, conditional);
        }
    }

    pub(super) fn definition_id(&self, node: Syntax<'_>, scope: usize) -> String {
        let name = self.text(node.child_by_field_name("name").unwrap());
        let prefix = &self.scopes[scope].qualified;
        let qualified = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };
        format!(
            "python:{}:{qualified}@{}",
            self.facts.path,
            node.start_byte()
        )
    }

    pub(super) fn definition(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        let name = self
            .text(node.child_by_field_name("name").unwrap())
            .to_owned();
        let qualified = if self.scopes[scope].qualified.is_empty() {
            name.clone()
        } else {
            format!("{}.{}", self.scopes[scope].qualified, name)
        };
        let id = format!(
            "python:{}:{}@{}",
            self.facts.path,
            qualified,
            node.start_byte()
        );
        let key = format!("python:{}:{}", self.facts.module, identifier(&qualified));
        let class = node.kind() == "class_definition";
        self.bind(
            scope,
            name.clone(),
            if conditional {
                Binding::Unknown
            } else {
                Binding::Definition {
                    key: key.clone(),
                    id: id.clone(),
                    start: node.end_byte(),
                }
            },
        );
        self.facts.nodes.push(Node {
            id: id.clone(),
            label: name,
            kind: if class {
                "class"
            } else if self.scopes[scope].kind == ScopeKind::Class {
                "method"
            } else {
                "function"
            }
            .into(),
            file: self.facts.path.clone(),
            line: Some(line(node)),
            end_line: Some(line_end(node)),
            qualified_name: Some(identifier(&qualified)),
            binding_key: Some(key),
            metadata: Value::Null,
        });
        self.facts.edges.push(Edge {
            id: format!("contains:{id}"),
            source: self.scopes[scope].owner.clone(),
            target: id.clone(),
            relation: "contains".into(),
            directed: true,
            file: Some(self.facts.path.clone()),
            line: Some(line(node)),
            confidence: "static".into(),
            metadata: Value::Null,
        });
        let child = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(scope),
            kind: if class {
                ScopeKind::Class
            } else {
                ScopeKind::Function
            },
            owner: id.clone(),
            qualified,
            bindings: HashMap::new(),
            uncertain: false,
        });
        if node.child_by_field_name("type_parameters").is_some() {
            self.scopes[child].uncertain = true;
        }
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.parameters(parameters, child);
            if !class
                && !conditional
                && self.scopes[scope].kind == ScopeKind::Class
                && !self
                    .builtin_methods
                    .iter()
                    .any(|(_, name, owner)| owner == &id && name == "staticmethod")
                && let Some(first) = parameters.named_child(0)
                && matches!(
                    first.kind(),
                    "identifier"
                        | "typed_parameter"
                        | "default_parameter"
                        | "typed_default_parameter"
                )
                && let Some(name) = parameter_names(first).first()
                && name.kind() == "identifier"
            {
                let name = identifier(self.text(*name));
                self.scopes[child].bindings.insert(
                    name,
                    Binding::Receiver {
                        class: scope,
                        method: child,
                    },
                );
            }
        }
        let body = node.child_by_field_name("body").unwrap();
        self.docstring(body, child);
        if let Some(parameters) = node.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                if let Some(annotation) = parameter.child_by_field_name("type") {
                    self.type_references(annotation, scope, &id, "parameter_type");
                }
            }
        }
        if let Some(annotation) = node.child_by_field_name("return_type") {
            self.type_references(annotation, scope, &id, "return_type");
        }
        if class {
            let literal = node
                .child_by_field_name("superclasses")
                .is_none_or(|bases| {
                    let mut cursor = bases.walk();
                    bases
                        .named_children(&mut cursor)
                        .filter(|n| n.kind() != "comment")
                        .all(|base| self.dotted(base).is_some())
                });
            self.facts
                .nodes
                .iter_mut()
                .find(|n| n.id == id)
                .unwrap()
                .metadata = json!({"python_literal_bases": literal});
        }
        if let Some(bases) = node.child_by_field_name("superclasses") {
            let mut cursor = bases.walk();
            for base in bases.named_children(&mut cursor) {
                if !matches!(
                    base.kind(),
                    "keyword_argument" | "list_splat" | "dictionary_splat"
                ) {
                    let head = base.child_by_field_name("value").unwrap_or(base);
                    self.reference(head, scope, &id, "inherits", "base_class");
                    if base.kind() == "subscript" {
                        let mut cursor = base.walk();
                        for argument in base.children_by_field_name("subscript", &mut cursor) {
                            self.type_references(argument, scope, &id, "generic_arg");
                        }
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for n in node.named_children(&mut cursor) {
            if n.id() == body.id() {
                self.visit(n, child, false);
            } else if n.kind() == "parameters" {
                let mut cursor = n.walk();
                for parameter in n.named_children(&mut cursor) {
                    if let Some(default) = parameter.child_by_field_name("value") {
                        self.visit(default, scope, conditional);
                    }
                }
            } else if n.kind() != "type" {
                self.visit(n, scope, conditional);
            }
        }
        if !class {
            self.collect_callable_flow(node, body, child);
        }
    }
}
