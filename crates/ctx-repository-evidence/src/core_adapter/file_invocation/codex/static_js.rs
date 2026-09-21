use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, BindingPattern, CallExpression, Expression, MemberExpression, ObjectPropertyKind,
    Program, Statement, VariableDeclarationKind,
};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_syntax::number::ToJsString;
use serde_json::{Map, Number, Value};

const MAX_STATIC_JS_BYTES: usize = 1024 * 1024;
const MAX_STATIC_AST_NODES: usize = 4_096;
const MAX_STATIC_RAW_COMPLEXITY_ITEMS: usize = 512;
const MAX_STATIC_WORK_UNITS: usize = MAX_STATIC_JS_BYTES * 4;
const MAX_STATIC_VALUE_WEIGHT: usize = MAX_STATIC_JS_BYTES;
const MAX_STATIC_NESTED_TOOL_CALLS: usize = 24;
const MAX_STATIC_BINDINGS: usize = 64;
const MAX_STATIC_LITERAL_DEPTH: usize = 32;
const MAX_STATIC_LITERAL_ITEMS: usize = 1_024;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum StaticNestedToolCall {
    ExecCommand { arguments: Map<String, Value> },
    ApplyPatch { patch: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StaticTool {
    ExecCommand,
    ApplyPatch,
}

#[derive(Debug, Clone)]
enum StaticValue {
    Null,
    Bool(bool),
    Number(f64),
    String(Arc<str>),
    Array(Arc<StaticArray>),
    Object(Arc<StaticObject>),
    OpaqueToolResult,
    ToolNamespace,
    ToolFunction(StaticTool),
}

#[derive(Debug)]
struct StaticArray {
    values: Vec<StaticValue>,
    expanded_weight: usize,
}

#[derive(Debug)]
struct StaticObject {
    entries: Vec<(Arc<str>, StaticValue)>,
    expanded_weight: usize,
}

impl StaticValue {
    fn expanded_weight(&self) -> usize {
        match self {
            Self::String(value) => value.len().max(1),
            Self::Array(value) => value.expanded_weight,
            Self::Object(value) => value.expanded_weight,
            Self::Null
            | Self::Bool(_)
            | Self::Number(_)
            | Self::OpaqueToolResult
            | Self::ToolNamespace
            | Self::ToolFunction(_) => 1,
        }
    }

    fn to_json(&self) -> Option<Value> {
        match self {
            Self::Null => Some(Value::Null),
            Self::Bool(value) => Some(Value::Bool(*value)),
            Self::Number(value) => Number::from_f64(*value).map(Value::Number),
            Self::String(value) => Some(Value::String(value.to_string())),
            Self::Array(values) => values
                .values
                .iter()
                .map(Self::to_json)
                .collect::<Option<Vec<_>>>()
                .map(Value::Array),
            Self::Object(values) => values
                .entries
                .iter()
                .try_fold(Map::new(), |mut object, (key, value)| {
                    object.insert(key.to_string(), value.to_json()?);
                    Some(object)
                })
                .map(Value::Object),
            Self::OpaqueToolResult | Self::ToolNamespace | Self::ToolFunction(_) => None,
        }
    }

    fn interpolation(&self) -> Option<StaticInterpolation> {
        match self {
            Self::Null => Some(StaticInterpolation::Known(Arc::from("null"))),
            Self::Bool(value) => Some(StaticInterpolation::Known(Arc::from(value.to_string()))),
            Self::Number(value) => {
                Some(StaticInterpolation::Known(Arc::from(value.to_js_string())))
            }
            Self::String(value) => Some(StaticInterpolation::Known(Arc::clone(value))),
            Self::OpaqueToolResult => Some(StaticInterpolation::Opaque),
            Self::Array(_) | Self::Object(_) | Self::ToolNamespace | Self::ToolFunction(_) => None,
        }
    }

    fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    fn is_output_safe(&self) -> bool {
        match self {
            Self::Array(values) => values.values.iter().all(Self::is_output_safe),
            Self::Object(values) => values
                .entries
                .iter()
                .all(|(_, value)| value.is_output_safe()),
            Self::ToolNamespace | Self::ToolFunction(_) => false,
            Self::Null
            | Self::Bool(_)
            | Self::Number(_)
            | Self::String(_)
            | Self::OpaqueToolResult => true,
        }
    }

    fn contains_opaque_tool_result(&self) -> bool {
        match self {
            Self::OpaqueToolResult => true,
            Self::Array(values) => values.values.iter().any(Self::contains_opaque_tool_result),
            Self::Object(values) => values
                .entries
                .iter()
                .any(|(_, value)| value.contains_opaque_tool_result()),
            Self::Null
            | Self::Bool(_)
            | Self::Number(_)
            | Self::String(_)
            | Self::ToolNamespace
            | Self::ToolFunction(_) => false,
        }
    }
}

enum StaticInterpolation {
    Known(Arc<str>),
    Opaque,
}

pub(super) struct StaticJsParser<'a> {
    source: &'a str,
}

impl<'a> StaticJsParser<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self { source }
    }

    pub(super) fn parse_program(self) -> Option<Vec<StaticNestedToolCall>> {
        if !has_bounded_parser_complexity(self.source) {
            return None;
        }
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, self.source, SourceType::mjs()).parse();
        if parsed.panicked || !parsed.diagnostics.is_empty() {
            return None;
        }
        let semantic = SemanticBuilder::new_compiler().build(&parsed.program);
        if !semantic.diagnostics.is_empty() {
            return None;
        }
        StaticEvaluator::new(self.source.len()).evaluate_program(&parsed.program)
    }
}

