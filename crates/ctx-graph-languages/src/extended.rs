//! Additional grammar-backed languages. Binding keys use explicit namespaces or
//! complete file paths; unresolved receiver types never fall back to a bare name.
use super::common::{Binding, Extractor, children, diagnostic, module_path, relative_path, tree};
use crate::model::FileFacts;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use tree_sitter::{Language, Node as Syntax};

pub(super) fn supports(path: &str) -> bool {
    grammar(path).is_some()
        || matches!(
            path.rsplit('.').next(),
            Some("dmm" | "dmf" | "dmi" | "dfm" | "lfm" | "lpk")
        )
}

pub(super) fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    if path.ends_with(".dmi") {
        let mut f = asset_facts(path, hash);
        diagnostic(&mut f, None, "DMI requires binary PNG ingestion");
        return Ok(Some(f));
    }
    if matches!(path.rsplit('.').next(), Some("dmm" | "dmf")) {
        return Ok(Some(asset(path, source, hash)));
    }
    match path.rsplit('.').next() {
        Some("dfm" | "lfm") => {
            return Ok(Some(parse_pascal_form_bytes(
                path,
                source.as_bytes(),
                hash,
            )?));
        }
        Some("lpk") => return Ok(Some(pascal_package_xml(path, source, hash))),
        Some("dpk") => return Ok(Some(pascal_package_source(path, source, hash))),
        _ => {}
    }
    let Some((lang, language)) = grammar(path) else {
        return Ok(None);
    };
    parse_grammar(path, source, hash, lang, language)
}

/// Parse a language selected by the caller, preserving the original script path.
pub(super) fn parse_named(
    path: &str,
    source: &str,
    hash: &str,
    language: &str,
) -> Result<Option<FileFacts>> {
    if language != "julia" {
        return Ok(None);
    }
    parse_grammar(
        path,
        source,
        hash,
        "julia",
        tree_sitter_julia::LANGUAGE.into(),
    )
}

type ContextReference<'t> = (Syntax<'t>, usize, String, &'static str, Vec<String>, String);

struct Extended<'s, 't> {
    e: Extractor<'s>,
    prefixes: Vec<String>,
    pending: Vec<(Syntax<'t>, usize, String, &'static str, Vec<String>)>,
    contextual: Vec<ContextReference<'t>>,
    value_types: HashMap<(usize, String), Option<String>>,
    dart_packages: HashSet<String>,
    dart_local_types: HashSet<String>,
    dart_bloc_types: HashMap<String, String>,
    cpp_defines: HashMap<String, Option<String>>,
    groovy_parameters: HashMap<usize, Vec<String>>,
}

#[derive(Clone)]
struct Token<'a> {
    text: &'a str,
    line: u32,
    quoted: bool,
}

/// Read only PNG metadata. Bitmap data is skipped, never decoded into pixels.
pub fn parse_dmi(path: &str, bytes: &[u8], hash: &str) -> Result<FileFacts> {
    anyhow::ensure!(
        !path.starts_with('/')
            && !path.contains('\\')
            && !path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".."),
        "source path must be a normalized relative POSIX path"
    );
    let mut f = asset_facts(path, hash);
    if bytes.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut f, None, "DMI exceeds the 4 MiB indexing limit");
        return Ok(f);
    }
    let descriptions = (|| -> Result<Vec<String>> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_limits(png::Limits {
            bytes: crate::parser::MAX_SOURCE_BYTES,
        });
        let mut reader = decoder.read_info()?;
        reader.finish()?;
        let info = reader.info();
        let mut descriptions = vec![];
        let mut remaining = 256 * 1024;
        for text in &info.uncompressed_latin1_text {
            if text.keyword == "Description" {
                anyhow::ensure!(
                    text.text.len() <= remaining,
                    "DMI description exceeds limit"
                );
                remaining -= text.text.len();
                descriptions.push(text.text.clone());
            }
        }
        for text in &info.compressed_latin1_text {
            if text.keyword == "Description" {
                let mut text = text.clone();
                text.decompress_text_with_limit(remaining)?;
                let value = text.get_text()?;
                anyhow::ensure!(value.len() <= remaining, "DMI description exceeds limit");
                remaining -= value.len();
                descriptions.push(value);
            }
        }
        for text in &info.utf8_text {
            if text.keyword == "Description" {
                let mut text = text.clone();
                text.decompress_text_with_limit(remaining)?;
                let value = text.get_text()?;
                anyhow::ensure!(value.len() <= remaining, "DMI description exceeds limit");
                remaining -= value.len();
                descriptions.push(value);
            }
        }
        anyhow::ensure!(
            descriptions.iter().map(String::len).sum::<usize>() <= 256 * 1024,
            "DMI description exceeds limit"
        );
        Ok(descriptions)
    })();
    let descriptions = match descriptions {
        Ok(d) => d,
        Err(_) => {
            diagnostic(
                &mut f,
                None,
                "Invalid PNG or DMI metadata exceeds the indexing limit",
            );
            return Ok(f);
        }
    };
    let root = asset_node(&mut f, path, "asset", 1, None);
    let mut current = None;
    for description in descriptions {
        for (i, line) in description.lines().enumerate() {
            let Some((name, value)) = line.trim().split_once('=') else {
                continue;
            };
            let value = value.trim();
            if name.trim() == "state"
                && value.starts_with('"')
                && value.ends_with('"')
                && value.len() >= 2
            {
                current = Some(asset_node(
                    &mut f,
                    &value[1..value.len() - 1],
                    "icon_state",
                    i as u32 + 1,
                    Some(root.clone()),
                ));
            } else if matches!(name.trim(), "dirs" | "frames" | "width" | "height")
                && let Ok(value) = value.parse::<u32>()
            {
                let id = current.as_ref().unwrap_or(&root);
                if let Some(n) = f.nodes.iter_mut().find(|n| &n.id == id) {
                    n.metadata[name.trim()] = serde_json::json!(value);
                }
            }
        }
    }
    Ok(f)
}

