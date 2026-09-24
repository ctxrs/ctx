use super::*;

impl<'a> Elixir<'a> {
    pub(super) fn new(e: Extractor<'a>) -> Self {
        Self {
            e,
            functions: HashMap::new(),
            pending: vec![],
        }
    }
    pub(super) fn extract(mut self, root: Syntax<'_>) -> FileFacts {
        self.sequence(root, 0, &ElixirContext::default());
        for (index, module, name, arity, ctx) in &self.pending {
            let keys = if let Some((key, _)) =
                self.functions.get(&(module.clone(), name.clone(), *arity))
            {
                vec![key.clone()]
            } else {
                let matching: Vec<_> = ctx
                    .imports
                    .iter()
                    .filter(|(_, only, except)| {
                        only.as_ref()
                            .is_none_or(|o| o.contains(&(name.clone(), *arity)))
                            && !except.contains(&(name.clone(), *arity))
                    })
                    .collect();
                if matching.len() == 1 {
                    vec![format!("elixir:function:{}:{name}/{arity}", matching[0].0)]
                } else {
                    vec![]
                }
            };
            self.e.facts.references[*index].candidate_keys = keys;
        }
        self.e.finish()
    }
    pub(super) fn module(&self, name: &str, ctx: &ElixirContext) -> String {
        if let Some(name) = name.strip_prefix("Elixir.") {
            return name.into();
        }
        if name == "__MODULE__" {
            return ctx.module.clone();
        }
        if let Some(rest) = name.strip_prefix("__MODULE__.") {
            return qualified(&ctx.module, rest, ".");
        }
        let (first, rest) = name.split_once('.').unwrap_or((name, ""));
        ctx.aliases
            .get(first)
            .map(|a| {
                if rest.is_empty() {
                    a.clone()
                } else {
                    format!("{a}.{rest}")
                }
            })
            .unwrap_or_else(|| name.into())
    }
    pub(super) fn module_args(&self, n: Syntax<'_>, ctx: &ElixirContext) -> Vec<String> {
        let Some(first) = children(n).first().copied() else {
            return vec![];
        };
        if first.kind() == "alias" {
            return vec![self.module(self.e.text(first), ctx)];
        }
        if first.kind() == "dot"
            && let (Some(left), Some(right)) = (field(first, "left"), field(first, "right"))
            && left.kind() == "alias"
            && right.kind() == "tuple"
        {
            let base = self.module(self.e.text(left), ctx);
            return children(right)
                .into_iter()
                .filter(|c| c.kind() == "alias")
                .map(|c| format!("{base}.{}", self.e.text(c)))
                .collect();
        }
        vec![]
    }
    pub(super) fn option<'b>(&self, n: Syntax<'b>, key: &str) -> Option<Syntax<'b>> {
        descendants(n, "pair")
            .into_iter()
            .find(|p| {
                field(*p, "key")
                    .is_some_and(|k| self.e.text(k).trim_end().trim_end_matches(':') == key)
            })
            .and_then(|p| field(p, "value"))
    }
    pub(super) fn selectors(&self, n: Syntax<'_>) -> HashSet<(String, usize)> {
        descendants(n, "pair")
            .into_iter()
            .filter_map(|p| {
                let name = self
                    .e
                    .text(field(p, "key")?)
                    .trim_end()
                    .trim_end_matches(':')
                    .to_owned();
                let arity = self.e.text(field(p, "value")?).parse().ok()?;
                Some((name, arity))
            })
            .collect()
    }
    pub(super) fn sequence(&mut self, n: Syntax<'_>, scope: usize, ctx: &ElixirContext) {
        let mut ctx = ctx.clone();
        for c in children(n) {
            if c.kind() == "call"
                && field(c, "target").is_some_and(|t| {
                    matches!(self.e.text(t), "alias" | "import" | "require" | "use")
                })
            {
                self.import(c, scope, &mut ctx);
            } else {
                self.visit(c, scope, &ctx);
            }
        }
    }
    pub(super) fn import(&mut self, n: Syntax<'_>, scope: usize, ctx: &mut ElixirContext) {
        let keyword = field(n, "target").map(|n| self.e.text(n)).unwrap_or("");
        let Some(args) = child(n, "arguments") else {
            return;
        };
        let modules = self.module_args(args, ctx);
        if modules.is_empty() {
            self.e.reference(
                n,
                scope,
                self.e.text(n).into(),
                "imports",
                vec![],
                "dynamic module expression",
            );
        }
        for module in modules {
            self.e.reference(
                n,
                scope,
                module.clone(),
                "imports",
                vec![format!("elixir:module-name:{module}")],
                "module is external or ambiguous",
            );
            if keyword == "alias" {
                let alias = self
                    .option(args, "as")
                    .filter(|n| n.kind() == "alias")
                    .map(|n| self.e.text(n))
                    .unwrap_or_else(|| module.rsplit('.').next().unwrap_or(&module))
                    .to_owned();
                ctx.aliases.insert(alias, module);
            } else if keyword == "import" {
                let only = self.option(args, "only").map(|o| self.selectors(o));
                let except = self
                    .option(args, "except")
                    .map(|o| self.selectors(o))
                    .unwrap_or_default();
                ctx.imports.push((module, only, except));
            }
        }
    }
    pub(super) fn head<'b>(&self, mut n: Syntax<'b>) -> Syntax<'b> {
        while n.kind() == "binary_operator"
            && field(n, "operator").is_some_and(|o| self.e.text(o) == "when")
        {
            let Some(left) = field(n, "left") else { break };
            n = left;
        }
        n
    }
    pub(super) fn function(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        ctx: &ElixirContext,
        keyword: &str,
    ) {
        let Some(args) = child(n, "arguments") else {
            return;
        };
        let Some(head) = args.named_child(0).map(|n| self.head(n)) else {
            return;
        };
        let target = field(head, "target").unwrap_or(head);
        if target.kind() != "identifier" {
            return;
        }
        let name = self.e.text(target);
        let params = child(head, "arguments").map(children).unwrap_or_default();
        let arity = params.len();
        let defaults = params
            .iter()
            .filter(|p| {
                p.kind() == "binary_operator"
                    && field(**p, "operator").is_some_and(|o| self.e.text(o) == "\\\\")
            })
            .count();
        let public = !matches!(keyword, "defp" | "defmacrop" | "defguardp");
        let key = if public {
            format!("elixir:function:{}:{name}/{arity}", ctx.module)
        } else {
            format!(
                "elixir:private:{}:{}:{name}/{arity}",
                self.e.facts.path, ctx.module
            )
        };
        let slot = (ctx.module.clone(), name.into(), arity);
        let nested = if let Some((_, existing)) = self.functions.get(&slot) {
            let existing = *existing;
            let owner = self.e.scopes[existing].owner.clone();
            if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| n.id == owner) {
                node.end_line = Some(end_line(n));
                node.metadata["end_byte"] = n.end_byte().into();
            }
            self.e.scope(
                scope,
                self.e.scopes[existing].qualified.clone(),
                Some(owner),
                false,
            )
        } else {
            let nested = self.e.define(
                n,
                scope,
                name,
                if keyword.contains("macro") {
                    "macro"
                } else {
                    "function"
                },
                Some(key.clone()),
                false,
            );
            self.functions.insert(slot, (key.clone(), nested));
            if defaults > 0 {
                let aliases: Vec<_> = (arity - defaults..arity)
                    .map(|a| {
                        let alias = if public {
                            format!("elixir:function:{}:{name}/{a}", ctx.module)
                        } else {
                            format!(
                                "elixir:private:{}:{}:{name}/{a}",
                                self.e.facts.path, ctx.module
                            )
                        };
                        self.functions.insert(
                            (ctx.module.clone(), name.into(), a),
                            (alias.clone(), nested),
                        );
                        alias
                    })
                    .collect();
                self.e.facts.nodes.last_mut().unwrap().metadata["binding_aliases"] =
                    serde_json::json!(aliases);
            }
            nested
        };
        for p in &params {
            unknown_parameters(&mut self.e, *p, nested, &["identifier"]);
        }
        if let Some(body) = child(n, "do_block") {
            self.sequence(body, nested, ctx);
        }
        if let Some(body) = self.option(args, "do") {
            self.visit(body, nested, ctx);
        }
        // Guard calls belong to the function, never the enclosing module.
        if let Some(original_head) = args
            .named_child(0)
            .filter(|h| h.kind() == "binary_operator")
            && let Some(guard) = field(original_head, "right")
        {
            self.visit(guard, nested, ctx);
        }
    }
    pub(super) fn visit(&mut self, n: Syntax<'_>, scope: usize, ctx: &ElixirContext) {
        if n.kind() == "call" {
            let Some(target) = field(n, "target") else {
                return;
            };
            let keyword = self.e.text(target);
            if matches!(keyword, "defmodule" | "defprotocol" | "defimpl") {
                let Some(args) = child(n, "arguments") else {
                    return;
                };
                let Some(name) = args.named_child(0).filter(|n| n.kind() == "alias") else {
                    return;
                };
                let declared = self.e.text(name);
                let full = if keyword == "defimpl" {
                    let implementation = self
                        .option(args, "for")
                        .filter(|n| matches!(n.kind(), "alias" | "atom"))
                        .map(|n| self.module(self.e.text(n).trim_start_matches(':'), ctx));
                    implementation
                        .map(|target| format!("{}.{target}", self.module(declared, ctx)))
                        .unwrap_or_else(|| {
                            format!("{}.<implementation@{}>", self.e.facts.path, n.start_byte())
                        })
                } else if declared.starts_with("Elixir.") {
                    declared.trim_start_matches("Elixir.").into()
                } else {
                    qualified(&ctx.module, declared, ".")
                };
                let nested = self.e.define(
                    n,
                    scope,
                    declared,
                    if keyword == "defprotocol" {
                        "interface"
                    } else if keyword == "defimpl" {
                        "impl"
                    } else {
                        "module"
                    },
                    Some(format!("elixir:module-name:{full}")),
                    false,
                );
                if keyword == "defimpl" {
                    self.e.reference(
                        name,
                        nested,
                        declared.into(),
                        "implements",
                        vec![format!("elixir:module-name:{}", self.module(declared, ctx))],
                        "protocol is external or ambiguous",
                    );
                }
                let mut inner = ctx.clone();
                inner.module = full;
                if let Some(body) = child(n, "do_block") {
                    self.sequence(body, nested, &inner);
                }
                return;
            }
            if matches!(
                keyword,
                "def" | "defp" | "defmacro" | "defmacrop" | "defguard" | "defguardp"
            ) {
                self.function(n, scope, ctx, keyword);
                return;
            }
            if matches!(keyword, "alias" | "import" | "require" | "use") {
                self.import(n, scope, &mut ctx.clone());
                return;
            }
            let args = child(n, "arguments");
            let mut arity = args.map(|a| a.named_child_count()).unwrap_or(0);
            if n.parent().is_some_and(|p| {
                p.kind() == "binary_operator"
                    && field(p, "right").is_some_and(|r| r.id() == n.id())
                    && field(p, "operator").is_some_and(|o| self.e.text(o) == "|>")
            }) {
                arity += 1;
            }
            if !matches!(
                keyword,
                "if" | "unless"
                    | "case"
                    | "cond"
                    | "with"
                    | "for"
                    | "quote"
                    | "unquote"
                    | "defstruct"
                    | "raise"
                    | "try"
                    | "receive"
            ) {
                let keys = if target.kind() == "dot" {
                    match (field(target, "left"), field(target, "right")) {
                        (Some(left), Some(right))
                            if left.kind() == "alias" && right.kind() == "identifier" =>
                        {
                            vec![format!(
                                "elixir:function:{}:{}/{arity}",
                                self.module(self.e.text(left), ctx),
                                self.e.text(right)
                            )]
                        }
                        _ => vec![],
                    }
                } else {
                    vec![]
                };
                let index = self.e.facts.references.len();
                self.e.reference(
                    n,
                    scope,
                    keyword.into(),
                    "calls",
                    keys,
                    "dynamic callable, unavailable module, or ambiguous import",
                );
                if target.kind() == "identifier" {
                    self.pending.push((
                        index,
                        ctx.module.clone(),
                        keyword.into(),
                        arity,
                        ctx.clone(),
                    ));
                }
            }
            if let Some(args) = args {
                for arg in children(args) {
                    self.visit(arg, scope, ctx);
                }
            }
            if let Some(body) = child(n, "do_block") {
                let nested = self.e.block(scope, body);
                self.sequence(body, nested, ctx);
            }
            return;
        }
        if n.kind() == "binary_operator"
            && field(n, "operator").is_some_and(|o| self.e.text(o) == "=")
        {
            if let Some(right) = field(n, "right") {
                self.visit(right, scope, ctx);
            }
            if let Some(left) = field(n, "left") {
                unknown_parameters(&mut self.e, left, scope, &["identifier"]);
            }
            return;
        }
        if n.kind() == "identifier" {
            let name = self.e.text(n);
            let mut current = Some(scope);
            while let Some(i) = current {
                if self.e.scopes[i].bindings.contains_key(name) {
                    return;
                }
                current = self.e.scopes[i].parent;
            }
            if name != "__MODULE__" {
                let index = self.e.facts.references.len();
                self.e.reference(
                    n,
                    scope,
                    name.into(),
                    "calls",
                    vec![],
                    "unbound zero-arity function",
                );
                self.pending
                    .push((index, ctx.module.clone(), name.into(), 0, ctx.clone()));
            }
            return;
        }
        if n.kind() == "anonymous_function" {
            let label = format!("<fn@{}>", n.start_byte());
            let nested = self.e.define(n, scope, &label, "function", None, false);
            for clause in children(n) {
                self.visit(clause, nested, ctx);
            }
            return;
        }
        if n.kind() == "stab_clause" {
            let nested = self.e.block(scope, n);
            if let Some(params) = field(n, "left") {
                unknown_parameters(&mut self.e, params, nested, &["identifier"]);
            }
            if let Some(body) = field(n, "right") {
                self.sequence(body, nested, ctx);
            }
            return;
        }
        for c in children(n) {
            self.visit(c, scope, ctx);
        }
    }
}