struct StaticEvaluator {
    bindings: HashMap<String, StaticValue>,
    calls: Vec<StaticNestedToolCall>,
    ast_nodes: usize,
    work_units: usize,
    literal_items: usize,
    saw_output: bool,
}

impl StaticEvaluator {
    fn new(source_bytes: usize) -> Self {
        Self {
            bindings: HashMap::new(),
            calls: Vec::new(),
            ast_nodes: 0,
            work_units: source_bytes,
            literal_items: 0,
            saw_output: false,
        }
    }

    fn evaluate_program(mut self, program: &Program<'_>) -> Option<Vec<StaticNestedToolCall>> {
        if program.hashbang.is_some() {
            return None;
        }
        for statement in &program.body {
            if let Statement::VariableDeclaration(declaration) = statement {
                for declarator in &declaration.declarations {
                    if let BindingPattern::BindingIdentifier(identifier) = &declarator.id
                        && is_static_global(identifier.name.as_str())
                    {
                        return None;
                    }
                }
            }
        }
        for directive in &program.directives {
            self.touch_node()?;
            self.charge_work(directive.directive.len())?;
            if directive.directive != "use strict" {
                return None;
            }
        }
        for statement in &program.body {
            let is_output = self.evaluate_statement(statement)?;
            if self.saw_output && !is_output {
                return None;
            }
            self.saw_output |= is_output;
        }
        Some(self.calls)
    }

