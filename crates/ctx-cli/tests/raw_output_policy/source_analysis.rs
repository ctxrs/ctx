use super::*;

#[derive(Debug, Clone, Default)]
pub(super) struct SourceAliases {
    output_helpers: BTreeSet<String>,
    output_modules: BTreeSet<String>,
    stdout_constructors: BTreeSet<String>,
    stderr_constructors: BTreeSet<String>,
    io_modules: BTreeSet<String>,
    write_traits: BTreeSet<String>,
}

impl SourceAliases {
    pub(super) fn analyze(tokens: &[Token]) -> Self {
        let mut aliases = Self::default();
        for index in 0..tokens.len() {
            if tokens[index].text != "use" {
                continue;
            }
            let Some(end) = statement_end(tokens, index + 1, tokens.len()) else {
                continue;
            };
            let mut imports = Vec::new();
            collect_use_imports(&tokens[index + 1..end], &[], &mut imports);
            for (path, alias) in imports {
                let suffix = path.iter().map(String::as_str).collect::<Vec<_>>();
                if path_ends_with(&suffix, &["std", "io", "stdout"]) {
                    aliases.stdout_constructors.insert(alias);
                } else if path_ends_with(&suffix, &["std", "io", "stderr"]) {
                    aliases.stderr_constructors.insert(alias);
                } else if path_ends_with(&suffix, &["std", "io"]) {
                    aliases.io_modules.insert(alias);
                } else if path_ends_with(&suffix, &["std", "io", "Write"]) {
                    aliases.write_traits.insert(alias);
                } else if path.last().is_some_and(|name| {
                    matches!(
                        name.as_str(),
                        "stdout_writer"
                            | "stderr_writer"
                            | "write_stdout"
                            | "write_stdout_line"
                            | "write_stderr_line"
                    )
                }) && path[..path.len().saturating_sub(1)]
                    .iter()
                    .any(|segment| segment == "output")
                {
                    aliases.output_helpers.insert(alias);
                } else if path.last().is_some_and(|segment| segment == "output") {
                    aliases.output_modules.insert(alias);
                }
            }
        }
        aliases
    }

    pub(super) fn io_constructor(&self, tokens: &[Token], index: usize) -> Option<Primitive> {
        let text = tokens.get(index)?.text.as_str();
        if self.stdout_constructors.contains(text)
            || (text == "stdout" && self.has_io_qualifier(tokens, index))
        {
            return Some(Primitive::StdoutConstructor);
        }
        if self.stderr_constructors.contains(text)
            || (text == "stderr" && self.has_io_qualifier(tokens, index))
        {
            return Some(Primitive::StderrConstructor);
        }
        None
    }

    fn has_io_qualifier(&self, tokens: &[Token], index: usize) -> bool {
        is_io_path(tokens, index)
            || (index >= 3
                && tokens[index - 2].text == ":"
                && tokens[index - 1].text == ":"
                && self.io_modules.contains(&tokens[index - 3].text))
    }

    pub(super) fn is_output_helper(&self, tokens: &[Token], index: usize) -> bool {
        if self.output_helpers.contains(&tokens[index].text) {
            return true;
        }
        path_contains(tokens, index, "output")
            || (index >= 3
                && tokens[index - 2].text == ":"
                && tokens[index - 1].text == ":"
                && self.output_modules.contains(&tokens[index - 3].text))
    }

    pub(super) fn is_write_trait_call(&self, tokens: &[Token], index: usize) -> bool {
        index >= 3
            && tokens[index - 2].text == ":"
            && tokens[index - 1].text == ":"
            && (tokens[index - 3].text == "Write"
                || self.write_traits.contains(&tokens[index - 3].text))
    }

    pub(super) fn is_imported_output_helper(&self, name: &str) -> bool {
        self.output_helpers.contains(name)
    }
}

fn path_ends_with(path: &[&str], suffix: &[&str]) -> bool {
    path.len() >= suffix.len() && &path[path.len() - suffix.len()..] == suffix
}

