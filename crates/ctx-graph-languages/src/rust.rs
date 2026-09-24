use super::common::*;
use crate::model::FileFacts;
use anyhow::Result;
use std::collections::HashMap;
use tree_sitter::Node as Syntax;

pub(super) fn parse(path: &str, source: &str, hash: &str) -> Result<FileFacts> {
    let (root, relative) = if let Some(rest) = path.strip_prefix("src/") {
        (".".to_owned(), rest)
    } else if let Some((root, rest)) = path.rsplit_once("/src/") {
        (root.to_owned(), rest)
    } else {
        (format!("file/{}", module_path(path)), "lib.rs")
    };
    let stem = module_path(relative);
    let module = if stem == "lib" || stem == "main" {
        String::new()
    } else {
        stem.strip_suffix("/mod")
            .unwrap_or(&stem)
            .replace('/', "::")
    };
    let mut e = Extractor::new(path, source, hash, "rust", module.clone());
    let Some(tree) = tree(tree_sitter_rust::LANGUAGE.into(), source, &mut e.facts)? else {
        return Ok(e.facts);
    };
    e.root(tree.root_node(), format!("rust:module:{root}:{module}"));
    e.scopes[0].fallback = Some(format!("rust:{root}:{}", prefix(&module)));
    let mut r = Rust {
        e,
        root,
        modules: HashMap::from([(0, module.clone())]),
        module_bindings: HashMap::new(),
        module_aliases: HashMap::new(),
        value_bindings: HashMap::new(),
        local_uses: vec![],
        paths: vec![],
        implementations: vec![],
        receivers: vec![],
    };
    r.visit(tree.root_node(), 0, &module, None);
    Ok(r.finish())
}
fn public(node: Syntax<'_>, source: &str) -> bool {
    children(node)
        .iter()
        .any(|n| n.kind() == "visibility_modifier" && &source[n.byte_range()] == "pub")
}
fn prefix(module: &str) -> String {
    if module.is_empty() {
        String::new()
    } else {
        format!("{module}::")
    }
}
struct LocalUse {
    scope: usize,
    name: String,
    parts: Vec<String>,
    key: String,
    references: std::ops::Range<usize>,
}
struct Rust<'a> {
    e: Extractor<'a>,
    root: String,
    modules: HashMap<usize, String>,
    // Modules occupy the type namespace; a same-named function is a value.
    module_bindings: HashMap<usize, HashMap<String, Option<String>>>,
    // Only aliases of proven module heads belong here, never imported members.
    module_aliases: HashMap<usize, HashMap<String, String>>,
    // Unknown call targets can still be proven to occupy only the value namespace.
    value_bindings: HashMap<usize, HashMap<String, bool>>,
    local_uses: Vec<LocalUse>,
    paths: Vec<(usize, usize, Vec<String>, String)>,
    implementations: Vec<(String, usize, Vec<String>, String)>,
    receivers: Vec<(String, usize, Vec<String>, String)>,
}

mod rust_callable_sequence;
mod rust_visit;
