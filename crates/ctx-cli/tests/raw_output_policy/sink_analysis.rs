use super::{innermost_function_index, matching_delimiter, split_top_level, FunctionSpan, Token};

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
