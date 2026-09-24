//! Static template facts. Embedded scripts keep the original byte coordinates.
//! Native Robot extraction covers a static subset. An explicitly selected Python
//! environment can provide the official static model; neither route runs suites.
use super::common::{children, diagnostic, module_path, relative_path, tree};
use crate::model::{Edge, FileFacts, Node, Reference};
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, ops::Range, path::Path};

pub fn supports(path: &str) -> bool {
    path.ends_with(".blade.php")
        || matches!(
            path.rsplit('.').next(),
            Some("vue" | "svelte" | "astro" | "razor" | "cshtml" | "xaml" | "robot" | "resource")
        )
}

pub fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    if !supports(path) {
        return Ok(None);
    }
    if path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|s| matches!(s, "" | "." | ".."))
    {
        bail!("source path must be a normalized relative POSIX path");
    }
    let lang = if path.ends_with(".blade.php") {
        "blade"
    } else {
        path.rsplit('.').next().unwrap()
    };
    let mut out = Template::new(path, source, hash, lang);
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut out.f, None, "Source exceeds the 4 MiB indexing limit");
        return Ok(Some(out.f));
    }
    out.node(
        path,
        "module",
        0..source.len(),
        Some(format!("template:file:{path}")),
        None,
    );
    match lang {
        "robot" | "resource" => out.robot(),
        "xaml" => out.xaml(),
        _ => out.markup(lang)?,
    }
    Ok(Some(out.f))
}

