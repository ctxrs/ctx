use anyhow::Result;
use clap::ValueEnum;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Markdown,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LocateFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SqlFormat {
    Table,
    Json,
    Csv,
    Raw,
}

impl LocateFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

impl OutputFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }
}

pub(crate) fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}
pub(crate) fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, nested| {
                prune_null_json(nested);
                !nested.is_null()
            });
        }
        Value::Array(items) => {
            for item in items {
                prune_null_json(item);
            }
        }
        _ => {}
    }
}
pub(crate) fn print_json(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

pub(crate) fn print_share_safe_value(mut value: Value) -> Result<()> {
    mark_share_safe(&mut value);
    print_json(value)
}

pub(crate) fn mark_share_safe(value: &mut Value) {
    if let Value::Object(map) = value {
        map.entry("share_safe").or_insert(Value::Bool(false));
    }
}

pub(crate) fn effective_format(format: OutputFormat, json: bool) -> OutputFormat {
    if json {
        OutputFormat::Json
    } else {
        format
    }
}

pub(crate) fn locate_json_output(format: LocateFormat, json: bool) -> bool {
    json || format == LocateFormat::Json
}
