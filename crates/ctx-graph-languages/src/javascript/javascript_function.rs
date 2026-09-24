use super::*;

impl Javascript<'_> {
    pub(super) fn function(&mut self, node: Syntax<'_>, scope: usize, assigned: Option<&str>) {
        let name_node = node.child_by_field_name("name");
        let name = assigned
            .map(str::to_owned)
            .or_else(|| name_node.map(|n| self.e.text(n).into()))
            .unwrap_or_else(|| format!("<anonymous@{}>", node.start_byte()));
        let method = node.kind() == "method_definition";
        let declaration = matches!(
            node.kind(),
            "function_declaration" | "generator_function_declaration"
        );
        let bind = assigned.is_some() || declaration;
        let static_member = token(node, "static");
        let visibility = children(node)
            .into_iter()
            .find(|n| n.kind() == "accessibility_modifier")
            .map(|n| self.e.text(n))
            .unwrap_or(if name.starts_with('#') {
                "private"
            } else {
                "public"
            });
        let plain_method = method
            && name != "constructor"
            && !token(node, "get")
            && !token(node, "set")
            && name_node.is_some_and(|n| {
                matches!(
                    n.kind(),
                    "property_identifier" | "private_property_identifier"
                )
            })
            && !children(node).iter().any(|n| n.kind() == "decorator");
        let key = if plain_method
            && let Some(class) = self.classes.get(&scope).filter(|c| !c.dynamic_members)
        {
            let mode = if static_member { "static" } else { "instance" };
            Some(if visibility == "public" {
                format!("{}#{mode}.{}", class.key, identifier(&name))
            } else {
                self.e
                    .local_key(scope, &format!("#{mode}.{}", identifier(&name)))
            })
        } else if bind || (!method && name_node.is_some()) {
            Some(self.key(scope, &name))
        } else {
            None
        };
        if method {
            for n in children(node)
                .into_iter()
                .filter(|n| matches!(n.kind(), "computed_property_name" | "decorator"))
            {
                self.visit(n, scope);
            }
        }
        let factory = self.e.facts.nodes.len();
        let child = self.e.define(
            node,
            scope,
            &name,
            if method { "method" } else { "function" },
            key.clone(),
            bind,
        );
        let exact_member_key = key.as_deref().map(|k| self.exact_key(k));
        if method && let Some(class) = self.classes.get_mut(&scope) {
            class
                .methods
                .entry((static_member, identifier(&name)))
                .and_modify(|k| *k = None)
                .or_insert(exact_member_key);
            let item = self.e.facts.nodes.last_mut().unwrap();
            item.metadata["declaring_class"] = class.id.clone().into();
            item.metadata["static"] = static_member.into();
            item.metadata["visibility"] = visibility.into();
            self.receiver(
                child,
                "this",
                node.start_byte(),
                Receiver::This {
                    class: scope,
                    static_member,
                    property_written: false,
                },
            );
        } else if node.kind() != "arrow_function" {
            self.e.bind(child, "this", Binding::Unknown);
        }
        if !declaration
            && !method
            && let (Some(n), Some(key)) = (name_node, key)
        {
            self.e.bind(
                child,
                self.e.text(n),
                Binding::Symbol {
                    keys: vec![key],
                    id: Some(self.e.scopes[child].owner.clone()),
                },
            );
        }
        self.type_parameters(node, child);
        if let Some(result) = node.child_by_field_name("return_type") {
            self.type_refs(result, child, "return_type");
        }
        for field in ["parameters", "parameter"] {
            if let Some(parameters) = node.child_by_field_name(field) {
                self.pattern(parameters, child, false);
                // Defaults execute in the function's parameter environment.
                self.visit(parameters, child);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            let body_scope = self.e.scopes.len();
            self.visit(body, child);
            if let Some((value, target)) = self.single_factory_return(node)
                && let Some(returned) = self.e.facts.nodes.iter().position(|n| {
                    n.kind == "function" && n.metadata["start_byte"] == target.start_byte()
                })
            {
                let probe = if value.kind() == "identifier" {
                    self.e.call(
                        value,
                        body_scope,
                        value,
                        Some(vec![self.e.text(value).into()]),
                    );
                    Some(format!(
                        "call:{}:{}-{}",
                        self.e.scopes[body_scope].owner,
                        value.start_byte(),
                        value.end_byte()
                    ))
                } else {
                    None
                };
                self.factory_returns.push(FactoryReturn {
                    factory,
                    returned,
                    probe,
                });
            }
        }
    }
    pub(super) fn import(&mut self, node: Syntax<'_>, scope: usize) {
        let source = node
            .child_by_field_name("source")
            .and_then(|n| self.string(n));
        let modules = source
            .as_deref()
            .map(|s| self.modules(s))
            .unwrap_or_default();
        self.e.reference(
            node,
            scope,
            source.clone().unwrap_or_else(|| self.e.text(node).into()),
            "imports",
            modules.iter().map(|m| module_key(m)).collect(),
            "module is external, unavailable, or ambiguous",
        );
        let type_only = token(node, "type");
        for clause in children(node)
            .into_iter()
            .filter(|n| n.kind() == "import_clause")
        {
            for part in children(clause) {
                match part.kind() {
                    "identifier" => self.import_binding(
                        part,
                        scope,
                        self.e.text(part),
                        "default",
                        &modules,
                        type_only,
                    ),
                    "namespace_import" => {
                        if let Some(name) = part.named_child(0) {
                            self.bind_type(
                                scope,
                                self.e.text(name),
                                Binding::Namespace {
                                    prefixes: modules
                                        .iter()
                                        .map(|m| format!("javascript:{m}:"))
                                        .collect(),
                                    separator: ".",
                                },
                            );
                            self.e.bind(
                                scope,
                                self.e.text(name),
                                if type_only {
                                    Binding::Unknown
                                } else {
                                    Binding::Namespace {
                                        prefixes: modules
                                            .iter()
                                            .map(|m| format!("javascript:{m}:"))
                                            .collect(),
                                        separator: ".",
                                    }
                                },
                            );
                        }
                    }
                    "named_imports" => {
                        for spec in children(part) {
                            if let Some(name) = spec.child_by_field_name("name") {
                                let alias = spec.child_by_field_name("alias").unwrap_or(name);
                                self.import_binding(
                                    spec,
                                    scope,
                                    self.e.text(alias),
                                    self.e.text(name),
                                    &modules,
                                    type_only || token(spec, "type"),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    pub(super) fn import_binding(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        local: &str,
        name: &str,
        modules: &[String],
        type_only: bool,
    ) {
        let name = identifier(name);
        let keys: Vec<_> = modules
            .iter()
            .map(|m| format!("javascript:{m}:{name}"))
            .collect();
        self.bind_type(
            scope,
            local,
            Binding::Symbol {
                keys: keys.clone(),
                id: None,
            },
        );
        self.e.bind(
            scope,
            local,
            if type_only {
                Binding::Unknown
            } else {
                Binding::Symbol {
                    keys: keys.clone(),
                    id: None,
                }
            },
        );
        self.e.reference(
            node,
            scope,
            format!("{name} as {local}"),
            "imports",
            keys,
            "import is external, unavailable, or ambiguous",
        );
    }
}