// Pascal strings, comments, and property blobs stay opaque. This lexer is used
// only by declarative forms and Delphi package headers, not Pascal source code.

/// Parse text Delphi/Lazarus forms and diagnose binary Delphi resources before
/// attempting UTF-8 decoding.
pub fn parse_pascal_form_bytes(path: &str, bytes: &[u8], hash: &str) -> Result<FileFacts> {
    anyhow::ensure!(
        !path.starts_with('/')
            && !path.contains('\\')
            && !path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".."),
        "source path must be a normalized relative POSIX path"
    );
    let mut f = asset_facts(path, hash);
    if bytes.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut f, None, "Form exceeds the 4 MiB indexing limit");
        return Ok(f);
    }
    if bytes.starts_with(b"TPF0") || bytes.starts_with(&[0xff, 0x0a]) {
        diagnostic(
            &mut f,
            None,
            "Binary DFM is unsupported; save the form as text in Delphi to index it",
        );
        return Ok(f);
    }
    let source = match std::str::from_utf8(bytes) {
        Ok(s) => s.trim_start_matches('\u{feff}'),
        Err(_) => {
            diagnostic(&mut f, None, "Form is not UTF-8 text");
            return Ok(f);
        }
    };
    let tokens = match pascal_tokens(source) {
        Ok(t) => t,
        Err(e) => {
            diagnostic(&mut f, None, &e.to_string());
            return Ok(f);
        }
    };
    let root = data_node(&mut f, "pascal", path, "asset", 1, None);
    let mut stack: Vec<(String, String)> = vec![];
    let mut instances = std::collections::HashMap::<String, Vec<String>>::new();
    let mut properties = vec![];
    let result = (|| -> Result<()> {
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            if !t.quoted
                && matches!(
                    t.text.to_ascii_lowercase().as_str(),
                    "object" | "inherited" | "inline"
                )
            {
                anyhow::ensure!(
                    i + 3 < tokens.len()
                        && pascal_identifier(tokens[i + 1].text)
                        && tokens[i + 2].text == ":"
                        && pascal_identifier(tokens[i + 3].text),
                    "Invalid form component declaration"
                );
                anyhow::ensure!(stack.len() < 256, "Form nesting exceeds indexing limit");
                let instance = tokens[i + 1].text;
                let class = tokens[i + 3].text;
                let parent = stack.last().map_or(&root, |p| &p.0).clone();
                let qualified = stack
                    .last()
                    .map_or_else(|| instance.into(), |p| format!("{}.{}", p.1, instance));
                let id = data_node(&mut f, "pascal", class, "component", t.line, Some(parent));
                let key = format!("pascal:form:{path}:{}", qualified.to_lowercase());
                let node = f.nodes.last_mut().unwrap();
                node.qualified_name = Some(qualified.clone());
                node.binding_key = Some(key.clone());
                node.metadata["name"] = serde_json::json!(instance);
                node.metadata["class"] = serde_json::json!(class);
                node.metadata["declaration"] = serde_json::json!(t.text.to_ascii_lowercase());
                node.metadata["properties"] = serde_json::json!({});
                instances
                    .entry(instance.to_lowercase())
                    .or_default()
                    .push(key);
                data_reference(
                    &mut f,
                    &id,
                    class,
                    "uses_type",
                    t.line,
                    vec![],
                    "component class unit is unknown",
                );
                stack.push((id, qualified));
                i += 4;
                // Streaming form files may carry an inherited component index.
                if tokens.get(i).is_some_and(|t| t.text == "[") {
                    i += 1;
                    while i < tokens.len() && tokens[i].text != "]" {
                        i += 1;
                    }
                    anyhow::ensure!(i < tokens.len(), "Unterminated component index");
                    i += 1;
                }
            } else if !t.quoted && t.text.eq_ignore_ascii_case("end") {
                let (id, _) = stack
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("Unmatched form end"))?;
                f.nodes.iter_mut().find(|n| n.id == id).unwrap().end_line = Some(t.line);
                i += 1;
            } else if i + 1 < tokens.len() && pascal_identifier(t.text) && tokens[i + 1].text == "="
            {
                let (owner, _) = stack
                    .last()
                    .ok_or_else(|| anyhow::anyhow!("Property outside a form component"))?;
                let owner = owner.clone();
                i += 2;
                let start = i;
                let mut closing = vec![];
                while i < tokens.len() {
                    let value = &tokens[i];
                    if closing.is_empty() && value.line > t.line && i > start {
                        break;
                    }
                    if closing.is_empty()
                        && i == start
                        && value.line > t.line
                        && !matches!(value.text, "(" | "<" | "[")
                    {
                        break;
                    }
                    if !value.quoted {
                        match value.text {
                            "(" => closing.push(")"),
                            "<" => closing.push(">"),
                            "[" => closing.push("]"),
                            ")" | ">" | "]" => anyhow::ensure!(
                                closing.pop() == Some(value.text),
                                "Unbalanced form property"
                            ),
                            _ => {}
                        }
                        anyhow::ensure!(
                            closing.len() <= 256,
                            "Property nesting exceeds indexing limit"
                        );
                    }
                    i += 1;
                    if closing.is_empty() && tokens.get(i).is_none_or(|v| v.line > value.line) {
                        break;
                    }
                }
                anyhow::ensure!(closing.is_empty(), "Unterminated form property");
                let values = &tokens[start..i];
                if values.len() == 1 {
                    let v = &values[0];
                    let raw = pascal_string(v);
                    let value = if v.quoted {
                        serde_json::json!(raw)
                    } else if raw.eq_ignore_ascii_case("true") {
                        serde_json::json!(true)
                    } else if raw.eq_ignore_ascii_case("false") {
                        serde_json::json!(false)
                    } else if let Ok(n) = raw.parse::<i64>() {
                        serde_json::json!(n)
                    } else {
                        serde_json::json!(raw)
                    };
                    f.nodes.iter_mut().find(|n| n.id == owner).unwrap().metadata["properties"]
                        [t.text] = value;
                    if !v.quoted
                        && pascal_identifier(&raw)
                        && !matches!(raw.to_ascii_lowercase().as_str(), "true" | "false" | "nil")
                    {
                        let event = t.text.to_ascii_lowercase().starts_with("on");
                        properties.push((owner, raw, t.line, event, t.text.to_string()));
                    }
                }
            } else {
                anyhow::bail!("Unsupported or malformed form statement at line {}", t.line);
            }
        }
        anyhow::ensure!(stack.is_empty(), "Unterminated form component");
        Ok(())
    })();
    if let Err(e) = result {
        invalid_data(&mut f, &e.to_string());
        return Ok(f);
    }
    for (owner, value, line, event, property) in properties {
        let keys = if event {
            vec![]
        } else {
            instances
                .get(&value.to_lowercase())
                .filter(|v| v.len() == 1)
                .cloned()
                .unwrap_or_default()
        };
        data_reference(
            &mut f,
            &owner,
            &value,
            "references",
            line,
            keys,
            &if event {
                format!("event property {property}; handler unit is unknown")
            } else {
                format!("property {property}; target is unavailable or ambiguous")
            },
        );
    }
    Ok(f)
}

