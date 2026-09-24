use super::*;

impl<'a> Ruby<'a> {
    pub(super) fn visit(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        namespace: &str,
        owner: Option<&str>,
    ) {
        match n.kind() {
            "class" | "module" => {
                let Some(name) = field(n, "name") else { return };
                let Some(full) = self.constant(name, namespace) else {
                    return;
                };
                let nested = self.e.define(
                    n,
                    scope,
                    self.e.text(name),
                    n.kind(),
                    Some(format!("ruby:type:{full}")),
                    false,
                );
                let base = field(n, "superclass").and_then(|n| n.named_child(0));
                let bases = base
                    .map(|base| self.constants(base, namespace))
                    .unwrap_or_default();
                let metadata = &mut self.e.facts.nodes.last_mut().unwrap().metadata;
                metadata["ruby_bases"] = serde_json::json!(bases);
                metadata["ruby_dynamic_base"] = (base.is_some() && bases.is_empty()).into();
                if !Self::direct_declaration(n) {
                    self.instance_barriers.insert(full.clone());
                    self.singleton_barriers.insert(full.clone());
                }
                self.owners.insert(nested, full.clone());
                self.singleton.insert(nested);
                self.e.scopes[nested].parent = None;
                self.e.scopes[nested].class = false;
                self.e.scopes[nested].fallback = Some(format!("ruby:singleton:{full}."));
                let mut lexical = vec![full.clone()];
                lexical.extend(self.lexical.get(namespace).cloned().unwrap_or_default());
                self.lexical.insert(full.clone(), lexical);
                if let Some(base) = base {
                    let keys = bases
                        .into_iter()
                        .map(|s| format!("ruby:type:{s}"))
                        .collect();
                    self.e.reference(
                        base,
                        nested,
                        self.e.text(base).into(),
                        "inherits",
                        keys,
                        "base constant is unavailable or dynamic",
                    );
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested, &full, Some(&full));
                }
                return;
            }
            "method" | "singleton_method" => {
                self.method(n, scope, namespace, owner);
                return;
            }
            "singleton_class" => {
                self.unsafe_lookup = true;
            }
            "lambda" | "block" | "do_block" => {
                let label = format!("<block@{}>", n.start_byte());
                let nested = self.e.define(n, scope, &label, "function", None, false);
                if let Some(p) = field(n, "parameters") {
                    unknown_parameters(&mut self.e, p, nested, &["identifier"]);
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested, namespace, owner);
                }
                return;
            }
            "assignment" | "operator_assignment" => {
                if let Some(left) = field(n, "left") {
                    if matches!(
                        left.kind(),
                        "left_assignment_list" | "destructured_left_assignment"
                    ) && !descendants(left, "constant").is_empty()
                    {
                        self.inheritance_unsafe = true;
                    }
                    if matches!(left.kind(), "constant" | "scope_resolution") {
                        let factory =
                            field(n, "right")
                                .filter(|r| r.kind() == "call")
                                .filter(|r| {
                                    let method = field(*r, "method").map(|m| self.e.text(m));
                                    let recv = field(*r, "receiver").map(|m| self.e.text(m));
                                    matches!(
                                        (recv, method),
                                        (Some("Class" | "Struct"), Some("new"))
                                            | (Some("Data"), Some("define"))
                                    )
                                });
                        if let Some(factory) = factory
                            && let Some(full) = self.constant(left, namespace)
                        {
                            let nested = self.e.define(
                                n,
                                scope,
                                self.e.text(left),
                                "class",
                                Some(format!("ruby:type:{full}")),
                                false,
                            );
                            if field(factory, "receiver").is_some_and(|r| self.e.text(r) == "Class")
                                && let Some(base) =
                                    field(factory, "arguments").and_then(|a| a.named_child(0))
                            {
                                let keys = self
                                    .constant(base, namespace)
                                    .map(|b| vec![format!("ruby:type:{b}")])
                                    .unwrap_or_default();
                                self.e.reference(
                                    base,
                                    nested,
                                    self.e.text(base).into(),
                                    "inherits",
                                    keys,
                                    "dynamic superclass",
                                );
                            }
                            if let Some(body) =
                                field(factory, "block").and_then(|b| field(b, "body"))
                            {
                                self.visit(body, nested, &full, Some(&full));
                            }
                            return;
                        }
                        self.unsafe_lookup = true;
                    } else if left.kind() == "identifier" {
                        let binding = field(n, "right")
                            .filter(|r| r.kind() == "call")
                            .filter(|r| {
                                field(*r, "method").is_some_and(|m| self.e.text(m) == "new")
                            })
                            .and_then(|r| field(r, "receiver"))
                            .map(|r| self.constants(r, namespace))
                            .filter(|names| !names.is_empty())
                            .map(|names| Binding::Namespace {
                                prefixes: names
                                    .into_iter()
                                    .map(|s| format!("ruby:instance:{s}#"))
                                    .collect(),
                                separator: ".",
                            })
                            .unwrap_or(Binding::Unknown);
                        self.e.bind(scope, self.e.text(left), binding);
                    }
                }
            }
            "call" => {
                let Some(method) = field(n, "method") else {
                    for c in children(n) {
                        self.visit(c, scope, namespace, owner);
                    }
                    return;
                };
                let setter = n.parent().is_some_and(|p| {
                    p.kind() == "assignment" && field(p, "left").is_some_and(|l| l.id() == n.id())
                });
                let method_name = if setter {
                    format!("{}=", self.e.text(method))
                } else {
                    self.e.text(method).into()
                };
                let name = method_name.as_str();
                let receiver = field(n, "receiver");
                let args = field(n, "arguments").map(children).unwrap_or_default();
                if matches!(
                    name,
                    "include"
                        | "extend"
                        | "prepend"
                        | "private"
                        | "protected"
                        | "public"
                        | "private_class_method"
                        | "public_class_method"
                ) && receiver.is_some_and(|r| r.kind() != "self")
                {
                    self.inheritance_unsafe = true;
                }
                if matches!(
                    name,
                    "private"
                        | "protected"
                        | "public"
                        | "private_class_method"
                        | "public_class_method"
                ) && (receiver.is_none() || receiver.is_some_and(|r| r.kind() == "self"))
                {
                    self.set_visibility(n, scope, owner, name, &args);
                } else if name == "super" && receiver.is_none() {
                    self.e.reference(
                        n,
                        scope,
                        "super".into(),
                        "calls",
                        vec![],
                        "superclass method is unavailable or dynamic",
                    );
                } else if matches!(name, "attr_reader" | "attr_writer" | "attr_accessor") {
                    if receiver.is_none() || receiver.is_some_and(|r| r.kind() == "self") {
                        self.attributes(n, scope, owner, name, &args);
                    } else {
                        self.inheritance_unsafe = true;
                    }
                    self.e.call(n, scope, method, Some(vec![name.into()]));
                } else if receiver.is_none()
                    && matches!(name, "require" | "require_relative" | "load")
                {
                    let value = args.first().and_then(|a| literal(&self.e, *a));
                    let modules = value
                        .as_deref()
                        .map(|v| path_modules(&self.e, v, name == "require_relative"))
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        value.unwrap_or_else(|| self.e.text(n).into()),
                        "imports",
                        modules
                            .into_iter()
                            .map(|m| format!("ruby:module:{m}"))
                            .collect(),
                        "dynamic require or unavailable load path",
                    );
                } else if receiver.is_none()
                    && name == "module_function"
                    && !self.e.scopes[scope].function
                {
                    if args.is_empty() {
                        self.module_functions.insert(scope);
                    } else if let Some(owner) = owner {
                        for arg in &args {
                            if arg.kind() == "simple_symbol" {
                                self.exported_methods.insert(format!(
                                    "ruby:instance:{owner}#{}",
                                    self.e.text(*arg).trim_start_matches(':')
                                ));
                            }
                        }
                    }
                } else if (receiver.is_none() || receiver.is_some_and(|r| r.kind() == "self"))
                    && matches!(name, "include" | "extend" | "prepend")
                {
                    if let Some(owner) = owner {
                        if name == "extend" {
                            if !args.iter().all(|a| a.kind() == "self") {
                                self.singleton_barriers.insert(owner.into());
                            }
                        } else {
                            self.instance_barriers.insert(owner.into());
                        }
                    }
                    for arg in &args {
                        if name == "extend" && arg.kind() == "self" {
                            if let Some(owner) = owner {
                                self.extended_self.insert(owner.into());
                            }
                            continue;
                        }
                        let keys = self
                            .constants(*arg, namespace)
                            .into_iter()
                            .map(|s| format!("ruby:type:{s}"))
                            .collect();
                        self.e.reference(
                            *arg,
                            scope,
                            self.e.text(*arg).into(),
                            "mixes_in",
                            keys,
                            "mixin is dynamic or unavailable",
                        );
                    }
                } else {
                    if matches!(
                        name,
                        "eval"
                            | "class_eval"
                            | "module_eval"
                            | "class_exec"
                            | "module_exec"
                            | "refine"
                            | "using"
                            | "define_method"
                            | "define_singleton_method"
                            | "remove_method"
                            | "undef_method"
                            | "const_set"
                            | "autoload"
                            | "alias_method"
                    ) {
                        self.unsafe_lookup = true;
                    }
                    if let Some(receiver) = receiver {
                        let label = format!("{}.{}", self.e.text(receiver), name);
                        let classes = self.constants(receiver, namespace);
                        if !classes.is_empty() {
                            let relation = if name == "new" {
                                "instantiates"
                            } else {
                                "calls"
                            };
                            let keys = classes
                                .into_iter()
                                .map(|class| {
                                    if name == "new" {
                                        format!("ruby:type:{class}")
                                    } else {
                                        format!("ruby:singleton:{class}.{name}")
                                    }
                                })
                                .collect();
                            self.e.reference(
                                n,
                                scope,
                                label,
                                relation,
                                keys,
                                "constant receiver target is unavailable or ambiguous",
                            );
                        } else if receiver.kind() == "self" && owner.is_some() {
                            let singleton = self.is_singleton(scope);
                            let o = owner.unwrap();
                            let key = if singleton {
                                format!("ruby:singleton:{o}.{name}")
                            } else {
                                format!("ruby:instance:{o}#{name}")
                            };
                            self.e.reference(
                                n,
                                scope,
                                label,
                                "calls",
                                vec![key],
                                "receiver method is unavailable or ambiguous",
                            );
                            self.self_calls
                                .insert(self.e.facts.references.last().unwrap().id.clone());
                        } else {
                            let parts = (receiver.kind() == "identifier")
                                .then(|| vec![self.e.text(receiver).into(), name.into()]);
                            self.call_labels.insert(
                                format!(
                                    "call:{}:{}-{}",
                                    self.e.scopes[scope].owner,
                                    n.start_byte(),
                                    n.end_byte()
                                ),
                                label,
                            );
                            self.e.call(n, scope, method, parts);
                        }
                    } else {
                        self.self_call(n, scope);
                        self.e.call(n, scope, method, Some(vec![name.into()]));
                    }
                }
                for arg in args {
                    self.visit(arg, scope, namespace, owner);
                }
                if let Some(recv) = receiver.filter(|r| r.kind() == "call") {
                    self.visit(recv, scope, namespace, owner);
                }
                if let Some(block) = field(n, "block") {
                    self.visit(block, scope, namespace, owner);
                }
                return;
            }
            "identifier" => {
                let name = self.e.text(n);
                if matches!(name, "private" | "protected" | "public") && owner.is_some() {
                    self.set_visibility(n, scope, owner, name, &[]);
                    return;
                }
                if name == "module_function" && owner.is_some() && !self.e.scopes[scope].function {
                    self.module_functions.insert(scope);
                    return;
                }
                if n.parent().is_some_and(|p| {
                    field(p, "left").is_some_and(|l| l.id() == n.id())
                        || field(p, "name").is_some_and(|l| l.id() == n.id())
                }) {
                    return;
                }
                let mut current = Some(scope);
                while let Some(i) = current {
                    if matches!(
                        self.e.scopes[i].bindings.get(name),
                        Some(Binding::Unknown | Binding::Namespace { .. })
                    ) {
                        return;
                    }
                    current = self.e.scopes[i].parent;
                }
                self.self_call(n, scope);
                self.e.call(n, scope, n, Some(vec![name.into()]));
                return;
            }
            "super" | "yield" => {
                self.e.reference(
                    n,
                    scope,
                    self.e.text(n).into(),
                    "calls",
                    vec![],
                    "inherited dispatch or runtime block",
                );
            }
            "alias" | "undef" => {
                self.unsafe_lookup = true;
            }
            _ => {}
        }
        for c in children(n) {
            self.visit(c, scope, namespace, owner);
        }
    }
}
