//! Recognize a single literal exec call and its unchanged output, never execute JS.
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Expression, ObjectPropertyKind, Statement, VariableDeclarationKind,
};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Output {
    Text,
    Envelope,
}

pub(super) fn arguments(source: &str) -> Option<(Map<String, Value>, Output)> {
    if !super::js_syntax::has_bounded_parser_complexity(source) {
        return None;
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if parsed.panicked
        || !parsed.diagnostics.is_empty()
        || parsed.program.hashbang.is_some()
        || !parsed.program.directives.is_empty()
    {
        return None;
    }
    if !SemanticBuilder::new_compiler()
        .build(&parsed.program)
        .diagnostics
        .is_empty()
    {
        return None;
    }
    let (call, output) = match parsed.program.body.as_slice() {
        [Statement::ExpressionStatement(statement)] => {
            let (value, output) = output_value(text_argument(&statement.expression)?)?;
            (awaited_call(value)?, output)
        }
        [
            Statement::VariableDeclaration(declaration),
            Statement::ExpressionStatement(statement),
        ] => {
            if declaration.kind != VariableDeclarationKind::Const || declaration.declare {
                return None;
            }
            let [declarator] = declaration.declarations.as_slice() else {
                return None;
            };
            let BindingPattern::BindingIdentifier(binding) = &declarator.id else {
                return None;
            };
            if matches!(binding.name.as_str(), "tools" | "text") {
                return None;
            }
            let (value, output) = output_value(text_argument(&statement.expression)?)?;
            if !identifier(value, binding.name.as_str()) {
                return None;
            }
            (awaited_call(declarator.init.as_ref()?)?, output)
        }
        _ => return None,
    };
    if call.optional || call.arguments.len() != 1 {
        return None;
    }
    let Expression::StaticMemberExpression(member) = &call.callee else {
        return None;
    };
    if member.optional
        || !identifier(&member.object, "tools")
        || member.property.name != "exec_command"
    {
        return None;
    }
    let Expression::ObjectExpression(object) = call.arguments[0].as_expression()? else {
        return None;
    };
    let mut arguments = Map::new();
    for property in &object.properties {
        let ObjectPropertyKind::ObjectProperty(property) = property else {
            return None;
        };
        if property.computed || property.method || property.shorthand {
            return None;
        }
        let key = property.key.static_name()?.into_owned();
        if matches!(key.as_str(), "__proto__" | "constructor" | "prototype") {
            return None;
        }
        // These wrappers need only scalar options. Reject getters, spreads,
        // expressions, bindings and interpolated templates, including in extras.
        let value = match &property.value {
            Expression::StringLiteral(value) if !value.lone_surrogates => {
                Value::String(value.value.to_string())
            }
            Expression::TemplateLiteral(value)
                if value.expressions.is_empty()
                    && value.quasis.len() == 1
                    && !value.quasis[0].lone_surrogates =>
            {
                Value::String(value.quasis[0].value.cooked.as_ref()?.to_string())
            }
            Expression::NumericLiteral(value) => {
                Value::Number(serde_json::Number::from_f64(value.value)?)
            }
            Expression::BooleanLiteral(value) => Value::Bool(value.value),
            Expression::NullLiteral(_) => Value::Null,
            _ => return None,
        };
        if arguments.insert(key, value).is_some() {
            return None;
        }
    }
    Some((arguments, output))
}

fn identifier(expression: &Expression<'_>, name: &str) -> bool {
    matches!(expression, Expression::Identifier(value) if value.name == name)
}

fn text_argument<'a>(expression: &'a Expression<'a>) -> Option<&'a Expression<'a>> {
    let Expression::CallExpression(call) = expression else {
        return None;
    };
    if call.optional || !identifier(&call.callee, "text") || call.arguments.len() != 1 {
        return None;
    }
    call.arguments[0].as_expression()
}

fn output_value<'a>(expression: &'a Expression<'a>) -> Option<(&'a Expression<'a>, Output)> {
    if let Expression::StaticMemberExpression(member) = expression {
        if member.optional || member.property.name != "output" {
            return None;
        }
        Some((member.object.without_parentheses(), Output::Text))
    } else {
        Some((expression, Output::Envelope))
    }
}

fn awaited_call<'a>(
    expression: &'a Expression<'a>,
) -> Option<&'a oxc_ast::ast::CallExpression<'a>> {
    let Expression::AwaitExpression(expression) = expression else {
        return None;
    };
    let Expression::CallExpression(call) = &expression.argument else {
        return None;
    };
    Some(call)
}

impl Output {
    pub(super) fn result(self, result: &Value) -> Option<Value> {
        // One emitted text item after the harness preamble; multiple emissions
        // cannot be assigned to the one command, even if their text looks valid.
        let [header, body] = result.as_array()?.as_slice() else {
            return None;
        };
        let text = |item: &Value| -> Option<String> {
            let object = item.as_object()?;
            (object.len() == 2 && object.get("type")?.as_str()? == "input_text")
                .then(|| object.get("text")?.as_str().map(str::to_owned))?
        };
        let header = text(header)?;
        if !header.starts_with("Script completed\n") || !header.ends_with("Output:\n") {
            return None;
        }
        let body = text(body)?;
        match self {
            Self::Text => Some(Value::String(body)),
            Self::Envelope => {
                let decoded = ctx_history_capture_model::exact_json_value(&body)?;
                let object = decoded.as_object()?;
                if object.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "chunk_id"
                            | "exit_code"
                            | "original_token_count"
                            | "output"
                            | "session_id"
                            | "wall_time_seconds"
                    )
                }) || object.get("exit_code").and_then(Value::as_i64) != Some(0)
                    || object
                        .get("session_id")
                        .is_some_and(|value| !value.is_null())
                {
                    return None;
                }
                Some(Value::String(object.get("output")?.as_str()?.to_owned()))
            }
        }
    }
}