fn collect_use_imports(
    tree: &[Token],
    prefix: &[String],
    imports: &mut Vec<(Vec<String>, String)>,
) {
    if tree.is_empty() {
        return;
    }
    let brace = tree.iter().position(|token| token.text == "{");
    if let Some(open) = brace {
        let Some(close) = matching_delimiter(tree, open, "{", "}") else {
            return;
        };
        let mut nested_prefix = prefix.to_vec();
        nested_prefix.extend(
            tree[..open]
                .iter()
                .filter(|token| is_path_ident(&token.text))
                .map(|token| token.text.clone()),
        );
        for (start, end) in split_top_level(&tree[open + 1..close], ",") {
            collect_use_imports(
                &tree[open + 1 + start..open + 1 + end],
                &nested_prefix,
                imports,
            );
        }
        return;
    }

    let alias_marker = tree.iter().position(|token| token.text == "as");
    let path_end = alias_marker.unwrap_or(tree.len());
    let mut tail = tree[..path_end]
        .iter()
        .filter(|token| is_path_ident(&token.text))
        .map(|token| token.text.clone())
        .collect::<Vec<_>>();
    if tail.first().is_some_and(|segment| segment == "self") {
        tail.remove(0);
    }
    let mut path = prefix.to_vec();
    path.extend(tail);
    if path.is_empty() {
        return;
    }
    let alias = alias_marker
        .and_then(|marker| tree.get(marker + 1))
        .filter(|token| is_path_ident(&token.text))
        .map(|token| token.text.clone())
        .or_else(|| path.last().cloned());
    if let Some(alias) = alias {
        imports.push((path, alias));
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct DocumentCatalog {
    free_functions: BTreeSet<String>,
    methods: BTreeSet<(String, String)>,
    fields: BTreeMap<(String, String), String>,
}

impl DocumentCatalog {
    pub(super) fn from_tokens(tokens: &[Token], functions: &[FunctionSpan]) -> Self {
        let mut catalog = Self::default();
        catalog.absorb(tokens, functions);
        catalog
    }

    pub(super) fn absorb(&mut self, tokens: &[Token], functions: &[FunctionSpan]) {
        for function in functions {
            if !function.returns_document {
                continue;
            }
            if let Some(impl_type) = &function.impl_type {
                self.methods
                    .insert((impl_type.clone(), function.name.clone()));
            } else {
                self.free_functions.insert(function.name.clone());
            }
        }
        for index in 0..tokens.len().saturating_sub(1) {
            if tokens[index].text != "struct" || !is_path_ident(&tokens[index + 1].text) {
                continue;
            }
            let type_name = tokens[index + 1].text.clone();
            let Some(open) = (index + 2..tokens.len())
                .find(|cursor| matches!(tokens[*cursor].text.as_str(), "{" | ";"))
            else {
                continue;
            };
            if tokens[open].text != "{" {
                continue;
            }
            let Some(close) = matching_delimiter(tokens, open, "{", "}") else {
                continue;
            };
            for (start, end) in split_top_level(&tokens[open + 1..close], ",") {
                let field = &tokens[open + 1 + start..open + 1 + end];
                let Some(colon) = field.iter().position(|token| token.text == ":") else {
                    continue;
                };
                let Some(field_name) = field[..colon]
                    .iter()
                    .find(|token| is_path_ident(&token.text) && token.text != "pub")
                    .map(|token| token.text.clone())
                else {
                    continue;
                };
                let Some(field_type) = field[colon + 1..]
                    .iter()
                    .find(|token| {
                        is_path_ident(&token.text)
                            && token
                                .text
                                .bytes()
                                .next()
                                .is_some_and(|byte| byte.is_ascii_uppercase())
                    })
                    .map(|token| token.text.clone())
                else {
                    continue;
                };
                self.fields
                    .insert((type_name.clone(), field_name), field_type);
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct DocumentOrigins {
    bindings: Vec<BTreeSet<String>>,
    binding_types: Vec<BTreeMap<String, String>>,
}

impl DocumentOrigins {
    pub(super) fn analyze(
        tokens: &[Token],
        functions: &[FunctionSpan],
        catalog: &DocumentCatalog,
    ) -> Self {
        let mut origins = Self {
            bindings: functions
                .iter()
                .map(|function| function.document_parameters.clone())
                .collect(),
            binding_types: functions
                .iter()
                .map(|function| function.parameter_types.clone())
                .collect(),
        };
        loop {
            let mut changed = false;
            for function_index in 0..functions.len() {
                changed |=
                    origins.discover_local_bindings(tokens, functions, function_index, catalog);
            }
            if !changed {
                return origins;
            }
        }
    }

    fn discover_local_bindings(
        &mut self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
        catalog: &DocumentCatalog,
    ) -> bool {
        let function = &functions[function_index];
        let mut changed = false;
        for index in function.open + 1..function.close {
            if tokens[index].text != "let" {
                continue;
            }
            let Some(equal) = (index + 1..function.close)
                .find(|cursor| matches!(tokens[*cursor].text.as_str(), "=" | ";"))
            else {
                break;
            };
            if tokens[equal].text != "=" {
                continue;
            }
            let Some(end) = statement_end(tokens, equal + 1, function.close) else {
                continue;
            };
            let Some(name) = tokens[index + 1..equal]
                .iter()
                .find(|token| is_path_ident(&token.text) && token.text != "mut")
                .map(|token| token.text.clone())
            else {
                continue;
            };
            if let Some(colon) = (index + 1..equal).find(|cursor| tokens[*cursor].text == ":") {
                if let Some(type_name) = tokens[colon + 1..equal]
                    .iter()
                    .find(|token| {
                        is_path_ident(&token.text)
                            && token
                                .text
                                .bytes()
                                .next()
                                .is_some_and(|byte| byte.is_ascii_uppercase())
                    })
                    .map(|token| token.text.clone())
                {
                    if !self.binding_types[function_index].contains_key(&name) {
                        self.binding_types[function_index].insert(name.clone(), type_name);
                        changed = true;
                    }
                }
            }
            if self.expression_returns_document(
                tokens,
                functions,
                function_index,
                equal + 1,
                end,
                catalog,
            ) {
                changed |= self.bindings[function_index].insert(name);
            }
        }
        changed
    }

    fn expression_returns_document(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
        start: usize,
        end: usize,
        catalog: &DocumentCatalog,
    ) -> bool {
        let function = &functions[function_index];
        for index in start..end.min(tokens.len()) {
            if tokens[index].text == "Document"
                && tokens.get(index + 1).map(|token| token.text.as_str()) == Some(":")
            {
                return true;
            }
            if !is_path_ident(&tokens[index].text)
                || tokens.get(index + 1).map(|token| token.text.as_str()) != Some("(")
            {
                continue;
            }
            if previous_is(tokens, index, ".") {
                if let Some(receiver_type) =
                    self.receiver_type(tokens, functions, function_index, index, catalog)
                {
                    if catalog
                        .methods
                        .contains(&(receiver_type, tokens[index].text.clone()))
                    {
                        return true;
                    }
                }
            } else if function
                .document_factory_parameters
                .contains(&tokens[index].text)
                || catalog.free_functions.contains(&tokens[index].text)
            {
                return true;
            }
        }
        false
    }

    fn receiver_type(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
        method_index: usize,
        catalog: &DocumentCatalog,
    ) -> Option<String> {
        let receiver_start = expression_start(tokens, method_index.saturating_sub(1));
        let receiver = &tokens[receiver_start..method_index.saturating_sub(1)];
        if receiver.len() == 1 {
            return self.binding_types[function_index]
                .get(&receiver[0].text)
                .cloned();
        }
        if receiver.len() == 3 && receiver[0].text == "self" && receiver[1].text == "." {
            let impl_type = functions[function_index].impl_type.as_ref()?;
            return catalog
                .fields
                .get(&(impl_type.clone(), receiver[2].text.clone()))
                .cloned();
        }
        None
    }

    pub(super) fn render_receiver_is_document(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        index: usize,
        catalog: &DocumentCatalog,
    ) -> bool {
        let Some(function_index) = innermost_function_index(functions, index) else {
            return false;
        };
        let receiver_start = expression_start(tokens, index.saturating_sub(1));
        let receiver_end = index.saturating_sub(1);
        let receiver = &tokens[receiver_start..receiver_end];
        if receiver.len() == 1 && self.bindings[function_index].contains(&receiver[0].text) {
            return true;
        }
        self.expression_returns_document(
            tokens,
            functions,
            function_index,
            receiver_start,
            receiver_end,
            catalog,
        )
    }
}