    fn evaluate_statement(&mut self, statement: &Statement<'_>) -> Option<bool> {
        self.touch_node()?;
        match statement {
            Statement::VariableDeclaration(declaration) => {
                if declaration.kind != VariableDeclarationKind::Const || declaration.declare {
                    return None;
                }
                for declarator in &declaration.declarations {
                    self.touch_node()?;
                    let BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
                        return None;
                    };
                    let initializer = declarator.init.as_ref()?;
                    let value = self.evaluate_expression(initializer, 0)?;
                    let name = identifier.name.as_str();
                    self.charge_work(name.len())?;
                    if self.bindings.len() >= MAX_STATIC_BINDINGS
                        || self.bindings.insert(name.to_owned(), value).is_some()
                    {
                        return None;
                    }
                }
                Some(false)
            }
            Statement::ExpressionStatement(statement) => {
                if self.is_output_call(&statement.expression) {
                    self.evaluate_output_call(&statement.expression)?;
                    return Some(true);
                }
                let call_count = self.calls.len();
                let value = self.evaluate_expression(&statement.expression, 0)?;
                (self.calls.len() > call_count && self.value_is_output_safe(&value)?)
                    .then_some(false)
            }
            Statement::EmptyStatement(_) => Some(self.saw_output),
            _ => None,
        }
    }

    fn evaluate_expression(
        &mut self,
        expression: &Expression<'_>,
        depth: usize,
    ) -> Option<StaticValue> {
        if depth > MAX_STATIC_LITERAL_DEPTH {
            return None;
        }
        self.touch_node()?;
        if let Some(member) = expression.as_member_expression() {
            return self.evaluate_member(member, depth + 1);
        }
        match expression {
            Expression::NullLiteral(_) => Some(StaticValue::Null),
            Expression::BooleanLiteral(value) => Some(StaticValue::Bool(value.value)),
            Expression::NumericLiteral(value) => Some(StaticValue::Number(value.value)),
            Expression::StringLiteral(value) => {
                self.charge_work(value.value.len())?;
                Some(StaticValue::String(Arc::from(value.value.as_str())))
            }
            Expression::TemplateLiteral(template) => self.evaluate_template(template, depth + 1),
            Expression::Identifier(identifier) => {
                let name = identifier.name.as_str();
                self.charge_work(name.len())?;
                if let Some(value) = self.bindings.get(name) {
                    return Some(value.clone());
                }
                (name == "tools").then_some(StaticValue::ToolNamespace)
            }
            Expression::ArrayExpression(array) => {
                let mut values = Vec::with_capacity(array.elements.len());
                for element in &array.elements {
                    self.touch_literal_item()?;
                    values.push(self.evaluate_expression(element.as_expression()?, depth + 1)?);
                }
                self.array_value(values)
            }
            Expression::ObjectExpression(object) => {
                let mut entries = Vec::with_capacity(object.properties.len());
                let mut keys = HashSet::with_capacity(object.properties.len());
                for property in &object.properties {
                    self.touch_literal_item()?;
                    let ObjectPropertyKind::ObjectProperty(property) = property else {
                        return None;
                    };
                    if property.computed || property.method {
                        return None;
                    }
                    let key = property.key.static_name()?.into_owned();
                    if is_prototype_sensitive_property(&key) {
                        return None;
                    }
                    self.charge_work(key.len())?;
                    let value = self.evaluate_expression(&property.value, depth + 1)?;
                    if !keys.insert(key.clone()) {
                        return None;
                    }
                    entries.push((Arc::from(key), value));
                }
                self.object_value(entries)
            }
            Expression::AwaitExpression(awaited) => {
                self.evaluate_expression(&awaited.argument, depth + 1)
            }
            Expression::ParenthesizedExpression(parenthesized) => {
                self.evaluate_expression(&parenthesized.expression, depth + 1)
            }
            Expression::CallExpression(call) => self.evaluate_call(call, depth + 1),
            Expression::LogicalExpression(logical) if logical.operator.is_coalesce() => {
                let left = self.evaluate_expression(&logical.left, depth + 1)?;
                if left.is_null() {
                    return self.evaluate_expression(&logical.right, depth + 1);
                }
                if matches!(left, StaticValue::OpaqueToolResult) {
                    let prior_calls = self.calls.len();
                    let right = self.evaluate_expression(&logical.right, depth + 1)?;
                    if self.calls.len() != prior_calls || !self.value_is_output_safe(&right)? {
                        return None;
                    }
                    return Some(StaticValue::OpaqueToolResult);
                }
                Some(left)
            }
            Expression::BinaryExpression(binary) if binary.operator.as_str() == "+" => {
                let left = self.evaluate_expression(&binary.left, depth + 1)?;
                let right = self.evaluate_expression(&binary.right, depth + 1)?;
                self.add_static_values(left, right)
            }
            Expression::UnaryExpression(unary) if matches!(unary.operator.as_str(), "+" | "-") => {
                let StaticValue::Number(mut value) =
                    self.evaluate_expression(&unary.argument, depth + 1)?
                else {
                    return None;
                };
                if unary.operator.as_str() == "-" {
                    value = -value;
                }
                value.is_finite().then_some(StaticValue::Number(value))
            }
            _ => None,
        }
    }

    fn evaluate_template(
        &mut self,
        template: &oxc_ast::ast::TemplateLiteral<'_>,
        depth: usize,
    ) -> Option<StaticValue> {
        if template.quasis.len() != template.expressions.len().checked_add(1)? {
            return None;
        }
        let mut rendered = String::new();
        let mut opaque = false;
        for (index, quasi) in template.quasis.iter().enumerate() {
            let cooked = quasi.value.cooked?;
            self.charge_work(cooked.len())?;
            if !opaque {
                if rendered.len().checked_add(cooked.len())? > MAX_STATIC_VALUE_WEIGHT {
                    return None;
                }
                rendered.push_str(cooked.as_str());
            }
            if let Some(expression) = template.expressions.get(index) {
                match self
                    .evaluate_expression(expression, depth + 1)?
                    .interpolation()?
                {
                    StaticInterpolation::Known(value) => {
                        self.charge_work(value.len())?;
                        if !opaque {
                            if rendered.len().checked_add(value.len())? > MAX_STATIC_VALUE_WEIGHT {
                                return None;
                            }
                            rendered.push_str(&value);
                        }
                    }
                    StaticInterpolation::Opaque => opaque = true,
                }
            }
        }
        if opaque {
            Some(StaticValue::OpaqueToolResult)
        } else {
            (rendered.len() <= MAX_STATIC_VALUE_WEIGHT)
                .then(|| StaticValue::String(Arc::from(rendered)))
        }
    }

    fn evaluate_member(
        &mut self,
        member: &MemberExpression<'_>,
        depth: usize,
    ) -> Option<StaticValue> {
        if member.optional() {
            return None;
        }
        let property = member.static_property_name()?;
        if is_prototype_sensitive_property(property) {
            return None;
        }
        self.charge_work(property.len())?;
        let object = self.evaluate_expression(member.object(), depth + 1)?;
        match object {
            StaticValue::ToolNamespace => match property {
                "exec_command" => Some(StaticValue::ToolFunction(StaticTool::ExecCommand)),
                "apply_patch" => Some(StaticValue::ToolFunction(StaticTool::ApplyPatch)),
                _ => None,
            },
            StaticValue::Object(values) => values
                .entries
                .iter()
                .find_map(|(key, value)| (key.as_ref() == property).then(|| value.clone())),
            StaticValue::Array(values) => {
                let index = canonical_array_index(property)?;
                values.values.get(index).cloned()
            }
            StaticValue::OpaqueToolResult => Some(StaticValue::OpaqueToolResult),
            StaticValue::Null
            | StaticValue::Bool(_)
            | StaticValue::Number(_)
            | StaticValue::String(_)
            | StaticValue::ToolFunction(_) => None,
        }
    }

    fn evaluate_call(&mut self, call: &CallExpression<'_>, depth: usize) -> Option<StaticValue> {
        if call.optional || call.type_arguments.is_some() {
            return None;
        }
        if self.is_exact_member_call(call, "Promise", "all") {
            let [argument] = call.arguments.as_slice() else {
                return None;
            };
            return match self.evaluate_argument(argument, depth + 1)? {
                StaticValue::Array(values) => Some(StaticValue::Array(values)),
                _ => None,
            };
        }

        let callee = self.evaluate_expression(&call.callee, depth + 1)?;
        let StaticValue::ToolFunction(tool) = callee else {
            return None;
        };
        let [argument] = call.arguments.as_slice() else {
            return None;
        };
        let argument = self.evaluate_argument(argument, depth + 1)?;
        if self.calls.len() >= MAX_STATIC_NESTED_TOOL_CALLS {
            return None;
        }
        let recovered = match tool {
            StaticTool::ExecCommand => {
                let Value::Object(arguments) = self.materialize_json(&argument)? else {
                    return None;
                };
                StaticNestedToolCall::ExecCommand { arguments }
            }
            StaticTool::ApplyPatch => {
                let StaticValue::String(patch) = argument else {
                    return None;
                };
                self.charge_work(patch.len())?;
                StaticNestedToolCall::ApplyPatch {
                    patch: patch.to_string(),
                }
            }
        };
        self.calls.push(recovered);
        Some(StaticValue::OpaqueToolResult)
    }

    fn evaluate_argument(&mut self, argument: &Argument<'_>, depth: usize) -> Option<StaticValue> {
        self.touch_node()?;
        self.evaluate_expression(argument.as_expression()?, depth + 1)
    }

    fn array_value(&mut self, values: Vec<StaticValue>) -> Option<StaticValue> {
        let expanded_weight = values.iter().try_fold(1usize, |weight, value| {
            weight.checked_add(value.expanded_weight())
        })?;
        if expanded_weight > MAX_STATIC_VALUE_WEIGHT {
            return None;
        }
        Some(StaticValue::Array(Arc::new(StaticArray {
            values,
            expanded_weight,
        })))
    }

    fn object_value(&mut self, entries: Vec<(Arc<str>, StaticValue)>) -> Option<StaticValue> {
        let expanded_weight = entries.iter().try_fold(1usize, |weight, (key, value)| {
            weight
                .checked_add(key.len())?
                .checked_add(value.expanded_weight())
        })?;
        if expanded_weight > MAX_STATIC_VALUE_WEIGHT {
            return None;
        }
        Some(StaticValue::Object(Arc::new(StaticObject {
            entries,
            expanded_weight,
        })))
    }

    fn materialize_json(&mut self, value: &StaticValue) -> Option<Value> {
        self.charge_work(value.expanded_weight())?;
        value.to_json()
    }

    fn add_static_values(&mut self, left: StaticValue, right: StaticValue) -> Option<StaticValue> {
        if matches!(left, StaticValue::OpaqueToolResult)
            || matches!(right, StaticValue::OpaqueToolResult)
        {
            return Some(StaticValue::OpaqueToolResult);
        }
        match (&left, &right) {
            (StaticValue::String(left), right) => {
                let StaticInterpolation::Known(right) = right.interpolation()? else {
                    return Some(StaticValue::OpaqueToolResult);
                };
                self.concatenate_strings(left, &right)
            }
            (left, StaticValue::String(right)) => {
                let StaticInterpolation::Known(left) = left.interpolation()? else {
                    return Some(StaticValue::OpaqueToolResult);
                };
                self.concatenate_strings(&left, right)
            }
            (StaticValue::Number(left), StaticValue::Number(right)) => {
                let value = left + right;
                value.is_finite().then_some(StaticValue::Number(value))
            }
            _ => None,
        }
    }

    fn concatenate_strings(&mut self, left: &str, right: &str) -> Option<StaticValue> {
        let length = left.len().checked_add(right.len())?;
        if length > MAX_STATIC_VALUE_WEIGHT {
            return None;
        }
        self.charge_work(length)?;
        let mut joined = String::with_capacity(length);
        joined.push_str(left);
        joined.push_str(right);
        Some(StaticValue::String(Arc::from(joined)))
    }

    fn is_output_call(&self, expression: &Expression<'_>) -> bool {
        let Expression::CallExpression(call) = expression else {
            return false;
        };
        let Expression::Identifier(callee) = &call.callee else {
            return false;
        };
        !self.bindings.contains_key(callee.name.as_str())
            && matches!(
                callee.name.as_str(),
                "text" | "image" | "generatedImage" | "notify"
            )
    }

    fn evaluate_output_call(&mut self, expression: &Expression<'_>) -> Option<()> {
        let Expression::CallExpression(call) = expression else {
            return None;
        };
        if call.optional || call.type_arguments.is_some() {
            return None;
        }
        let Expression::Identifier(callee) = &call.callee else {
            return None;
        };
        let valid_arity = match callee.name.as_str() {
            "text" | "generatedImage" | "notify" => call.arguments.len() == 1,
            "image" => matches!(call.arguments.len(), 1 | 2),
            _ => false,
        };
        if !valid_arity {
            return None;
        }
        for argument in &call.arguments {
            let value = self.evaluate_output_argument(argument, 1)?;
            if !self.value_is_output_safe(&value)? {
                return None;
            }
        }
        Some(())
    }

    fn evaluate_output_argument(
        &mut self,
        argument: &Argument<'_>,
        depth: usize,
    ) -> Option<StaticValue> {
        let expression = argument.as_expression()?;
        let Expression::CallExpression(call) = expression else {
            return self.evaluate_argument(argument, depth + 1);
        };
        if !self.is_exact_member_call(call, "JSON", "stringify")
            || call.optional
            || call.type_arguments.is_some()
        {
            return self.evaluate_argument(argument, depth + 1);
        }
        self.touch_node()?;
        let [value, replacer, spacing] = call.arguments.as_slice() else {
            return None;
        };
        let value = self.evaluate_argument(value, depth + 1)?;
        let replacer = self.evaluate_argument(replacer, depth + 1)?;
        let spacing = self.evaluate_argument(spacing, depth + 1)?;
        if !self.value_is_output_safe(&value)?
            || !self.value_contains_opaque_tool_result(&value)?
            || !matches!(replacer, StaticValue::Null)
            || !matches!(spacing, StaticValue::Number(value) if value == 2.0)
        {
            return None;
        }
        Some(StaticValue::OpaqueToolResult)
    }

    fn value_is_output_safe(&mut self, value: &StaticValue) -> Option<bool> {
        self.charge_work(value.expanded_weight())?;
        Some(value.is_output_safe())
    }

    fn value_contains_opaque_tool_result(&mut self, value: &StaticValue) -> Option<bool> {
        self.charge_work(value.expanded_weight())?;
        Some(value.contains_opaque_tool_result())
    }

    fn is_exact_member_call(
        &self,
        call: &CallExpression<'_>,
        object: &str,
        property: &str,
    ) -> bool {
        if self.bindings.contains_key(object) {
            return false;
        }
        call.callee.as_member_expression().is_some_and(|member| {
            !member.optional()
                && member.object().is_specific_id(object)
                && member.static_property_name() == Some(property)
        })
    }

    fn touch_node(&mut self) -> Option<()> {
        self.ast_nodes = self.ast_nodes.checked_add(1)?;
        self.charge_work(1)?;
        (self.ast_nodes <= MAX_STATIC_AST_NODES).then_some(())
    }

    fn touch_literal_item(&mut self) -> Option<()> {
        self.literal_items = self.literal_items.checked_add(1)?;
        self.touch_node()?;
        (self.literal_items <= MAX_STATIC_LITERAL_ITEMS).then_some(())
    }

    fn charge_work(&mut self, units: usize) -> Option<()> {
        self.work_units = self.work_units.checked_add(units)?;
        (self.work_units <= MAX_STATIC_WORK_UNITS).then_some(())
    }
}

