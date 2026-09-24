use super::*;

impl<'s, 't> Compiled<'s, 't> {
    pub(super) fn name(&self, n: Syntax<'_>) -> Option<String> {
        if n.kind() == "field_expression" && self.e.language == "cpp" {
            let receiver = n.child_by_field_name("argument")?;
            let field = n.child_by_field_name("field")?;
            return self
                .name(receiver)
                .zip(self.name(field))
                .map(|(a, b)| format!("{a}.{b}"));
        }
        simple_name(self.e.text(n))
    }
    pub(super) fn key(&self, qualified: &str) -> String {
        self.member_key("symbol", qualified)
    }
    pub(super) fn member_key(&self, kind: &str, qualified: &str) -> String {
        for header in &self.includes {
            if let Some(symbol) = qualified.strip_prefix(&format!("@{header}.")) {
                return header_key(kind, header, symbol);
            }
        }
        format!("{}:{kind}:{qualified}", self.e.language)
    }
    pub(super) fn bind(&mut self, scope: usize, name: &str, value: Binding) {
        if self.e.language == "swift"
            && let Some(local) = self
                .swift_callables
                .locals
                .get_mut(&(scope, name.to_owned()))
        {
            local.blocked = true;
        }
        self.scopes
            .get_mut(&scope)
            .unwrap()
            .bindings
            .entry(name.into())
            .and_modify(|v| *v = Binding::Unknown)
            .or_insert(value);
    }
    pub(super) fn lookup(&self, mut scope: usize, name: &str) -> Option<&Binding> {
        loop {
            let ctx = &self.scopes[&scope];
            if ctx.uncertain {
                return Some(&Binding::Unknown);
            }
            if let Some(b) = ctx.bindings.get(name) {
                return Some(b);
            }
            scope = self.e.scopes[scope].parent?;
        }
    }
    // This lookup also sees uncertain closure scopes, solely to invalidate a
    // captured local. Positive function identity still requires normal lookup.
    pub(super) fn swift_binding_scope(&self, mut scope: usize, name: &str) -> Option<usize> {
        loop {
            if self.scopes[&scope].bindings.contains_key(name) {
                return Some(scope);
            }
            scope = self.e.scopes[scope].parent?;
        }
    }
    pub(super) fn swift_function_value(
        &self,
        scope: usize,
        n: Syntax<'_>,
    ) -> Option<SwiftFunctionValue> {
        if n.kind() != "simple_identifier" {
            return None;
        }
        let name = self.e.text(n);
        let Binding::Symbol(key) = self.lookup(scope, name)? else {
            return None;
        };
        let node = (*self.swift_callables.functions.get(key)?)?;
        Some(SwiftFunctionValue {
            scope: self.swift_binding_scope(scope, name)?,
            name: name.to_owned(),
            key: key.clone(),
            node,
        })
    }
    pub(super) fn swift_conditional_scope(&self, mut scope: usize) -> bool {
        loop {
            if self
                .swift_callables
                .conditional_depth
                .get(&scope)
                .is_some_and(|depth| *depth > 0)
            {
                return true;
            }
            let Some(parent) = self.e.scopes[scope].parent else {
                return false;
            };
            scope = parent;
        }
    }
    pub(super) fn swift_callable_read(&mut self, n: Syntax<'t>, scope: usize) {
        if n.kind() == "directive" {
            // Swift's #if markers are siblings, not containers for the guarded
            // statements. Decline the affected body's entire callable timeline.
            self.swift_callables
                .conditional_owners
                .insert(self.e.scopes[scope].owner.clone());
            let depth = self
                .swift_callables
                .conditional_depth
                .entry(scope)
                .or_default();
            match n.child(0).map(|c| c.kind()) {
                Some("#if") => *depth += 1,
                Some("#elseif" | "#else") => *depth = (*depth).max(1),
                Some("#endif") => *depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if n.kind() == "assignment"
            && let Some(name) = swift_assignment_name(n)
            && self.swift_binding_scope(scope, self.e.text(name)).is_none()
        {
            // The ordinary assignment visitor creates an unknown binding for
            // this spelling. Remember the write before that can hide a later
            // outer declaration captured by a nested function.
            self.swift_callables
                .unbound_writes
                .push((scope, self.e.text(name).to_owned()));
        }
        if n.kind() == "simple_identifier"
            && !n.parent().is_some_and(|p| p.kind() == "navigation_suffix")
        {
            // A member suffix is not a lexical read. Exclude it from both the
            // immediate capture check and the deferred capture/escape pass.
            // The navigation receiver still visits the normal lexical path.
            let name = self.e.text(n).to_owned();
            if let Some(binding_scope) = self.swift_binding_scope(scope, &name)
                && self.e.scopes[scope].owner != self.e.scopes[binding_scope].owner
                && let Some(local) = self.swift_callables.locals.get_mut(&(binding_scope, name))
            {
                local.blocked = true;
            }
            // Resolve captures after all declarations have been visited: nested
            // functions and closures do not execute on their textual timeline.
            self.swift_callables.uses.push((n, scope));
        }
        if n.kind() != "call_expression" {
            return;
        }
        let Some(target) = n.named_child(0).filter(|c| c.kind() == "simple_identifier") else {
            return;
        };
        let name = self.e.text(target).to_owned();
        let Some(binding_scope) = self.swift_binding_scope(scope, &name) else {
            return;
        };
        let binding = (binding_scope, name);
        let Some(local) = self.swift_callables.locals.get(&binding) else {
            return;
        };
        // Swift represents subscripts as call expressions too. Only an ordinary
        // parenthesized invocation, directly in this function body, has proof.
        let ordinary = n.named_child(1).is_some_and(|suffix| {
            suffix.kind() == "call_suffix"
                && suffix.named_child_count() == 1
                && suffix.named_child(0).is_some_and(|args| {
                    args.kind() == "value_arguments"
                        && args.child(0).is_some_and(|c| c.kind() == "(")
                })
        });
        let statement = n
            .parent()
            .filter(|p| {
                p.kind() == "control_transfer_statement"
                    && p.child(0).is_some_and(|c| c.kind() == "return")
                    && p.child_by_field_name("result") == Some(n)
            })
            .unwrap_or(n);
        let value = if ordinary && swift_body_statement(statement) && !self.uncertain(scope) {
            local.value.clone()
        } else {
            None
        };
        self.swift_callables.calls.insert(
            (scope, n.start_byte()),
            SwiftCallableCall {
                binding,
                assignment: local.assignment,
                value,
            },
        );
    }
    pub(super) fn swift_callable_write(&mut self, n: Syntax<'_>, scope: usize) {
        if n.kind() == "property_declaration"
            && self.scopes[&scope].local
            && swift_body_statement(n)
            && !self.uncertain(scope)
        {
            let Some(pattern) = n.child_by_field_name("name") else {
                return;
            };
            let Some(name) = pattern.child_by_field_name("bound_identifier") else {
                return;
            };
            let mut cursor = n.walk();
            if name.kind() != "simple_identifier"
                || pattern.child_count() != 1
                || n.children_by_field_name("name", &mut cursor).count() != 1
                || children(n).iter().any(|c| {
                    matches!(
                        c.kind(),
                        "modifiers"
                            | "computed_property"
                            | "willset_didset_block"
                            | "type_constraints"
                    )
                })
            {
                return;
            }
            let Some(value) = n.child_by_field_name("value") else {
                return;
            };
            let value = self.swift_function_value(scope, value);
            let mutable = children(n).iter().any(|c| {
                c.kind() == "value_binding_pattern"
                    && c.child_by_field_name("mutability")
                        .is_some_and(|m| self.e.text(m) == "var")
            });
            let span = (n.start_byte(), n.end_byte());
            self.swift_callables
                .locals
                .entry((scope, self.e.text(name).to_owned()))
                .and_modify(|local| local.blocked = true)
                .or_insert(SwiftCallableLocal {
                    declaration: span,
                    assignment: span,
                    value,
                    mutable,
                    blocked: false,
                });
        } else if n.kind() == "assignment" {
            let Some(name) = swift_assignment_name(n) else {
                return;
            };
            let name = self.e.text(name).to_owned();
            let Some(binding_scope) = self.swift_binding_scope(scope, &name) else {
                return;
            };
            let value = n
                .child_by_field_name("result")
                .and_then(|value| self.swift_function_value(scope, value));
            if let Some(local) = self.swift_callables.locals.get_mut(&(binding_scope, name)) {
                if scope != binding_scope
                    || !swift_body_statement(n)
                    || !local.mutable
                    || !n
                        .child_by_field_name("operator")
                        .is_some_and(|op| self.e.text(op) == "=")
                {
                    local.blocked = true;
                } else {
                    local.value = value;
                    local.assignment = (n.start_byte(), n.end_byte());
                }
            }
        }
    }
    pub(super) fn swift_callable_escapes(&mut self) {
        for (mut scope, name) in std::mem::take(&mut self.swift_callables.unbound_writes) {
            loop {
                if let Some(local) = self.swift_callables.locals.get_mut(&(scope, name.clone())) {
                    local.blocked = true;
                    break;
                }
                if let Some(Binding::Symbol(key)) = self.scopes[&scope].bindings.get(&name)
                    && self.swift_callables.functions.contains_key(key)
                {
                    self.swift_callables.escaped.insert((scope, name));
                    break;
                }
                let Some(parent) = self.e.scopes[scope].parent else {
                    break;
                };
                scope = parent;
            }
        }
        for (n, scope) in std::mem::take(&mut self.swift_callables.uses) {
            let name = self.e.text(n).to_owned();
            let Some(binding_scope) = self.swift_binding_scope(scope, &name) else {
                continue;
            };
            let binding = (binding_scope, name);
            let escaped = std::iter::successors(n.parent(), |p| p.parent())
                .take_while(|p| p.kind() != "statements")
                .any(|p| {
                    (p.kind() == "prefix_expression"
                        && p.child_by_field_name("operation")
                            .is_some_and(|op| self.e.text(op) == "&"))
                        || (p.kind() == "assignment"
                            && swift_assignment_name(p).is_none()
                            && p.child_by_field_name("target").is_some_and(|target| {
                                target.start_byte() <= n.start_byte()
                                    && n.end_byte() <= target.end_byte()
                            }))
                });
            if escaped {
                self.swift_callables.escaped.insert(binding.clone());
            }
            if let Some(local) = self.swift_callables.locals.get_mut(&binding)
                && (escaped || self.e.scopes[scope].owner != self.e.scopes[binding_scope].owner)
            {
                local.blocked = true;
            }
        }
    }
    pub(super) fn swift_callable_target<'a>(
        &self,
        call: &'a SwiftCallableCall,
    ) -> Option<&'a SwiftFunctionValue> {
        if self.swift_callables.locals[&call.binding].blocked
            || self
                .swift_callables
                .conditional_owners
                .contains(&self.e.scopes[call.binding.0].owner)
        {
            return None;
        }
        let value = call.value.as_ref()?;
        // A snapshot records a declaration identity, never just a spelling. A
        // later overload, shadowing declaration, or unknown write invalidates it.
        (matches!(self.scopes[&value.scope].bindings.get(&value.name),
            Some(Binding::Symbol(key)) if key == &value.key)
            && !self
                .swift_callables
                .escaped
                .contains(&(value.scope, value.name.clone()))
            && self.swift_callables.functions.get(&value.key) == Some(&Some(value.node))
            && self.e.facts.nodes[value.node].binding_key.as_ref() == Some(&value.key))
        .then_some(value)
    }
    pub(super) fn child(&mut self, scope: usize, n: Syntax<'_>) -> usize {
        let child = self.e.block(scope, n);
        let mut ctx = self.scopes[&scope].clone();
        ctx.bindings.clear();
        self.scopes.insert(child, ctx);
        child
    }
    pub(super) fn qualify(&self, scope: usize, name: &str) -> Option<String> {
        let (head, tail) = name.split_once('.').unwrap_or((name, ""));
        match self.lookup(scope, head) {
            Some(Binding::Type(q)) => {
                return Some(if tail.is_empty() {
                    q.clone()
                } else {
                    format!("{q}.{tail}")
                });
            }
            Some(Binding::Module(module)) if !tail.is_empty() => {
                return Some(format!("!{module}.{tail}"));
            }
            Some(Binding::Unknown | Binding::TypeParameter | Binding::Value(_)) => return None,
            _ => {}
        }
        if name.contains('.') {
            Some(name.into())
        } else if matches!(self.e.language, "c" | "cpp")
            && self.includes.len() == 1
            && self.scopes[&scope].namespace.starts_with('@')
        {
            Some(format!("@{}.{name}", self.includes[0]))
        } else {
            Some(format!("{}.{name}", self.scopes[&scope].namespace))
        }
    }
    pub(super) fn define(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        name: &str,
        kind: &str,
        qualified: String,
        public: bool,
    ) -> usize {
        let key = self.key(&qualified);
        let cross_project_public = self.cross_project_public(n, scope, kind, &qualified);
        let swift_exported = self.e.language == "swift"
            && !self.scopes[&scope].local
            && (self.modifier(n, "public") || self.modifier(n, "open"))
            && self
                .e
                .facts
                .nodes
                .iter()
                .find(|node| node.id == self.e.scopes[scope].owner)
                .is_some_and(|node| {
                    node.kind == "module"
                        || (node.kind != "extension" && node.metadata["swift_exported"] == true)
                });
        let child = self
            .e
            .define(n, scope, name, kind, public.then_some(key), false);
        self.e.facts.nodes.last_mut().unwrap().metadata["qualified_symbol"] = json!(qualified);
        self.e.facts.nodes.last_mut().unwrap().qualified_name = Some(qualified.clone());
        self.e.facts.nodes.last_mut().unwrap().metadata["cross_project_public"] =
            json!(cross_project_public);
        self.e.facts.nodes.last_mut().unwrap().metadata["explicit_public"] =
            json!(self.modifier(n, "public") || self.modifier(n, "open"));
        self.e.facts.nodes.last_mut().unwrap().metadata["declaration_certain"] =
            json!(!self.uncertain(scope));
        self.e.facts.nodes.last_mut().unwrap().metadata["partial"] =
            json!(self.modifier(n, "partial"));
        self.e.facts.nodes.last_mut().unwrap().metadata["member_accessible"] = json!(
            !self.modifier(n, "private")
                && !self.modifier(n, "fileprivate")
                && !self.modifier(n, "protected")
                && (self.e.language != "java"
                    || self.modifier(n, "public")
                    || self.scopes[&scope].abstract_members)
                && (!matches!(self.e.language, "cpp" | "csharp")
                    || cross_project_public
                    || self.modifier(n, "public")
                    || self.scopes[&scope].abstract_members
                    || (self.e.language == "cpp" && self.cpp_member_public(n)))
        );
        if self.e.language == "swift" {
            self.e.facts.nodes.last_mut().unwrap().metadata["explicit_access"] = json!(
                [
                    "public",
                    "open",
                    "internal",
                    "package",
                    "fileprivate",
                    "private"
                ]
                .into_iter()
                .find(|access| self.modifier(n, access))
            );
            self.e.facts.nodes.last_mut().unwrap().metadata["swift_exported"] =
                json!(swift_exported);
        }
        let mut ctx = self.scopes[&scope].clone();
        ctx.prefix = qualified;
        ctx.bindings.clear();
        self.scopes.insert(child, ctx);
        child
    }
    // Access evidence is independent of callable binding: a public virtual method
    // is still dynamic, and a public member of a hidden type is not exported.
    pub(super) fn cross_project_public(
        &self,
        n: Syntax<'_>,
        scope: usize,
        kind: &str,
        qualified: &str,
    ) -> bool {
        if self.scopes[&scope].local
            || self.uncertain(scope)
            || !matches!(
                kind,
                "namespace"
                    | "class"
                    | "struct"
                    | "union"
                    | "enum"
                    | "interface"
                    | "type"
                    | "function"
                    | "method"
                    | "declaration"
            )
            || ["private", "protected", "internal", "file"]
                .iter()
                .any(|m| self.modifier(n, m))
        {
            return false;
        }
        let Some(owner) = self
            .e
            .facts
            .nodes
            .iter()
            .find(|node| node.id == self.e.scopes[scope].owner)
        else {
            return false;
        };
        if owner.kind != "module" && owner.metadata["cross_project_public"] != true {
            return false;
        }
        match self.e.language {
            "java" | "csharp" => {
                kind == "namespace" || self.modifier(n, "public") || owner.kind == "interface"
            }
            "kotlin" => true,
            "cpp" => {
                if qualified.starts_with('@')
                    || matches!(
                        self.e.facts.nodes[0].metadata["dialect"].as_str(),
                        Some("cpp_cli" | "metal")
                    )
                {
                    return false;
                }
                if kind == "namespace" {
                    return true;
                }
                if self.scopes[&scope].class.is_none() {
                    // Qualified out-of-line members need their class declaration
                    // to prove access; this file alone cannot grant it.
                    return !self.modifier(n, "static")
                        && !n
                            .child_by_field_name("declarator")
                            .and_then(declarator_name)
                            .is_some_and(|name| self.e.text(name).contains("::"));
                }
                self.cpp_member_public(n)
            }
            _ => false,
        }
    }
    pub(super) fn cpp_member_public(&self, mut declaration: Syntax<'_>) -> bool {
        while let Some(parent) = declaration.parent() {
            if parent.kind() == "friend_declaration" {
                return false;
            }
            if parent.kind() == "field_declaration_list" {
                let mut public = parent
                    .parent()
                    .is_some_and(|ty| matches!(ty.kind(), "struct_specifier" | "union_specifier"));
                for sibling in children(parent) {
                    if sibling.start_byte() >= declaration.start_byte() {
                        break;
                    }
                    if sibling.kind().starts_with("preproc_") {
                        return false;
                    }
                    if sibling.kind() == "access_specifier" {
                        public = self.e.text(sibling).trim().trim_end_matches(':') == "public";
                    }
                }
                return public;
            }
            declaration = parent;
        }
        false
    }
}