/// Explicit opt-in to an installed Robot Framework 7.5.x static parser.
/// Uses isolated Python with bounded stdin/stdout and a ten-second deadline.
/// Never loads declared libraries, variable files or resources, or runs a suite.
/// Changing the installed Robot version requires a forced reindex.
pub use robot_official::parse_robot_official;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotModel {
    schema_version: u32,
    robot_version: String,
    failure: Option<String>,
    languages: Vec<String>,
    definitions: Vec<RobotDefinition>,
    imports: Vec<RobotImport>,
    calls: Vec<RobotCall>,
    diagnostics: Vec<RobotDiagnostic>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotSpan {
    start: usize,
    end: usize,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum RobotDefinitionKind {
    Test,
    Keyword,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotDefinition {
    id: usize,
    kind: RobotDefinitionKind,
    name: String,
    span: RobotSpan,
    embedded: bool,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum RobotImportKind {
    Resource,
    Library,
    Variables,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotImport {
    kind: RobotImportKind,
    name: String,
    alias: Option<String>,
    span: RobotSpan,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotCall {
    owner: Option<usize>,
    name: String,
    span: RobotSpan,
    alternatives: Vec<String>,
    target: Option<usize>,
    ambiguous: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotDiagnostic {
    code: String,
    span: RobotSpan,
}

struct Template<'a> {
    f: FileFacts,
    source: &'a str,
    language: &'a str,
    lines: Vec<usize>,
}

/// Index literal script bodies only in source-ordered regions identified as markup.
/// All masks retain UTF-8 byte offsets, including CRLF and non-ASCII host text.
pub(super) fn append_inline_javascript(
    host: &mut FileFacts,
    source: &str,
    regions: &[Range<usize>],
) -> Result<()> {
    if regions.is_empty() {
        return Ok(());
    }
    let visible = mask_ranges(source, regions);
    let mut classic = Vec::new();
    let mut modules = Vec::new();
    let mut pos = 0;
    while let Some(offset) = visible[pos..].find('<') {
        pos += offset;
        if visible[pos..].starts_with("<!--") {
            pos = visible[pos + 4..]
                .find("-->")
                .map_or(visible.len(), |end| pos + 4 + end + 3);
            continue;
        }
        let Some(tag) = tag_at(&visible, pos) else {
            // Do not look inside a malformed opening tag's quoted attributes.
            if visible
                .as_bytes()
                .get(pos + 1)
                .is_some_and(u8::is_ascii_alphabetic)
            {
                break;
            }
            pos += 1;
            continue;
        };
        pos = tag.range.end;
        let name = tag.name.to_ascii_lowercase();
        if name == "plaintext" {
            break;
        }
        if !matches!(
            name.as_str(),
            "script"
                | "style"
                | "textarea"
                | "title"
                | "xmp"
                | "iframe"
                | "noembed"
                | "noframes"
                | "noscript"
        ) {
            continue;
        }
        let Some((end, after)) = find_close_tag(&visible, pos, &format!("</{name}")) else {
            if name == "script" {
                diagnostic(
                    host,
                    Some(
                        source[..tag.range.start]
                            .bytes()
                            .filter(|b| *b == b'\n')
                            .count() as u32
                            + 1,
                    ),
                    "Unclosed inline script block; JavaScript omitted",
                );
            }
            break;
        };
        let body = pos..end;
        pos = after;
        if name != "script"
            || !regions
                .get(
                    regions
                        .partition_point(|r| r.start <= tag.range.start)
                        .saturating_sub(1),
                )
                .is_some_and(|r| r.start <= tag.range.start && after <= r.end)
            || tag.attrs.iter().any(|a| a.name.eq_ignore_ascii_case("src"))
        {
            continue;
        }
        // Reject unknown or conflicting script language declarations, including data blocks.
        if tag.attrs.iter().any(|a| {
            let value = a.value.trim().to_ascii_lowercase();
            match a.name.to_ascii_lowercase().as_str() {
                "type" => !matches!(
                    value.as_str(),
                    "" | "module"
                        | "text/javascript"
                        | "application/javascript"
                        | "text/ecmascript"
                        | "application/ecmascript"
                ),
                "lang" | "language" => {
                    !matches!(value.as_str(), "" | "js" | "javascript" | "ecmascript")
                }
                _ => false,
            }
        }) {
            continue;
        }
        if tag.attrs.iter().any(|a| {
            a.name.eq_ignore_ascii_case("type") && a.value.trim().eq_ignore_ascii_case("module")
        }) {
            if modules.len() == 128 {
                diagnostic(
                    host,
                    None,
                    "Inline module script limit reached; remaining scripts omitted",
                );
                break;
            }
            modules.push(body);
        } else {
            classic.push(body);
        }
    }
    // Classic blocks share file scope; each module script has its own scope.
    let groups = std::iter::once(classic).chain(modules.into_iter().map(|r| vec![r]));
    for (group, ranges) in groups.enumerate() {
        if ranges.is_empty() {
            continue;
        }
        let masked = mask_ranges(source, &ranges);
        if ranges.len() > 1 {
            // Masking must not join two incomplete scripts into a fabricated declaration/call.
            let Some(syntax) = tree(tree_sitter_javascript::LANGUAGE.into(), &masked, host)? else {
                continue;
            };
            if children(syntax.root_node()).into_iter().any(|n| {
                !n.is_extra()
                    && !ranges
                        .get(
                            ranges
                                .partition_point(|r| r.start <= n.start_byte())
                                .saturating_sub(1),
                        )
                        .is_some_and(|r| r.start <= n.start_byte() && n.end_byte() <= r.end)
            }) {
                diagnostic(
                    host,
                    None,
                    "JavaScript syntax crosses script block boundaries; classic scripts omitted",
                );
                continue;
            }
        }
        let mut js = super::javascript::parse(&host.path, &masked, &host.hash)?;
        host.diagnostics.append(&mut js.diagnostics);
        if js.nodes.is_empty() {
            continue;
        }
        let root = js.nodes.remove(0).id;
        let owner = &host.nodes[0].id;
        // Keep native language/byte-based IDs. Namespace every local binding and alias
        // so page.php cannot export into page.js, another PHP suffix, or a sibling module.
        let keys: HashMap<_, _> = js
            .nodes
            .iter()
            .flat_map(|n| {
                n.binding_key.iter().map(String::as_str).chain(
                    n.metadata["binding_aliases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_str),
                )
            })
            .map(|key| {
                (
                    key.to_owned(),
                    format!(
                        "javascript:embedded:{}:{}:{group}:{key}",
                        host.path.len(),
                        host.path
                    ),
                )
            })
            .collect();
        for n in &mut js.nodes {
            n.binding_key = n.binding_key.as_ref().map(|key| keys[key].clone());
            if let Some(aliases) = n
                .metadata
                .get_mut("binding_aliases")
                .and_then(serde_json::Value::as_array_mut)
            {
                for alias in aliases {
                    if let Some(key) = alias.as_str().and_then(|key| keys.get(key)) {
                        *alias = json!(key);
                    }
                }
            }
        }
        for edge in &mut js.edges {
            if edge.source == root {
                edge.source = owner.clone();
            }
            if edge.target == root {
                edge.target = owner.clone();
            }
        }
        for reference in &mut js.references {
            if reference.source == root {
                reference.source = owner.clone();
            }
            for key in &mut reference.candidate_keys {
                if let Some(mapped) = keys.get(key) {
                    *key = mapped.clone();
                }
            }
        }
        host.nodes.extend(js.nodes);
        host.edges.extend(js.edges);
        host.references.extend(js.references);
    }
    Ok(())
}

#[derive(Debug)]
struct Attribute {
    name: String,
    value: String,
    range: Range<usize>,
    braced: bool,
}
#[derive(Debug)]
struct Tag {
    name: String,
    range: Range<usize>,
    attrs: Vec<Attribute>,
}

// Bounded lexical delimiters; quoted strings and ordinary comments cannot close a block.

/// A C# declaration inventory supplied by the project discovery layer.
#[derive(Debug, Clone)]
pub struct ProjectType {
    pub path: String,
    pub qualified_name: String,
    pub members: Vec<String>,
    pub event_handlers: Vec<String>,
    /// Generated nodes retain the attribute/declaration's real C# source range.
    pub generated: Vec<Node>,
}

/// The caller owns discovery, ignore rules, nested-project boundaries and cache invalidation.
pub struct TemplateProject<'a> {
    pub root: &'a str,
    pub namespace: Option<&'a str>,
    pub types: &'a [ProjectType],
}

/// Read only the supplied C# source through the existing grammar; never load assemblies.
pub fn project_types(path: &str, source: &str) -> Result<Vec<ProjectType>> {
    if !path.ends_with(".cs") || !relative_source(path) {
        return Ok(vec![]);
    }
    let mut facts =
        super::common::Extractor::new(path, source, "", "csharp", module_path(path)).facts;
    let Some(tree) = tree(tree_sitter_c_sharp::LANGUAGE.into(), source, &mut facts)? else {
        return Ok(vec![]);
    };
    let root = tree.root_node();
    let file_namespace = children(root)
        .into_iter()
        .find(|n| n.kind() == "file_scoped_namespace_declaration")
        .and_then(|n| n.child_by_field_name("name"))
        .map(|n| source[n.byte_range()].to_owned())
        .unwrap_or_default();
    let mut result = vec![];
    let mut pending = vec![(root, file_namespace)];
    while let Some((node, mut namespace)) = pending.pop() {
        if node.kind() == "namespace_declaration"
            && let Some(name) = node.child_by_field_name("name")
        {
            namespace = qualify(&namespace, &source[name.byte_range()]);
        }
        if node.kind() == "class_declaration" {
            let Some(name) = node.child_by_field_name("name") else {
                continue;
            };
            let qualified = qualify(&namespace, &source[name.byte_range()]);
            let mut ty = ProjectType {
                path: path.into(),
                qualified_name: qualified.clone(),
                members: vec![],
                event_handlers: vec![],
                generated: vec![],
            };
            let partial = children(node)
                .iter()
                .any(|n| n.kind() == "modifier" && &source[n.byte_range()] == "partial");
            if let Some(body) = node.child_by_field_name("body") {
                for member in children(body) {
                    if member.kind() == "class_declaration" {
                        pending.push((member, qualified.clone()));
                        continue;
                    }
                    let name = member
                        .child_by_field_name("name")
                        .map(|n| source[n.byte_range()].to_owned());
                    if let Some(name) = &name {
                        ty.members.push(name.clone());
                    }
                    if member.kind() == "method_declaration"
                        && event_signature(member, source)
                        && let Some(name) = &name
                    {
                        ty.event_handlers.push(name.clone());
                    }
                    if !partial {
                        continue;
                    }
                    for list in children(member)
                        .into_iter()
                        .filter(|n| n.kind() == "attribute_list")
                    {
                        for attr in children(list)
                            .into_iter()
                            .filter(|n| n.kind() == "attribute")
                        {
                            let Some(attr_name) = attr.child_by_field_name("name") else {
                                continue;
                            };
                            let full =
                                source[attr_name.byte_range()].trim_start_matches("global::");
                            let simple = full
                                .rsplit('.')
                                .next()
                                .unwrap_or(full)
                                .trim_end_matches("Attribute");
                            let required = match simple {
                                "ObservableProperty" => "CommunityToolkit.Mvvm.ComponentModel",
                                "RelayCommand" => "CommunityToolkit.Mvvm.Input",
                                _ => continue,
                            };
                            // An unrelated attribute with the same short name is not a generator.
                            if full.contains('.') {
                                if full.trim_end_matches("Attribute")
                                    != format!("{required}.{simple}")
                                {
                                    continue;
                                }
                            } else if !toolkit_using(root, member, source, required) {
                                continue;
                            }
                            let generated_names = if simple == "RelayCommand"
                                && member.kind() == "method_declaration"
                            {
                                name.as_ref()
                                    .map(|n| {
                                        vec![format!(
                                            "{}Command",
                                            n.strip_suffix("Async").unwrap_or(n)
                                        )]
                                    })
                                    .unwrap_or_default()
                            } else if simple == "ObservableProperty"
                                && member.kind() == "field_declaration"
                            {
                                children(member)
                                    .into_iter()
                                    .filter(|n| n.kind() == "variable_declaration")
                                    .flat_map(children)
                                    .filter(|n| n.kind() == "variable_declarator")
                                    .filter_map(|n| n.child_by_field_name("name"))
                                    .map(|n| {
                                        let raw = &source[n.byte_range()];
                                        pascal(
                                            raw.strip_prefix("m_")
                                                .unwrap_or(raw)
                                                .trim_start_matches('_'),
                                        )
                                    })
                                    .collect()
                            } else {
                                vec![]
                            };
                            for name in generated_names.into_iter().filter(|s| identifier(s)) {
                                ty.generated.push(Node {
                                    id: format!("xaml-generated:{path}:{qualified}.{name}@{}", attr.start_byte()), label: name.clone(),
                                    kind: if simple == "RelayCommand" { "command" } else { "property" }.into(),
                                    file: path.into(), line: Some(super::common::line(attr)), end_line: Some(super::common::end_line(member)),
                                    qualified_name: Some(format!("{qualified}.{name}")), binding_key: None,
                                    metadata: json!({"language":"csharp", "generated_by":format!("{required}.{simple}"), "qualified_symbol":format!("{qualified}.{name}"), "start_byte":attr.start_byte(), "end_byte":member.end_byte(), "start_column":attr.start_position().column, "inferred":true}),
                                });
                            }
                        }
                    }
                }
            }
            result.push(ty);
        } else if node.kind() != "file_scoped_namespace_declaration" {
            pending.extend(children(node).into_iter().map(|n| (n, namespace.clone())));
        }
    }
    Ok(result)
}

/// Apply only unique matches within the caller's project inventory to C#, XAML and Razor facts.
/// Calling twice is safe. No filesystem access or arbitrary source execution occurs here.
pub fn apply_project(facts: &mut FileFacts, project: &TemplateProject<'_>) {
    if !in_project(&facts.path, project.root) || facts.nodes.is_empty() {
        return;
    }
    let types: Vec<_> = project
        .types
        .iter()
        .filter(|t| in_project(&t.path, project.root))
        .collect();
    if facts.path.ends_with(".cs") {
        let source_prefix = format!("@{}.", facts.path);
        for ty in types.iter().filter(|t| t.path == facts.path) {
            let qualified = &ty.qualified_name;
            let Some(owner) = facts
                .nodes
                .iter()
                .find(|n| {
                    n.kind == "class"
                        && n.metadata["qualified_symbol"]
                            .as_str()
                            .is_some_and(|symbol| {
                                symbol.strip_prefix(&source_prefix).unwrap_or(symbol) == qualified
                            })
                })
                .map(|n| n.id.clone())
            else {
                continue;
            };
            for node in &mut facts.nodes {
                let Some(symbol) = node.metadata["qualified_symbol"].as_str() else {
                    continue;
                };
                let symbol = symbol.strip_prefix(&source_prefix).unwrap_or(symbol);
                let hidden_prefix = format!("@{}:", facts.path);
                let symbol = symbol.strip_prefix(&hidden_prefix).unwrap_or(symbol);
                if symbol == qualified
                    || symbol
                        .strip_prefix(qualified)
                        .is_some_and(|s| s.starts_with('.'))
                {
                    let key = project_key(
                        project,
                        if symbol == qualified {
                            "type"
                        } else {
                            "member"
                        },
                        symbol,
                    );
                    let mut aliases = node.metadata["binding_aliases"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    if !aliases.iter().any(|a| a.as_str() == Some(&key)) {
                        aliases.push(json!(key));
                    }
                    node.metadata["binding_aliases"] = json!(aliases);
                }
            }
            for generated in &ty.generated {
                if ty.members.contains(&generated.label)
                    || facts.nodes.iter().any(|n| n.id == generated.id)
                {
                    continue;
                }
                let mut node = generated.clone();
                node.binding_key = Some(project_key(
                    project,
                    "member",
                    node.qualified_name.as_deref().unwrap(),
                ));
                facts.edges.push(Edge {
                    id: format!("defines:{}", node.id),
                    source: owner.clone(),
                    target: node.id.clone(),
                    relation: "defines".into(),
                    directed: true,
                    file: Some(facts.path.clone()),
                    line: node.line,
                    confidence: "inferred".into(),
                    metadata: json!({"generator":node.metadata["generated_by"]}),
                });
                facts.nodes.push(node);
            }
        }
        return;
    }
    if facts.path.ends_with(".razor") || facts.path.ends_with(".cshtml") {
        let mut declarations = HashMap::<&str, usize>::new();
        for ty in &types {
            *declarations.entry(&ty.qualified_name).or_default() += 1;
        }
        let prefix = project_key(project, "type", "");
        for reference in &mut facts.references {
            if !matches!(reference.relation.as_str(), "uses_type" | "inherits") {
                continue;
            }
            // Candidates already reflect explicit using/alias/qualified syntax.
            // Never select a same-named type from an unrelated namespace or project.
            let mut matches: Vec<_> = reference
                .candidate_keys
                .iter()
                .filter_map(|key| {
                    key.strip_prefix("csharp:symbol:")
                        .or_else(|| key.strip_prefix(&prefix))
                })
                .filter(|name| declarations.contains_key(name))
                .collect();
            matches.sort_unstable();
            matches.dedup();
            reference.candidate_keys = match matches.as_slice() {
                [name] if declarations[name] == 1 => vec![project_key(project, "type", name)],
                _ => vec![],
            };
            reference.reason = "Razor type requires a unique declaration in its project".into();
        }
        return;
    }
    if !facts.path.ends_with(".xaml") {
        return;
    }
    let info = facts.nodes[0].metadata["xaml"].clone();
    let class = info["class"].as_str();
    let explicit = info["explicit_context"]
        .as_str()
        .and_then(|s| s.strip_prefix("csharp:symbol:"));
    let view_name = class.and_then(|s| s.rsplit('.').next()).or_else(|| {
        (info["prism_autowire"].as_bool() == Some(true)).then(|| {
            facts
                .path
                .rsplit('/')
                .next()
                .unwrap()
                .trim_end_matches(".xaml")
        })
    });
    let names = view_name.map(viewmodel_names).unwrap_or_default();
    let candidates: Vec<_> = types
        .iter()
        .copied()
        .filter(|ty| {
            if let Some(explicit) = explicit {
                return ty.qualified_name == explicit;
            }
            if info["has_data_context"].as_bool() == Some(true) {
                return false;
            }
            let name = ty.qualified_name.rsplit('.').next().unwrap_or("");
            if !names.iter().any(|n| n == name) {
                return false;
            }
            let namespace = class
                .and_then(|s| s.rsplit_once('.').map(|(ns, _)| ns))
                .map(|ns| ns.strip_suffix(".Views").unwrap_or(ns))
                .or(project.namespace);
            let namespace_match = namespace.is_some_and(|ns| {
                ty.qualified_name == format!("{ns}.ViewModels.{name}")
                    || ty.qualified_name == format!("{ns}.{name}")
            });
            let expected = if project.root.is_empty() {
                format!("ViewModels/{name}.cs")
            } else {
                format!("{}/ViewModels/{name}.cs", project.root)
            };
            // With no declared view namespace, Prism may use the exact sibling ViewModels path.
            namespace_match || (namespace.is_none() && ty.path == expected)
        })
        .collect();
    let viewmodel = if candidates.len() == 1 {
        Some(candidates[0])
    } else {
        None
    };
    let behind = class.and_then(|class| {
        let found: Vec<_> = types
            .iter()
            .copied()
            .filter(|ty| ty.path == format!("{}.cs", facts.path) && ty.qualified_name == class)
            .collect();
        (found.len() == 1).then(|| found[0])
    });
    for reference in &mut facts.references {
        match reference.relation.as_str() {
            "binds" | "binds_command" if identifier(&reference.label) => {
                reference.candidate_keys.clear();
                if info["nested_context"].as_bool() != Some(true)
                    && let Some(ty) = viewmodel
                {
                    let members = ty.members.iter().filter(|m| *m == &reference.label).count()
                        + ty.generated
                            .iter()
                            .filter(|n| n.label == reference.label)
                            .count();
                    if members == 1 {
                        reference.candidate_keys.push(project_key(
                            project,
                            "member",
                            &format!("{}.{}", ty.qualified_name, reference.label),
                        ));
                    }
                }
            }
            "binds_method" => {
                reference.candidate_keys.clear();
                if let Some(ty) = behind
                    && ty
                        .event_handlers
                        .iter()
                        .filter(|n| *n == &reference.label)
                        .count()
                        == 1
                {
                    reference.candidate_keys.push(project_key(
                        project,
                        "member",
                        &format!("{}.{}", ty.qualified_name, reference.label),
                    ));
                }
            }
            "code_behind" => {
                reference.candidate_keys = behind
                    .map(|ty| vec![project_key(project, "type", &ty.qualified_name)])
                    .unwrap_or_default();
            }
            "data_context" | "uses_type" => {
                for key in &mut reference.candidate_keys {
                    if let Some(name) = key.strip_prefix("csharp:symbol:") {
                        if types.iter().filter(|ty| ty.qualified_name == name).count() == 1 {
                            *key = project_key(project, "type", name);
                        } else {
                            key.clear();
                        }
                    }
                }
                reference.candidate_keys.retain(|k| !k.is_empty());
            }
            _ => {}
        }
    }
    let id = format!("xaml:view-model:{}", facts.path);
    facts.references.retain(|r| r.id != id);
    if let Some(ty) = viewmodel {
        facts.references.push(Reference {
            id,
            source: facts.nodes[0].id.clone(),
            label: ty.qualified_name.clone(),
            relation: "view_model".into(),
            file: facts.path.clone(),
            line: 1,
            candidate_keys: vec![project_key(project, "type", &ty.qualified_name)],
            reason: if explicit.is_some() {
                "explicit XAML DataContext"
            } else {
                "inferred from view namespace or conventional project path"
            }
            .into(),
        });
        facts.nodes[0].metadata["xaml"]["viewmodel_inferred"] = json!(explicit.is_none());
    }
}

const ROBOT_STANDARD_LIBRARIES: &[&str] = &[
    "BuiltIn",
    "Collections",
    "DateTime",
    "Dialogs",
    "Easter",
    "OperatingSystem",
    "Process",
    "Remote",
    "Reserved",
    "Screenshot",
    "String",
    "Telnet",
    "XML",
];

mod robotmodel_validate;
mod robotspan_range;
mod template_new;
mod template_xaml;

mod robot_dynamic;
use robot_dynamic::*;

mod robot_official;
