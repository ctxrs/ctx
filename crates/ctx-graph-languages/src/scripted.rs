//! Native scripted-language extraction. Parsing never evaluates code or follows imports.
use super::common::*;
use crate::model::{FileFacts, Node, Reference};
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Language, Node as Syntax};

pub fn supports(path: &str) -> bool {
    language(path).is_some()
}
fn language(path: &str) -> Option<&'static str> {
    Some(match path.rsplit('.').next()? {
        "rb" | "rake" | "gemspec" => "ruby",
        "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "phps" => "php",
        "lua" => "lua",
        "toc" => "lua-manifest",
        "luau" => "luau",
        "sh" | "bash" | "zsh" | "ksh" => "bash",
        "ps1" | "psm1" | "psd1" => "powershell",
        "ex" | "exs" => "elixir",
        _ if matches!(
            path.rsplit('/').next(),
            Some("Gemfile" | "Rakefile" | "Guardfile")
        ) =>
        {
            "ruby"
        }
        _ => return None,
    })
}
pub fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    let Some(lang) = language(path).or_else(|| shebang_language(source)) else {
        return Ok(None);
    };
    parse_named(path, source, hash, lang)
}
/// Recognize a literal interpreter name without invoking env or a shell.
pub use ctx_graph_types::shebang_language;
/// Select a native grammar while preserving the actual source path and ranges.
pub fn parse_named(
    path: &str,
    source: &str,
    hash: &str,
    language: &str,
) -> Result<Option<FileFacts>> {
    let lang = match language {
        "ruby" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "luau" => "luau",
        "bash" | "sh" => "bash",
        "powershell" | "pwsh" => "powershell",
        "elixir" => "elixir",
        "lua-manifest" => "lua-manifest",
        _ => return Ok(None),
    };
    if path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        bail!("source path must be a normalized relative POSIX path");
    }
    if lang == "lua-manifest" {
        return Ok(Some(lua_manifest(path, source, hash)));
    }
    let grammar: Language = match lang {
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "luau" => tree_sitter_luau::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "powershell" => tree_sitter_powershell::LANGUAGE.into(),
        _ => tree_sitter_elixir::LANGUAGE.into(),
    };
    let mut e = Extractor::new(path, source, hash, lang, module_path(path));
    let Some(tree) = tree(grammar, source, &mut e.facts)? else {
        return Ok(Some(e.facts));
    };
    let root = tree.root_node();
    e.root(root, format!("{lang}:module:{}", e.facts.module));
    let mut facts = match lang {
        "ruby" => Ruby::new(e).extract(root),
        "php" => Php::new(e).extract(root),
        "elixir" => Elixir::new(e).extract(root),
        _ => Script::new(e).extract(root),
    };
    if lang == "php" {
        let regions = descendants(root, "text")
            .into_iter()
            .map(|n| n.byte_range())
            .collect::<Vec<_>>();
        super::templates::append_inline_javascript(&mut facts, source, &regions)?;
    }
    Ok(Some(facts))
}
fn child<'a>(n: Syntax<'a>, kind: &str) -> Option<Syntax<'a>> {
    children(n).into_iter().find(|n| n.kind() == kind)
}
fn field<'a>(n: Syntax<'a>, name: &str) -> Option<Syntax<'a>> {
    n.child_by_field_name(name)
}
fn descendants<'a>(n: Syntax<'a>, kind: &str) -> Vec<Syntax<'a>> {
    let mut found = vec![];
    let mut pending = vec![n];
    while let Some(n) = pending.pop() {
        if n.kind() == kind {
            found.push(n);
        } else {
            pending.extend(children(n).into_iter().rev());
        }
    }
    found
}
fn literal(e: &Extractor<'_>, n: Syntax<'_>) -> Option<String> {
    let text = e.text(n);
    if matches!(
        n.kind(),
        "word" | "command_name" | "generic_token" | "path_command_name" | "path_command_name_token"
    ) && n.named_child_count() == 0
    {
        return (!text.is_empty() && !text.contains(['$', '`', '\\', '*', '?']))
            .then(|| text.into());
    }
    if matches!(
        n.kind(),
        "string" | "raw_string" | "string_literal" | "encapsed_string"
    ) {
        if children(n).iter().any(|c| {
            matches!(
                c.kind(),
                "interpolation"
                    | "string_interpolation"
                    | "simple_expansion"
                    | "expansion"
                    | "command_substitution"
                    | "variable_name"
                    | "variable"
                    | "sub_expression"
            )
        }) {
            return None;
        }
        if text.len() >= 2
            && matches!(text.as_bytes()[0], b'\'' | b'"')
            && text.as_bytes().last() == text.as_bytes().first()
            && !text.contains(['\\', '`'])
        {
            let inner = &text[1..text.len() - 1];
            if text.starts_with('"') && (inner.contains('$') || inner.contains("#{")) {
                return None;
            }
            return Some(inner.into());
        }
        if let Some(content) = field(n, "content") {
            return Some(e.text(content).into());
        }
    }
    if n.named_child_count() == 1
        && matches!(
            n.kind(),
            "argument"
                | "arguments"
                | "command_name"
                | "command_name_expr"
                | "path_command_name"
                | "array_literal_expression"
                | "unary_expression"
                | "parenthesized_expression"
        )
    {
        return literal(e, n.named_child(0)?);
    }
    None
}
fn path_modules(e: &Extractor<'_>, name: &str, relative: bool) -> Vec<String> {
    if name.starts_with('/') || name.contains(['\\', '$', '`']) {
        return vec![];
    }
    let base = if relative {
        e.facts.path.rsplit_once('/').map_or("", |(p, _)| p)
    } else {
        ""
    };
    relative_path(base, name)
        .map(|p| {
            let known = p.rsplit_once('.').is_some_and(|(_, ext)| {
                matches!(
                    ext,
                    "rb" | "php" | "phtml" | "sh" | "bash" | "ps1" | "psm1" | "psd1"
                )
            });
            vec![if known { module_path(&p) } else { p }]
        })
        .unwrap_or_default()
}
fn unknown_parameters(e: &mut Extractor<'_>, n: Syntax<'_>, scope: usize, kinds: &[&str]) {
    for kind in kinds {
        for name in descendants(n, kind) {
            e.bind(scope, e.text(name), Binding::Unknown);
        }
    }
}
fn qualified(prefix: &str, name: &str, sep: &str) -> String {
    if prefix.is_empty() {
        name.into()
    } else {
        format!("{prefix}{sep}{name}")
    }
}