// This is intentionally not a JavaScript lexer. It is a conservative raw-byte
// envelope applied before Oxc allocates an AST. Every ASCII punctuation byte,
// every ASCII alphanumeric run, and every other non-whitespace byte counts,
// including inside comments and literals. False abstention is preferable to
// interpreting JavaScript twice.
fn has_bounded_parser_complexity(source: &str) -> bool {
    if source.len() > MAX_STATIC_JS_BYTES {
        return false;
    }
    let mut complexity_items = 0usize;
    let mut in_word_run = false;
    for byte in source.bytes() {
        let starts_item = if byte.is_ascii_alphanumeric() {
            let starts_item = !in_word_run;
            in_word_run = true;
            starts_item
        } else {
            in_word_run = false;
            byte.is_ascii_punctuation() || !byte.is_ascii_whitespace()
        };
        if starts_item {
            let Some(updated) = complexity_items.checked_add(1) else {
                return false;
            };
            complexity_items = updated;
            if complexity_items > MAX_STATIC_RAW_COMPLEXITY_ITEMS {
                return false;
            }
        }
    }
    true
}

fn is_prototype_sensitive_property(property: &str) -> bool {
    matches!(property, "__proto__" | "prototype" | "constructor")
}

fn canonical_array_index(property: &str) -> Option<usize> {
    if property == "0" {
        return Some(0);
    }
    if property.starts_with('0') {
        return None;
    }
    let index = property.parse::<u32>().ok()?;
    if index == u32::MAX || index.to_string() != property {
        return None;
    }
    Some(index as usize)
}

