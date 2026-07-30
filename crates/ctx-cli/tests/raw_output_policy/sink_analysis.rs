use super::{
    expression_start, innermost_function_index, matching_delimiter, split_top_level, statement_end,
    FunctionSpan, Token,
};

pub(super) fn render_receiver_is_glyph(
    tokens: &[Token],
    functions: &[FunctionSpan],
    index: usize,
) -> bool {
    let receiver_start = expression_start(tokens, index.saturating_sub(1));
    let receiver = &tokens[receiver_start..index.saturating_sub(1)];
    if receiver.iter().any(|token| token.text == "Glyph") {
        return true;
    }
    let Some(name) = (receiver.len() == 1).then(|| receiver[0].text.as_str()) else {
        return false;
    };
    let Some(function_index) = innermost_function_index(functions, index) else {
        return false;
    };
    if functions[function_index].glyph_parameters.contains(name) {
        return true;
    }
    let function = &functions[function_index];
    for cursor in function.open + 1..index {
        if tokens[cursor].text != "let" {
            continue;
        }
        let Some(equal) = (cursor + 1..index)
            .find(|candidate| matches!(tokens[*candidate].text.as_str(), "=" | ";"))
        else {
            break;
        };
        if tokens[equal].text != "="
            || !tokens[cursor + 1..equal]
                .iter()
                .any(|token| token.text == name)
        {
            continue;
        }
        let Some(end) = statement_end(tokens, equal + 1, index) else {
            continue;
        };
        if tokens[equal + 1..end]
            .iter()
            .any(|token| token.text == "Glyph")
        {
            return true;
        }
    }
    false
}

pub(super) fn write_method_has_document_argument(
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
    let argument = &tokens[index + 2 + start..index + 2 + end];
    argument.len() == 1
        && functions[function_index]
            .document_parameters
            .contains(&argument[0].text)
}