/// File families whose explicit declarations can supply extended project links.
pub fn applies(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some("h" | "m" | "mm" | "pas" | "pp" | "dpr" | "lpr" | "inc" | "dfm" | "lfm")
    )
}

/// Source-only project navigation. Declarations are retained separately; an
/// ambiguous class, implementation, overload or ancestor never becomes a link.
#[derive(Default)]
pub struct ExtendedContext {
    nodes: HashMap<String, crate::model::Node>,
    imports: HashMap<String, Vec<String>>,
    groups: HashMap<String, String>,
    bindings: HashMap<String, Option<String>>,
    source_hashes: HashMap<String, String>,
    fingerprint: String,
}

// A parse-only view of syntax missing from the released Groovy grammar. Every
// replacement has the original byte length, so Extractor reads original text.
struct GroovySource {
    source: String,
    parameters: HashMap<usize, Vec<String>>,
    spans: Vec<serde_json::Value>,
}
struct GroovyToken<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    quoted: bool,
}

#[cfg(test)]
mod named_tests;

mod extended_fact_node;
mod extended_imports;
mod extended_norm;
mod extended_owner_metadata;
mod extended_visit;
mod extendedcontext_discover;

mod grammar;
use grammar::*;
mod pascal_package_xml;
use pascal_package_xml::*;