// Lua, Bash and PowerShell have file-scoped exports; imported filenames are
// normalized lexically, never read. All ordinary calls use the shared scope resolver.
#[derive(Clone)]
struct BashPath {
    value: String,
    anchored: bool,
}
struct Script<'a> {
    e: Extractor<'a>,
    exported_table: Option<String>,
    sourced: HashMap<usize, Vec<String>>,
    type_calls: Vec<(usize, usize, String, String)>,
    invalidated_prefixes: Vec<String>,
    invalidated_members: HashSet<String>,
    manifest_modules: Vec<String>,
    bash_paths: HashMap<(usize, String), Option<BashPath>>,
    bash_functions: HashSet<String>,
    import_scopes: Vec<(usize, usize)>,
}

struct RubyOwner {
    count: usize,
    class: bool,
    bases: Vec<String>,
    dynamic_base: bool,
    instance_barrier: bool,
    singleton_barrier: bool,
    visibility: HashMap<String, bool>,
}

/// A writer-side snapshot of exact Ruby bindings. No source or dependency is loaded.
pub struct RubyContext {
    unsafe_lookup: bool,
    owners: HashMap<String, RubyOwner>,
    methods: HashMap<String, (usize, bool, bool)>,
    method_owners: HashMap<String, String>,
    self_calls: HashSet<String>,
}

struct Ruby<'a> {
    e: Extractor<'a>,
    owners: HashMap<usize, String>,
    singleton: HashSet<usize>,
    unsafe_lookup: bool,
    lexical: HashMap<String, Vec<String>>,
    call_labels: HashMap<String, String>,
    module_functions: HashSet<usize>,
    exported_methods: HashSet<String>,
    extended_self: HashSet<String>,
    attribute_shadows: HashSet<String>,
    instance_barriers: HashSet<String>,
    singleton_barriers: HashSet<String>,
    inheritance_unsafe: bool,
    visibility: HashMap<usize, &'static str>,
    visibility_overrides: HashMap<String, HashMap<String, String>>,
    self_calls: HashSet<String>,
}

