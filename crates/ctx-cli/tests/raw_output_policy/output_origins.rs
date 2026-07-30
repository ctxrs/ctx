use super::*;

#[derive(Debug, Default)]
pub(super) struct OutputOrigins {
    bindings: Vec<BTreeSet<String>>,
    fields: BTreeSet<(String, String)>,
    pub(super) aliases: SourceAliases,
}

impl OutputOrigins {
    pub(super) fn analyze(
        tokens: &[Token],
        functions: &[FunctionSpan],
        aliases: SourceAliases,
    ) -> Self {
        let mut origins = Self {
            bindings: vec![BTreeSet::new(); functions.len()],
            fields: BTreeSet::new(),
            aliases,
        };
        loop {
            let mut changed = false;
            for (function_index, function) in functions.iter().enumerate() {
                changed |= origins.discover_local_bindings(tokens, functions, function_index);
                changed |= origins.propagate_call_arguments(tokens, functions, function_index);
                changed |= origins.discover_fields(tokens, functions, function_index);
                debug_assert!(function.open < function.close);
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
    ) -> bool {
        let function = &functions[function_index];
        let mut changed = false;
        let mut index = function.open + 1;
        while index < function.close {
            if tokens[index].text != "let" {
                index += 1;
                continue;
            }
            let Some(equal) = (index + 1..function.close)
                .find(|cursor| matches!(tokens[*cursor].text.as_str(), "=" | ";"))
            else {
                break;
            };
            if tokens[equal].text != "=" {
                index = equal + 1;
                continue;
            }
            let Some(end) = statement_end(tokens, equal + 1, function.close) else {
                break;
            };
            let binding = tokens[index + 1..equal]
                .iter()
                .find(|token| is_path_ident(&token.text) && token.text != "mut")
                .map(|token| token.text.clone());
            if let Some(binding) = binding {
                if self.expression_has_origin(tokens, functions, function_index, equal + 1, end) {
                    changed |= self.bindings[function_index].insert(binding);
                }
            }
            index += 1;
        }
        changed
    }

    fn propagate_call_arguments(
        &mut self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        caller_index: usize,
    ) -> bool {
        let caller = &functions[caller_index];
        let mut changes = Vec::new();
        for index in caller.open + 1..caller.close {
            if !is_path_ident(&tokens[index].text)
                || tokens.get(index + 1).map(|token| token.text.as_str()) != Some("(")
                || previous_is(tokens, index, "fn")
                || previous_is(tokens, index, ".")
            {
                continue;
            }
            let Some(close) = matching_delimiter(tokens, index + 1, "(", ")") else {
                continue;
            };
            if close > caller.close {
                continue;
            }
            let qualifier = if index >= 3
                && tokens[index - 1].text == ":"
                && tokens[index - 2].text == ":"
                && is_path_ident(&tokens[index - 3].text)
            {
                Some(tokens[index - 3].text.as_str())
            } else {
                None
            };
            let arguments = &tokens[index + 2..close];
            for (callee_index, callee) in functions.iter().enumerate() {
                if callee.name != tokens[index].text
                    || !callee_matches_qualifier(callee, qualifier, caller.impl_type.as_deref())
                {
                    continue;
                }
                for ((start, end), parameter) in split_top_level(arguments, ",")
                    .into_iter()
                    .zip(&callee.parameters)
                {
                    if self.expression_has_origin(
                        tokens,
                        functions,
                        caller_index,
                        index + 2 + start,
                        index + 2 + end,
                    ) {
                        changes.push((callee_index, parameter.clone()));
                    }
                }
            }
        }
        let mut changed = false;
        for (function_index, binding) in changes {
            changed |= self.bindings[function_index].insert(binding);
        }
        changed
    }

    fn discover_fields(
        &mut self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
    ) -> bool {
        let function = &functions[function_index];
        let Some(impl_type) = function.impl_type.as_deref() else {
            return false;
        };
        let mut changes = Vec::new();
        for index in function.open + 1..function.close {
            if tokens[index].text != "Self"
                || tokens.get(index + 1).map(|token| token.text.as_str()) != Some("{")
            {
                continue;
            }
            let Some(close) = matching_delimiter(tokens, index + 1, "{", "}") else {
                continue;
            };
            for (start, end) in split_top_level(&tokens[index + 2..close], ",") {
                let entry_start = index + 2 + start;
                let entry_end = index + 2 + end;
                let Some(field) = tokens[entry_start..entry_end]
                    .iter()
                    .find(|token| is_path_ident(&token.text))
                    .map(|token| token.text.clone())
                else {
                    continue;
                };
                let colon = (entry_start..entry_end).find(|cursor| tokens[*cursor].text == ":");
                let value_start = colon.map_or(entry_start, |colon| colon + 1);
                if self.expression_has_origin(
                    tokens,
                    functions,
                    function_index,
                    value_start,
                    entry_end,
                ) {
                    changes.push(field);
                }
            }
        }
        let mut changed = false;
        for field in changes {
            changed |= self.fields.insert((impl_type.to_owned(), field));
        }
        changed
    }

    fn expression_has_origin(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
        start: usize,
        end: usize,
    ) -> bool {
        let function = &functions[function_index];
        for index in start..end.min(tokens.len()) {
            if self.aliases.io_constructor(tokens, index).is_some()
                && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
            {
                return true;
            }
            if matches!(
                tokens[index].text.as_str(),
                "stdout_writer" | "stderr_writer"
            ) && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
                && !previous_is(tokens, index, "fn")
            {
                return true;
            }
            if self.aliases.is_imported_output_helper(&tokens[index].text)
                && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
            {
                return true;
            }
            if self.bindings[function_index].contains(&tokens[index].text) {
                return true;
            }
            if tokens[index].text == "self"
                && tokens.get(index + 1).map(|token| token.text.as_str()) == Some(".")
            {
                if let (Some(impl_type), Some(field)) = (
                    function.impl_type.as_deref(),
                    tokens.get(index + 2).map(|token| token.text.as_str()),
                ) {
                    if self
                        .fields
                        .contains(&(impl_type.to_owned(), field.to_owned()))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub(super) fn write_macro_has_origin(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        index: usize,
    ) -> bool {
        if tokens.get(index + 1).map(|token| token.text.as_str()) != Some("!")
            || tokens.get(index + 2).map(|token| token.text.as_str()) != Some("(")
        {
            return false;
        }
        let Some(close) = matching_delimiter(tokens, index + 2, "(", ")") else {
            return false;
        };
        let Some((start, end)) = split_top_level(&tokens[index + 3..close], ",")
            .into_iter()
            .next()
        else {
            return false;
        };
        let Some(function_index) = innermost_function_index(functions, index) else {
            return false;
        };
        self.expression_has_origin(
            tokens,
            functions,
            function_index,
            index + 3 + start,
            index + 3 + end,
        )
    }

    pub(super) fn write_method_has_origin(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        index: usize,
    ) -> bool {
        let Some(function_index) = innermost_function_index(functions, index) else {
            return false;
        };
        let start = expression_start(tokens, index.saturating_sub(1));
        self.expression_has_origin(tokens, functions, function_index, start, index - 1)
    }

    pub(super) fn write_associated_has_origin(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        index: usize,
    ) -> bool {
        let Some(function_index) = innermost_function_index(functions, index) else {
            return false;
        };
        let Some(close) = matching_delimiter(tokens, index + 1, "(", ")") else {
            return false;
        };
        let Some((start, end)) = split_top_level(&tokens[index + 2..close], ",")
            .into_iter()
            .next()
        else {
            return false;
        };
        self.expression_has_origin(
            tokens,
            functions,
            function_index,
            index + 2 + start,
            index + 2 + end,
        )
    }
}
