use super::*;

impl<'a> Php<'a> {
    pub(super) fn new(e: Extractor<'a>) -> Self {
        Self {
            e,
            semantic_sources: vec![],
            config_uses: vec![],
        }
    }
    pub(super) fn extract(mut self, root: Syntax<'_>) -> FileFacts {
        self.sequence(root, 0, &PhpContext::default());
        for (index, source_key, fallback_relation) in &self.semantic_sources {
            let sources: Vec<_> = self
                .e
                .facts
                .nodes
                .iter()
                .filter(|n| n.binding_key.as_ref() == Some(source_key))
                .collect();
            if sources.len() == 1 {
                self.e.facts.references[*index].source = sources[0].id.clone();
            } else {
                self.e.facts.references[*index].relation = (*fallback_relation).into();
            }
        }
        for (index, namespace) in &self.config_uses {
            let local = format!(
                "php:function:{}",
                qualified(namespace, "config", "\\").to_ascii_lowercase()
            );
            if self.e.facts.nodes.iter().any(|n| {
                n.binding_key
                    .as_deref()
                    .is_some_and(|k| k == local || k == "php:function:config")
            }) {
                self.e.facts.references[*index].candidate_keys.clear();
                self.e.facts.references[*index].reason =
                    "config helper is shadowed by a project function".into();
            }
        }
        self.e.finish()
    }
    pub(super) fn name(&self, n: Syntax<'_>, ctx: &PhpContext, function: bool) -> Option<String> {
        if !matches!(
            n.kind(),
            "name" | "qualified_name" | "namespace_name" | "relative_name" | "relative_scope"
        ) {
            return None;
        }
        let text = self.e.text(n);
        if text.eq_ignore_ascii_case("self") {
            return ctx.class.clone();
        }
        if matches!(text.to_lowercase().as_str(), "static" | "parent") {
            return None;
        }
        if text.starts_with('\\') {
            return Some(text.trim_start_matches('\\').to_lowercase());
        }
        if let Some(rest) = text.strip_prefix("namespace\\") {
            return Some(qualified(&ctx.namespace, rest, "\\").to_lowercase());
        }
        let (first, rest) = text.split_once('\\').unwrap_or((text, ""));
        let aliases = if function && rest.is_empty() {
            &ctx.functions
        } else {
            &ctx.aliases
        };
        if let Some(base) = aliases.get(&first.to_lowercase()) {
            return Some(
                qualified(base, rest, if rest.is_empty() { "" } else { "\\" }).to_lowercase(),
            );
        }
        Some(qualified(&ctx.namespace, text, "\\").to_lowercase())
    }
    pub(super) fn sequence(&mut self, n: Syntax<'_>, scope: usize, context: &PhpContext) {
        let mut ctx = context.clone();
        let mut owner = scope;
        for c in children(n) {
            if c.kind() == "namespace_definition" {
                ctx = PhpContext {
                    namespace: field(c, "name")
                        .map(|n| self.e.text(n).into())
                        .unwrap_or_default(),
                    ..Default::default()
                };
                let label = if ctx.namespace.is_empty() {
                    "<global>"
                } else {
                    &ctx.namespace
                };
                owner = self.e.define(
                    c,
                    scope,
                    label,
                    "namespace",
                    Some(format!("php:namespace:{}", ctx.namespace.to_lowercase())),
                    false,
                );
                if let Some(body) = field(c, "body") {
                    self.sequence(body, owner, &ctx);
                    ctx = context.clone();
                    owner = scope;
                }
            } else if c.kind() == "namespace_use_declaration" {
                self.use_import(c, owner, &mut ctx);
            } else {
                self.visit(c, owner, &ctx);
            }
        }
    }
    pub(super) fn use_import(&mut self, n: Syntax<'_>, scope: usize, ctx: &mut PhpContext) {
        let prefix = child(n, "namespace_name")
            .map(|p| self.e.text(p).trim_end_matches('\\'))
            .unwrap_or("");
        for clause in descendants(n, "namespace_use_clause") {
            let Some(target) = children(clause)
                .into_iter()
                .find(|c| Some(c.id()) != field(clause, "alias").map(|a| a.id()))
            else {
                continue;
            };
            let full = qualified(prefix, self.e.text(target).trim_start_matches('\\'), "\\")
                .to_lowercase();
            let alias = field(clause, "alias")
                .map(|a| self.e.text(a))
                .unwrap_or_else(|| self.e.text(target).rsplit('\\').next().unwrap_or(""))
                .to_lowercase();
            let kind = field(clause, "type")
                .or_else(|| field(n, "type"))
                .map(|n| self.e.text(n));
            let function = kind == Some("function");
            let key = format!(
                "php:{}:{full}",
                if function {
                    "function"
                } else if kind == Some("const") {
                    "constant"
                } else {
                    "type"
                }
            );
            self.e.reference(
                clause,
                scope,
                full.clone(),
                "imports",
                vec![key],
                "import target is unavailable or ambiguous",
            );
            if function {
                ctx.functions.insert(alias, full);
            } else if kind != Some("const") {
                ctx.aliases.insert(alias, full);
            }
        }
    }
    pub(super) fn type_refs(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        ctx: &PhpContext,
        context: &str,
    ) {
        let mut types = descendants(n, "named_type");
        types.extend(descendants(n, "primitive_type"));
        for ty in types {
            if let Some(name) = if ty.kind() == "primitive_type" {
                Some(ty)
            } else {
                ty.named_child(0)
            } {
                let keys = self
                    .name(name, ctx, false)
                    .map(|s| vec![format!("php:type:{s}")])
                    .unwrap_or_default();
                self.e.reference(
                    name,
                    scope,
                    self.e.text(name).into(),
                    "references",
                    keys.clone(),
                    context,
                );
                let owner = self.e.scopes[scope].owner.clone();
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| n.id == owner) {
                    if !node.metadata["type_contexts"].is_array() {
                        node.metadata["type_contexts"] = serde_json::json!([]);
                    }
                    node.metadata["type_contexts"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({"context":context,"keys":keys,"line":line(name)}));
                }
            }
        }
    }
    pub(super) fn class_literal(&self, n: Syntax<'_>, ctx: &PhpContext) -> Option<String> {
        let n = if n.kind() == "argument" {
            n.named_child(0)?
        } else {
            n
        };
        if n.kind() != "class_constant_access_expression" {
            return None;
        }
        let parts = children(n);
        if !parts
            .last()
            .is_some_and(|c| self.e.text(*c).eq_ignore_ascii_case("class"))
        {
            return None;
        }
        self.name(*parts.first()?, ctx, false)
    }
    pub(super) fn registration(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        source: &str,
        target: &str,
        relation: &'static str,
    ) {
        let source_key = format!("php:type:{source}");
        self.e.reference(
            n,
            scope,
            source.into(),
            if relation == "bound_to" {
                "binds"
            } else {
                "listens_for"
            },
            vec![source_key.clone()],
            "registered contract/event is external or ambiguous",
        );
        let index = self.e.facts.references.len();
        self.e.reference(
            n,
            scope,
            target.into(),
            relation,
            vec![format!("php:type:{target}")],
            "registered implementation/listener is external or ambiguous",
        );
        self.semantic_sources.push((
            index,
            source_key,
            if relation == "bound_to" {
                "registers_binding"
            } else {
                "registers_listener"
            },
        ));
    }
    pub(super) fn visit(&mut self, n: Syntax<'_>, scope: usize, ctx: &PhpContext) {
        match n.kind() {
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
            | "anonymous_class" => {
                let label = field(n, "name")
                    .map(|n| self.e.text(n).to_owned())
                    .unwrap_or_else(|| format!("<class@{}>", n.start_byte()));
                let full = qualified(&ctx.namespace, &label, "\\").to_lowercase();
                let kind = match n.kind() {
                    "interface_declaration" => "interface",
                    "trait_declaration" => "trait",
                    "enum_declaration" => "enum",
                    _ => "class",
                };
                let nested = self.e.define(
                    n,
                    scope,
                    &label,
                    kind,
                    Some(format!("php:type:{full}")),
                    false,
                );
                let mut inner = ctx.clone();
                inner.class = Some(full);
                for base in children(n)
                    .into_iter()
                    .filter(|c| matches!(c.kind(), "base_clause" | "class_interface_clause"))
                {
                    for target in children(base) {
                        let keys = self
                            .name(target, ctx, false)
                            .map(|s| vec![format!("php:type:{s}")])
                            .unwrap_or_default();
                        self.e.reference(
                            target,
                            nested,
                            self.e.text(target).into(),
                            if base.kind() == "base_clause" {
                                "inherits"
                            } else {
                                "implements"
                            },
                            keys,
                            "base type is unavailable or dynamic",
                        );
                    }
                }
                if let Some(body) = field(n, "body") {
                    self.sequence(body, nested, &inner);
                }
                return;
            }
            "function_definition"
            | "method_declaration"
            | "anonymous_function"
            | "arrow_function" => {
                let label = field(n, "name")
                    .map(|n| self.e.text(n).to_owned())
                    .unwrap_or_else(|| format!("<function@{}>", n.start_byte()));
                let method = n.kind() == "method_declaration";
                let key = if method {
                    ctx.class
                        .as_ref()
                        .map(|c| format!("php:method:{c}::{}", label.to_lowercase()))
                } else if n.kind() == "function_definition" {
                    Some(format!(
                        "php:function:{}",
                        qualified(&ctx.namespace, &label, "\\").to_lowercase()
                    ))
                } else {
                    None
                };
                let nested = self.e.define(
                    n,
                    scope,
                    &label,
                    if method { "method" } else { "function" },
                    key,
                    false,
                );
                if let Some(params) = field(n, "parameters") {
                    unknown_parameters(&mut self.e, params, nested, &["variable_name"]);
                    self.type_refs(params, nested, ctx, "parameter_type");
                    for parameter in children(params) {
                        if let Some(value) = field(parameter, "default_value") {
                            self.visit(value, nested, ctx);
                        }
                    }
                }
                if let Some(ty) = field(n, "return_type") {
                    self.type_refs(ty, nested, ctx, "return_type");
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested, ctx);
                }
                return;
            }
            "property_declaration" => {
                for property in children(n)
                    .into_iter()
                    .filter(|p| p.kind() == "property_element")
                {
                    let Some(name) = field(property, "name") else {
                        continue;
                    };
                    let label = self.e.text(name);
                    let key = ctx
                        .class
                        .as_ref()
                        .map(|c| format!("php:property:{c}::{label}"));
                    let nested = self
                        .e
                        .define(property, scope, label, "property", key, false);
                    if let Some(ty) = field(n, "type") {
                        self.type_refs(ty, nested, ctx, "field");
                    }
                    if let Some(value) = field(property, "default_value") {
                        if matches!(label, "$listen" | "$subscribe")
                            && value.kind() == "array_creation_expression"
                        {
                            for entry in children(value)
                                .into_iter()
                                .filter(|e| e.kind() == "array_element_initializer")
                            {
                                let parts = children(entry);
                                if let (Some(event), Some(listeners)) = (
                                    parts.first().and_then(|n| self.class_literal(*n, ctx)),
                                    parts
                                        .get(1)
                                        .filter(|n| n.kind() == "array_creation_expression"),
                                ) {
                                    for listener in children(*listeners)
                                        .into_iter()
                                        .filter_map(|n| n.named_child(0))
                                    {
                                        if let Some(target) = self.class_literal(listener, ctx) {
                                            self.registration(
                                                listener,
                                                nested,
                                                &event,
                                                &target,
                                                "listened_by",
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        self.visit(value, nested, ctx);
                    }
                }
                return;
            }
            "scoped_property_access_expression" | "class_constant_access_expression" => {
                let target = field(n, "scope").or_else(|| n.named_child(0));
                let keys = target
                    .and_then(|t| self.name(t, ctx, false))
                    .map(|s| vec![format!("php:type:{s}")])
                    .unwrap_or_default();
                self.e.reference(
                    n,
                    scope,
                    self.e.text(n).into(),
                    if n.kind() == "scoped_property_access_expression" {
                        "uses_static_prop"
                    } else {
                        "references_constant"
                    },
                    keys,
                    "static owner is dynamic, external, or ambiguous",
                );
            }
            "use_declaration" => {
                for target in children(n).into_iter().filter(|c| c.kind() != "use_list") {
                    let keys = self
                        .name(target, ctx, false)
                        .map(|s| vec![format!("php:type:{s}")])
                        .unwrap_or_default();
                    self.e.reference(
                        target,
                        scope,
                        self.e.text(target).into(),
                        "mixes_in",
                        keys,
                        "trait is dynamic or unavailable",
                    );
                }
            }
            "function_call_expression" => {
                if let Some(target) = field(n, "function") {
                    if self
                        .e
                        .text(target)
                        .trim_start_matches('\\')
                        .eq_ignore_ascii_case("config")
                        && !ctx.functions.contains_key("config")
                        && let Some(value) = field(n, "arguments")
                            .and_then(|a| a.named_child(0))
                            .and_then(|a| literal(&self.e, a))
                    {
                        let section = value.split('.').next().unwrap_or("").to_owned();
                        if !section.is_empty()
                            && section.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            let index = self.e.facts.references.len();
                            self.e.reference(
                                n,
                                scope,
                                value,
                                "uses_config",
                                vec![
                                    format!("php:module:config/{section}"),
                                    format!(
                                        "php:type:{}",
                                        qualified(&ctx.namespace, &section, "\\")
                                            .to_ascii_lowercase()
                                    ),
                                ],
                                "static config convention target is external or ambiguous",
                            );
                            self.config_uses.push((index, ctx.namespace.clone()));
                        }
                    }
                    let keys = self
                        .name(target, ctx, true)
                        .map(|s| vec![format!("php:function:{s}")])
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        self.e.text(target).into(),
                        "calls",
                        keys,
                        "dynamic callable or unavailable namespaced function",
                    );
                }
            }
            "scoped_call_expression" => {
                if let (Some(class), Some(name)) = (field(n, "scope"), field(n, "name")) {
                    let keys = (name.kind() == "name")
                        .then(|| self.name(class, ctx, false))
                        .flatten()
                        .map(|c| {
                            vec![format!(
                                "php:method:{c}::{}",
                                self.e.text(name).to_lowercase()
                            )]
                        })
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        format!("{}::{}", self.e.text(class), self.e.text(name)),
                        "calls",
                        keys,
                        "dynamic dispatch or unavailable static method",
                    );
                }
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                if let (Some(object), Some(name)) = (field(n, "object"), field(n, "name")) {
                    let container = object.kind() == "member_access_expression"
                        && field(object, "object").is_some_and(|o| self.e.text(o) == "$this")
                        && field(object, "name").is_some_and(|n| self.e.text(n) == "app");
                    if container
                        && matches!(
                            self.e.text(name),
                            "bind" | "singleton" | "scoped" | "instance"
                        )
                    {
                        let args = field(n, "arguments").map(children).unwrap_or_default();
                        if let (Some(contract), Some(implementation)) = (
                            args.first().and_then(|a| self.class_literal(*a, ctx)),
                            args.get(1).and_then(|a| self.class_literal(*a, ctx)),
                        ) {
                            self.registration(n, scope, &contract, &implementation, "bound_to");
                        }
                    }
                    let keys = if self.e.text(object) == "$this" && name.kind() == "name" {
                        ctx.class
                            .as_ref()
                            .map(|c| {
                                vec![format!(
                                    "php:method:{c}::{}",
                                    self.e.text(name).to_lowercase()
                                )]
                            })
                            .unwrap_or_default()
                    } else {
                        vec![]
                    };
                    self.e.reference(
                        n,
                        scope,
                        format!("{}->{}", self.e.text(object), self.e.text(name)),
                        "calls",
                        keys,
                        "runtime receiver type or dynamic method",
                    );
                }
            }
            "object_creation_expression" => {
                if let Some(target) = children(n).into_iter().find(|c| c.kind() != "arguments") {
                    let keys = self
                        .name(target, ctx, false)
                        .map(|s| vec![format!("php:type:{s}")])
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        self.e.text(target).into(),
                        "instantiates",
                        keys,
                        "runtime class or unavailable type",
                    );
                }
            }
            "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => {
                let target = n.named_child(0);
                let value = target.and_then(|t| literal(&self.e, t));
                let keys = value
                    .as_deref()
                    .map(|s| {
                        path_modules(&self.e, s, true)
                            .into_iter()
                            .map(|m| format!("php:module:{m}"))
                            .collect()
                    })
                    .unwrap_or_default();
                self.e.reference(
                    n,
                    scope,
                    value.unwrap_or_else(|| self.e.text(n).into()),
                    "imports",
                    keys,
                    "dynamic include or unavailable path",
                );
            }
            _ => {}
        }
        for c in children(n) {
            self.visit(c, scope, ctx);
        }
    }
}