#[derive(Clone, Default)]
struct PhpContext {
    namespace: String,
    class: Option<String>,
    aliases: HashMap<String, String>,
    functions: HashMap<String, String>,
}
struct Php<'a> {
    e: Extractor<'a>,
    semantic_sources: Vec<(usize, String, &'static str)>,
    config_uses: Vec<(usize, String)>,
}

type ElixirSignatures = HashSet<(String, usize)>;

#[derive(Clone, Default)]
struct ElixirContext {
    module: String,
    aliases: HashMap<String, String>,
    imports: Vec<(String, Option<ElixirSignatures>, ElixirSignatures)>,
}
struct Elixir<'a> {
    e: Extractor<'a>,
    functions: HashMap<(String, String, usize), (String, usize)>,
    pending: Vec<(usize, String, String, usize, ElixirContext)>,
}

// TOC is a line-oriented addon manifest, not Lua source. Entries never execute.
fn lua_manifest(path: &str, source: &str, hash: &str) -> FileFacts {
    use crate::model::{Node, Reference};
    let module = module_path(path);
    let owner = format!("lua:{path}:manifest");
    let mut facts = FileFacts {
        path: path.into(),
        hash: hash.into(),
        module: module.clone(),
        nodes: vec![],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    };
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut facts, None, "Source exceeds the 4 MiB indexing limit");
        return facts;
    }
    facts.nodes.push(Node { id: owner.clone(), label: module.clone(), kind: "manifest".into(), file: path.into(), line: Some(1), end_line: Some(source.lines().count().max(1) as u32), qualified_name: Some(module.clone()), binding_key: Some(format!("lua:manifest:{module}")), metadata: serde_json::json!({"language":"lua-manifest","start_byte":0,"end_byte":source.len(),"manifest":{}}) });
    let base = path.rsplit_once('/').map_or("", |(base, _)| base);
    for (row, line) in source.lines().enumerate() {
        let text = line.trim().trim_start_matches('\u{feff}');
        if text.is_empty() {
            continue;
        }
        if let Some(metadata) = text.strip_prefix("##") {
            if let Some((name, value)) = metadata.split_once(':') {
                facts.nodes[0].metadata["manifest"][name.trim()] = value.trim().into();
                if matches!(
                    name.trim(),
                    "Dependencies" | "RequiredDeps" | "OptionalDeps"
                ) {
                    for (index, dependency) in value
                        .split(',')
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .enumerate()
                    {
                        facts.references.push(Reference {
                            id: format!("imports:{owner}:{row}:{index}"),
                            source: owner.clone(),
                            label: dependency.into(),
                            relation: "imports".into(),
                            file: path.into(),
                            line: row as u32 + 1,
                            candidate_keys: vec![],
                            reason: "addon load order and installed dependencies are external"
                                .into(),
                        });
                    }
                }
            }
            continue;
        }
        if text.starts_with('#') {
            continue;
        }
        let normalized = text.replace('\\', "/");
        let target = (!normalized.starts_with('/') && !normalized.contains(['$', ':']))
            .then(|| relative_path(base, &normalized))
            .flatten();
        let keys = target
            .as_ref()
            .map(|p| match p.rsplit('.').next() {
                Some("lua") => vec![format!("lua:module:{}", module_path(p))],
                Some("luau") => vec![format!("luau:module:{}", module_path(p))],
                _ => vec![],
            })
            .unwrap_or_default();
        if target.is_none() {
            diagnostic(
                &mut facts,
                Some(row as u32 + 1),
                "Manifest entry is not a repository-relative static path",
            );
        }
        facts.references.push(Reference {
            id: format!("imports:{owner}:{row}"),
            source: owner.clone(),
            label: text.into(),
            relation: "imports".into(),
            file: path.into(),
            line: row as u32 + 1,
            candidate_keys: keys,
            reason: "manifest entry is dynamic, non-code, or unavailable".into(),
        });
    }
    facts
}

mod elixir_new;
mod php_new;
mod ruby_new;
mod ruby_visit;
mod rubycontext_from_nodes;
mod script_bash;
mod script_new;