fn is_static_global(name: &str) -> bool {
    matches!(
        name,
        "tools" | "Promise" | "JSON" | "text" | "image" | "generatedImage" | "notify"
    )
}

#[cfg(test)]
#[path = "static_js/tests.rs"]
mod hardening_tests;

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "*** Begin Patch\n*** Add File: src/new.rs\n+new\n*** End Patch";

    #[test]
    fn recovers_aliases_templates_and_exact_output_wrappers() {
        let patch = serde_json::to_string(PATCH).expect("patch");
        let source = format!(
            "const action = 'status';\n\
             const command = `git ${{action}}`;\n\
             const args = {{cmd: command, workdir: '/repo'}};\n\
             const run = tools.exec_command;\n\
             const result = await run(args);\n\
             const patch = {patch};\n\
             const applied = await tools.apply_patch(patch);\n\
             text(`${{result.output ?? ''}}${{applied ?? ''}}`);"
        );

        let calls = StaticJsParser::new(&source)
            .parse_program()
            .expect("static program");
        assert_eq!(
            calls,
            vec![
                StaticNestedToolCall::ExecCommand {
                    arguments: serde_json::from_value(serde_json::json!({
                        "cmd": "git status",
                        "workdir": "/repo",
                    }))
                    .expect("arguments"),
                },
                StaticNestedToolCall::ApplyPatch {
                    patch: PATCH.to_owned(),
                },
            ]
        );
    }

    #[test]
    fn recovers_static_promise_all_collection_in_source_order() {
        let patch = serde_json::to_string(PATCH).expect("patch");
        let pending = format!(
            "const patch = {patch};\n\
             const pending = [\n\
               tools.exec_command({{cmd: 'git status'}}),\n\
               tools.apply_patch(patch),\n\
             ];"
        );
        StaticJsParser::new(&pending)
            .parse_program()
            .expect("static collection");
        let promised = format!("{pending}\nconst results = await Promise.all(pending);");
        StaticJsParser::new(&promised)
            .parse_program()
            .expect("static Promise.all binding");
        let source = format!("{promised}\ntext(JSON.stringify(results, null, 2));");

        let calls = StaticJsParser::new(&source)
            .parse_program()
            .expect("static Promise.all");
        assert!(matches!(
            calls.as_slice(),
            [
                StaticNestedToolCall::ExecCommand { .. },
                StaticNestedToolCall::ApplyPatch { .. }
            ]
        ));
    }

    #[test]
    fn template_numbers_use_ecmascript_string_coercion() {
        let source = "const large = 1e21;\n\
                      const negative_zero = -0;\n\
                      await tools.exec_command({\n\
                        cmd: `printf '%s %s' '${large}' '${negative_zero}'`,\n\
                      });";
        let calls = StaticJsParser::new(source)
            .parse_program()
            .expect("static numeric template");
        let [StaticNestedToolCall::ExecCommand { arguments }] = calls.as_slice() else {
            panic!("expected one exec command");
        };
        assert_eq!(
            arguments.get("cmd").and_then(Value::as_str),
            Some("printf '%s %s' '1e+21' '0'")
        );
    }

    #[test]
    fn dynamic_or_ambiguous_programs_abstain() {
        let cases = [
            "if (ready) await tools.apply_patch('*** Begin Patch');",
            "let patch = '*** Begin Patch'; await tools.apply_patch(patch);",
            "const name = dynamic(); await tools.apply_patch(name);",
            "const methods = tools; await methods[method]('*** Begin Patch');",
            "await Promise.all(items.map(run));",
            "const patch = '*** Begin Patch'; patch = other; await tools.apply_patch(patch);",
            "const result = await tools.exec_command({cmd: 'git status'}); unknown(result);",
            "const result = await tools.exec_command({cmd: 'git status'}); const tools = {};",
            "const broken = ; await tools.apply_patch('*** Begin Patch');",
        ];
        for source in cases {
            assert!(
                StaticJsParser::new(source).parse_program().is_none(),
                "unexpectedly accepted {source}"
            );
        }
    }

    #[test]
    fn byte_depth_node_binding_and_call_bounds_abstain() {
        let oversized = " ".repeat(MAX_STATIC_JS_BYTES + 1);
        assert!(StaticJsParser::new(&oversized).parse_program().is_none());

        let deeply_nested = format!(
            "const value = {}0{};",
            "[".repeat(MAX_STATIC_LITERAL_DEPTH + 2),
            "]".repeat(MAX_STATIC_LITERAL_DEPTH + 2)
        );
        assert!(
            StaticJsParser::new(&deeply_nested)
                .parse_program()
                .is_none()
        );

        let too_many_bindings = (0..=MAX_STATIC_BINDINGS)
            .map(|index| format!("const value{index} = {index};"))
            .collect::<String>();
        assert!(
            StaticJsParser::new(&too_many_bindings)
                .parse_program()
                .is_none()
        );

        let too_many_calls = (0..=MAX_STATIC_NESTED_TOOL_CALLS)
            .map(|index| format!("await tools.exec_command({{cmd: 'git status {index}'}});"))
            .collect::<String>();
        assert!(
            StaticJsParser::new(&too_many_calls)
                .parse_program()
                .is_none()
        );

        let too_many_nodes = format!(
            "const values = [{}];",
            std::iter::repeat_n("0", MAX_STATIC_AST_NODES)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(
            StaticJsParser::new(&too_many_nodes)
                .parse_program()
                .is_none()
        );
    }
}
