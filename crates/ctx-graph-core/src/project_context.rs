//! Repository configuration that gives context-free syntax facts a project identity.
use crate::model::{FileFacts, Node};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
};

#[derive(Default)]
pub(crate) struct ProjectContext {
    go: BTreeMap<(String, String), GoPackage>,
    javascript: JavascriptContext,
    rust: RustContext,
    templates: TemplateContext,
    swift: SwiftContext,
    terraform: crate::languages::configs::TerraformContext,
    cargo_packages: crate::languages::configs::CargoPackageContext,
    compiled: crate::languages::compiled::CompiledContext,
    compiled_source_hashes: BTreeMap<String, String>,
    extended: crate::languages::extended::ExtendedContext,
}

struct GoPackage {
    owner: String,
    import_path: Option<String>,
    fingerprint: String,
    production: bool,
    type_counts: BTreeMap<String, usize>,
}

/// File and exact-symbol aliases let document links reuse the indexed resolver.
pub(crate) fn add_document_aliases(facts: &mut FileFacts) {
    let file_root = facts
        .nodes
        .iter()
        .position(|n| matches!(n.kind.as_str(), "module" | "file" | "document"));
    for (index, node) in facts.nodes.iter_mut().enumerate() {
        let mut aliases = vec![];
        if file_root == Some(index) {
            aliases.push(format!("file:{}", facts.path));
        }
        if matches!(
            node.kind.as_str(),
            "function" | "class" | "method" | "interface" | "struct" | "enum" | "type"
        ) {
            aliases.push(format!("symbol:{}", node.label));
            if let Some(qualified) = &node.qualified_name {
                aliases.push(format!("symbol:{qualified}"));
                aliases.push(format!("symbol:{}::{qualified}", facts.path));
            }
        }
        if aliases.is_empty() {
            continue;
        }
        if node.metadata.is_null() {
            node.metadata = json!({});
        }
        if let Some(metadata) = node.metadata.as_object_mut() {
            if let Some(existing) = metadata.get("binding_aliases").and_then(|v| v.as_array()) {
                aliases.extend(
                    existing
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned)),
                );
            }
            aliases.sort();
            aliases.dedup();
            metadata.insert("binding_aliases".into(), json!(aliases));
        }
    }
}

fn javascript_source(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some(
            "js" | "jsx"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "mts"
                | "cts"
                | "vue"
                | "svelte"
                | "astro"
        )
    )
}
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}
fn within(path: &str, directory: &str) -> bool {
    directory.is_empty()
        || path
            .strip_prefix(directory)
            .is_some_and(|p| p.starts_with('/'))
}
fn join(base: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/')
        || relative.contains('\\')
        || relative.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    let mut parts: Vec<_> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}
fn ancestors(path: &str) -> Vec<String> {
    let mut result = vec![];
    let mut dir = directory(path);
    loop {
        result.push(dir.into());
        if dir.is_empty() {
            break;
        }
        dir = directory(dir);
    }
    result
}
fn stem(path: &str) -> &str {
    path.rsplit_once('.')
        .filter(|(stem, suffix)| !stem.is_empty() && !stem.ends_with('/') && !suffix.contains('/'))
        .map_or(path, |(stem, _)| stem)
}
fn digest<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let mut hash = blake3::Hasher::new();
    for item in items {
        hash.update(&(item.len() as u64).to_le_bytes());
        hash.update(item.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}
struct Inventory<'a> {
    root: &'a Path,
    files: BTreeSet<String>,
    configs: BTreeMap<String, Option<String>>,
}

#[derive(Default)]
struct SwiftContext {
    owners: BTreeMap<String, String>,
    imports: Vec<(String, String)>,
    target_imports: BTreeMap<String, Vec<(String, String)>>,
    source_hashes: BTreeMap<String, String>,
    fingerprint: String,
}

struct SwiftTarget {
    name: String,
    id: String,
    root: String,
    sources: Option<Vec<String>>,
    exclude: Vec<String>,
    dependencies: Vec<String>,
    test: bool,
}

// This is a closed literal subset of PackageDescription, not a Swift evaluator.
// Every membership-affecting argument is consumed; unknown syntax refuses the package.
fn swift_package(source: &str, package: &str) -> Option<Vec<SwiftTarget>> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    let items = swift_children(tree.root_node());
    let [import, declaration] = items.as_slice() else {
        return None;
    };
    if import.kind() != "import_declaration"
        || swift_tokens(*import, source)? != "importPackageDescription"
        || declaration.kind() != "property_declaration"
    {
        return None;
    }
    let children = swift_children(*declaration);
    let [binding, pattern, value] = children.as_slice() else {
        return None;
    };
    if binding.kind() != "value_binding_pattern"
        || swift_tokens(*binding, source)? != "let"
        || pattern.kind() != "pattern"
        || swift_tokens(*pattern, source)? != "package"
    {
        return None;
    }
    let (callee, args) = swift_call(*value, source)?;
    if callee != "Package" {
        return None;
    }
    let mut args = swift_labels(args)?;
    swift_string(args.remove("name")?, source)?;
    let targets = swift_array(args.remove("targets")?)?;
    for (key, value) in args {
        if !matches!(
            key.as_str(),
            "products"
                | "dependencies"
                | "platforms"
                | "defaultLocalization"
                | "swiftLanguageVersions"
                | "swiftLanguageModes"
                | "cLanguageStandard"
                | "cxxLanguageStandard"
        ) || !swift_literal(value, source, 0)
        {
            return None;
        }
    }
    let mut result = vec![];
    for target in targets {
        let (kind, args) = swift_call(target, source)?;
        if !matches!(
            kind.as_str(),
            ".target" | ".executableTarget" | ".testTarget"
        ) {
            return None;
        }
        let mut args = swift_labels(args)?;
        let name = swift_string(args.remove("name")?, source)?.to_owned();
        if !swift_identifier(&name) {
            return None;
        }
        let test = kind == ".testTarget";
        let root = match args.remove("path") {
            Some(value) => swift_path(package, swift_string(value, source)?)?,
            None => join(
                package,
                &format!("{}/{name}", if test { "Tests" } else { "Sources" }),
            )?,
        };
        let sources = match args.remove("sources") {
            Some(value) => Some(swift_paths(value, source, &root)?),
            None => None,
        };
        let exclude = match args.remove("exclude") {
            Some(value) => swift_paths(value, source, &root)?,
            None => vec![],
        };
        let mut dependencies = vec![];
        if let Some(value) = args.remove("dependencies") {
            for dependency in swift_array(value)? {
                if let Some(name) = swift_string(dependency, source) {
                    dependencies.push(name.to_owned());
                } else {
                    let (kind, args) = swift_call(dependency, source)?;
                    let mut args = swift_labels(args)?;
                    let name = swift_string(args.remove("name")?, source)?.to_owned();
                    match kind.as_str() {
                        ".target" | ".byName" => dependencies.push(name),
                        ".product" => {
                            swift_string(args.remove("package")?, source)?;
                        }
                        _ => return None,
                    }
                    // Conditions and module aliases are deliberately unsupported.
                    if !args.is_empty() {
                        return None;
                    }
                }
            }
        }
        if !args.is_empty() {
            return None;
        }
        // Duplicate names and overlapping target roots are ambiguous even if an
        // explicit source list happens to make today's indexed subset disjoint.
        if result.iter().any(|t: &SwiftTarget| {
            t.name == name || t.root == root || within(&root, &t.root) || within(&t.root, &root)
        }) {
            return None;
        }
        result.push(SwiftTarget {
            id: format!("swiftpm-{}", digest([package, name.as_str()])),
            name,
            root,
            sources,
            exclude,
            dependencies,
            test,
        });
    }
    Some(result)
}

fn swift_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
fn swift_path(base: &str, relative: &str) -> Option<String> {
    if relative.contains(['\\', ':'])
        || relative.starts_with('/')
        || relative.split('/').any(|p| p == "..")
    {
        return None;
    }
    join(base, relative)
}
fn swift_paths(node: tree_sitter::Node<'_>, source: &str, root: &str) -> Option<Vec<String>> {
    swift_array(node)?
        .into_iter()
        .map(|n| swift_path(root, swift_string(n, source)?))
        .collect()
}
fn swift_children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| !matches!(n.kind(), "comment" | "multiline_comment"))
        .collect()
}
fn swift_tokens(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    // Compare grammatical spellings without interpreting comments as source text.
    let mut pending = vec![node];
    let mut text = String::new();
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "comment" | "multiline_comment") {
            continue;
        }
        if node.child_count() == 0 {
            text.push_str(node.utf8_text(source.as_bytes()).ok()?);
        } else {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            pending.extend(children.into_iter().rev());
        }
    }
    Some(text)
}
fn swift_string<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    if node.kind() != "line_string_literal"
        || swift_children(node)
            .iter()
            .any(|n| n.kind() != "line_str_text")
    {
        return None;
    }
    node.utf8_text(source.as_bytes())
        .ok()?
        .strip_prefix('"')?
        .strip_suffix('"')
}
fn swift_array(node: tree_sitter::Node<'_>) -> Option<Vec<tree_sitter::Node<'_>>> {
    if node.kind() != "array_literal" {
        return None;
    }
    let mut cursor = node.walk();
    // Field iteration includes unnamed expressions such as nil, which must not disappear.
    let elements: Vec<_> = node
        .children_by_field_name("element", &mut cursor)
        .collect();
    swift_children(node)
        .iter()
        .all(|n| elements.contains(n))
        .then_some(elements)
}
type SwiftArguments<'a> = Vec<(Option<String>, tree_sitter::Node<'a>)>;
fn swift_call<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
) -> Option<(String, SwiftArguments<'a>)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let children = swift_children(node);
    let [callee, suffix] = children.as_slice() else {
        return None;
    };
    if suffix.kind() != "call_suffix" {
        return None;
    }
    let children = swift_children(*suffix);
    let [arguments] = children.as_slice() else {
        return None;
    };
    if arguments.kind() != "value_arguments" {
        return None;
    }
    let mut result = vec![];
    for argument in swift_children(*arguments) {
        if argument.kind() != "value_argument" {
            return None;
        }
        let value = argument.child_by_field_name("value")?;
        let name = match argument.child_by_field_name("name") {
            Some(node) => Some(swift_tokens(node, source)?),
            None => None,
        };
        if argument
            .child_by_field_name("reference_specifier")
            .is_some()
            || swift_children(argument)
                .iter()
                .any(|n| *n != value && Some(*n) != argument.child_by_field_name("name"))
        {
            return None;
        }
        result.push((name, value));
    }
    Some((swift_tokens(*callee, source)?, result))
}
fn swift_labels<'a>(args: SwiftArguments<'a>) -> Option<BTreeMap<String, tree_sitter::Node<'a>>> {
    let mut result = BTreeMap::new();
    for (name, value) in args {
        if result.insert(name?, value).is_some() {
            return None;
        }
    }
    Some(result)
}
fn swift_literal(node: tree_sitter::Node<'_>, source: &str, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match node.kind() {
        "line_string_literal" => swift_string(node, source).is_some(),
        "integer_literal" | "boolean_literal" | "nil" => true,
        "array_literal" => swift_array(node).is_some_and(|items| {
            items
                .into_iter()
                .all(|n| swift_literal(n, source, depth + 1))
        }),
        "prefix_expression" => swift_tokens(node, source)
            .and_then(|s| s.strip_prefix('.').map(swift_identifier))
            .unwrap_or(false),
        "call_expression" => swift_call(node, source).is_some_and(|(name, args)| {
            matches!(
                name.as_str(),
                ".library"
                    | ".executable"
                    | ".package"
                    | ".exact"
                    | ".upToNextMajor"
                    | ".upToNextMinor"
                    | ".branch"
                    | ".revision"
                    | ".macOS"
                    | ".iOS"
                    | ".tvOS"
                    | ".watchOS"
                    | ".visionOS"
                    | ".macCatalyst"
                    | ".driverKit"
            ) && args
                .into_iter()
                .all(|(_, n)| swift_literal(n, source, depth + 1))
        }),
        _ => false,
    }
}

#[derive(Default)]
struct TemplateContext {
    // None is a boundary with ambiguous or unsupported project configuration.
    projects: BTreeMap<String, Option<CsharpProject>>,
    loose: Vec<crate::languages::templates::ProjectType>,
    fingerprint: String,
    scope_fingerprint: String,
    source_hashes: BTreeMap<String, String>,
}
struct CsharpProject {
    manifest: String,
    namespace: Option<String>,
    types: Vec<crate::languages::templates::ProjectType>,
}

// Only unconditional literal RootNamespace is a namespace hint. Neither project
// imports, conditions nor MSBuild expressions are evaluated.
fn csharp_namespace(source: &str) -> Option<Option<String>> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_str(source);
    let mut stack: Vec<(String, bool)> = vec![];
    let mut roots = 0;
    let mut namespaces = vec![];
    let mut invalid_namespace = false;
    loop {
        match reader.read_event().ok()? {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(&event, Event::Empty(_));
                let element = match &event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                if stack.len() >= 128 {
                    return None;
                }
                let name = String::from_utf8(element.local_name().as_ref().to_vec()).ok()?;
                if stack.is_empty() {
                    roots += 1;
                    if roots != 1 || name != "Project" {
                        return None;
                    }
                }
                let mut conditional = stack.last().is_some_and(|(_, conditional)| *conditional);
                for attribute in element.attributes() {
                    if attribute.ok()?.key.local_name().as_ref() == b"Condition" {
                        conditional = true;
                    }
                }
                if name == "RootNamespace" {
                    let text = if empty {
                        String::new()
                    } else {
                        reader
                            .read_text(element.name())
                            .ok()?
                            .decode()
                            .ok()?
                            .into_owned()
                    };
                    let value = text.trim();
                    let literal = !value.is_empty()
                        && value.split('.').all(|part| {
                            let mut chars = part.chars();
                            chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
                                && chars.all(|c| c == '_' || c.is_alphanumeric())
                        });
                    if conditional || !literal || stack.len() != 2 || stack[1].0 != "PropertyGroup"
                    {
                        invalid_namespace = true;
                    } else {
                        namespaces.push(value.to_owned());
                    }
                } else if !empty {
                    stack.push((name, conditional));
                }
            }
            Event::End(_) => {
                stack.pop()?;
            }
            Event::DocType(_) => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    if roots != 1 || !stack.is_empty() || invalid_namespace || namespaces.len() > 1 {
        return None;
    }
    Some(namespaces.pop())
}

#[derive(Clone, Default)]
struct TypescriptConfig {
    base_url: Option<String>,
    invalid_base_url: bool,
    paths: BTreeMap<String, Vec<String>>,
    paths_origin: String,
}
fn typescript_config(
    path: &str,
    inventory: &mut Inventory<'_>,
    stack: &mut Vec<String>,
    observations: &mut BTreeMap<String, Option<String>>,
) -> Result<TypescriptConfig> {
    ensure!(
        stack.len() < 32 && !stack.iter().any(|p| p == path),
        "cyclic or excessively nested TypeScript extends"
    );
    let Some(source) = javascript_config(inventory, path, observations)? else {
        return Ok(TypescriptConfig::default());
    };
    // Empty and comment-only tsconfig/jsconfig files are common fixtures and
    // carry no mapping information. Treat them like `{}` instead of aborting
    // an otherwise valid repository-wide index.
    let value = jsonc_parser::parse_to_serde_value(&source, &Default::default())?
        .unwrap_or_else(|| serde_json::json!({}));
    ensure!(
        value.is_object(),
        "TypeScript configuration must be an object"
    );
    stack.push(path.into());
    let mut result = TypescriptConfig::default();
    let bases: Vec<_> = value
        .get("extends")
        .into_iter()
        .flat_map(|v| {
            if let Some(s) = v.as_str() {
                vec![s]
            } else {
                v.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect()
            }
        })
        .collect();
    for base in bases {
        // Package-name extends needs installed dependency resolution; never probe node_modules.
        if !base.starts_with('.') {
            continue;
        }
        let Some(mut target) = join(directory(path), base) else {
            continue;
        };
        if !target.ends_with(".json") {
            target.push_str(".json");
        }
        let inherited = typescript_config(&target, inventory, stack, observations)?;
        if inherited.base_url.is_some() || inherited.invalid_base_url {
            result.base_url = inherited.base_url;
            result.invalid_base_url = inherited.invalid_base_url;
        }
        if !inherited.paths.is_empty() {
            result.paths = inherited.paths;
            result.paths_origin = inherited.paths_origin;
        }
    }
    if let Some(options) = value.get("compilerOptions") {
        if let Some(base) = options.get("baseUrl").and_then(Value::as_str) {
            result.base_url = join(directory(path), base);
            result.invalid_base_url = result.base_url.is_none();
        }
        if let Some(paths) = options.get("paths").and_then(Value::as_object) {
            result.paths.clear();
            result.paths_origin = directory(path).into();
            for (pattern, values) in paths {
                if pattern.matches('*').count() > 1 {
                    continue;
                }
                let targets = values
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|v| v.matches('*').count() <= 1)
                    .map(str::to_owned)
                    .collect();
                result.paths.insert(pattern.clone(), targets);
            }
        }
    }
    stack.pop();
    Ok(result)
}
fn javascript_config(
    inventory: &mut Inventory<'_>,
    path: &str,
    observations: &mut BTreeMap<String, Option<String>>,
) -> Result<Option<String>> {
    let source = inventory.config(path)?;
    observations.insert(
        path.into(),
        source
            .as_ref()
            .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string()),
    );
    Ok(source)
}
fn capture<'a>(pattern: &str, value: &'a str) -> Option<&'a str> {
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        value.strip_prefix(prefix)?.strip_suffix(suffix)
    } else {
        (pattern == value).then_some("")
    }
}
fn workspace_member(patterns: &[String], directory: &str) -> bool {
    patterns.iter().any(|pattern| {
        globset::Glob::new(pattern)
            .ok()
            .is_some_and(|g| g.compile_matcher().is_match(directory))
    })
}
#[derive(Default)]
struct JavascriptContext {
    files: BTreeSet<String>,
    configs: BTreeMap<String, TypescriptConfig>,
    packages: BTreeMap<String, Value>,
    workspaces: BTreeMap<String, Vec<String>>,
    raw_facts: BTreeMap<String, FileFacts>,
    source_hashes: BTreeMap<String, String>,
    config_hashes: BTreeMap<String, Option<String>>,
    fingerprints: BTreeMap<String, String>,
    // Some(key): one proven declaration; None: type-only or conflicting route.
    // Absent entries retain ordinary function/class resolution.
    imported_callees: BTreeMap<String, Option<String>>,
    // Exact immutable value declaration -> exact returned callable body key.
    factory_results: BTreeMap<String, String>,
    star_aliases: BTreeMap<String, Vec<String>>,
    esm_files: BTreeSet<String>,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum JavascriptCallee {
    Declaration { node: String, key: String },
    Ordinary(String),
    Unresolved,
}

fn export_target(exports: &Value, subpath: &str, require: bool) -> Option<String> {
    let key = if subpath.is_empty() {
        ".".into()
    } else {
        format!("./{subpath}")
    };
    let (target, capture) = if let Some(map) = exports
        .as_object()
        .filter(|m| m.keys().any(|k| k.starts_with('.')))
    {
        let (pattern, target, capture) = map
            .iter()
            .filter_map(|(p, v)| capture(p, &key).map(|c| (p, v, c)))
            .max_by_key(|(p, _, _)| {
                (
                    usize::from(!p.contains('*')),
                    p.split('*').next().unwrap_or("").len(),
                )
            })?;
        if pattern.matches('*').count() > 1 {
            return None;
        }
        (target, capture)
    } else if subpath.is_empty() {
        (exports, "")
    } else {
        return None;
    };
    fn condition(value: &Value, require: bool) -> Option<&str> {
        if let Some(target) = value.as_str() {
            return Some(target);
        }
        let map = value.as_object()?;
        if map
            .keys()
            .any(|k| !matches!(k.as_str(), "import" | "default" | "require" | "types"))
        {
            return None;
        }
        let import = map
            .get(if require { "require" } else { "import" })
            .and_then(|v| condition(v, require));
        let default = map.get("default").and_then(|v| condition(v, require));
        match (import, default) {
            (Some(a), Some(b)) if a != b => None,
            (Some(a), _) | (_, Some(a)) => Some(a),
            _ => None,
        }
    }
    let target = condition(target, require)?;
    target
        .starts_with("./")
        .then(|| target.replace('*', capture))
}

#[derive(Default)]
struct RustContext {
    crates: BTreeMap<String, RustCrate>,
    owners: BTreeMap<String, String>,
    modules: BTreeMap<String, Vec<RustModule>>,
    forwarding: BTreeMap<String, Vec<String>>,
    generic_owners: BTreeMap<(String, String), u64>,
    fingerprint: String,
    fingerprints: BTreeMap<String, String>,
    source_hashes: BTreeMap<String, String>,
    facts: BTreeMap<String, FileFacts>,
}
struct RustCrate {
    name: String,
    library: String,
    root: Option<String>,
    dependencies: BTreeMap<String, String>,
    modules: BTreeSet<String>,
    public_modules: BTreeSet<String>,
    unavailable_modules: BTreeSet<String>,
}
#[derive(Debug)]
struct RustModule {
    package: String,
    module: String,
    public: bool,
}
fn toml_strings(item: Option<&toml_edit::Item>) -> Vec<String> {
    item.and_then(toml_edit::Item::as_array)
        .into_iter()
        .flat_map(|a| a.iter())
        .filter_map(toml_edit::Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn outcome_fingerprint(discriminator: &str, mut facts: FileFacts) -> Result<String> {
    facts.hash.clear();
    facts.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    facts.edges.sort_by(|a, b| a.id.cmp(&b.id));
    facts.references.sort_by(|a, b| a.id.cmp(&b.id));
    facts
        .diagnostics
        .sort_by(|a, b| (&a.file, a.line, &a.message).cmp(&(&b.file, b.line, &b.message)));
    Ok(format!(
        "{discriminator}-{}",
        blake3::hash(&serde_json::to_vec(&facts)?).to_hex()
    ))
}
fn merge_aliases(node: &mut Node, mut aliases: Vec<String>) {
    if aliases.is_empty() {
        return;
    }
    if !node.metadata.is_object() {
        node.metadata = json!({});
    }
    aliases.extend(
        node.metadata
            .get("binding_aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    aliases.sort();
    aliases.dedup();
    node.metadata["binding_aliases"] = json!(aliases);
}

#[cfg(test)]
mod source_snapshot_tests;

mod inventory_new;
mod javascriptcontext_apply_factory_returns;
mod javascriptcontext_discover;
mod projectcontext_discover_with_swift_modules;
mod rustcontext_discover;
mod swiftcontext_discover;
mod swifttarget_contains;
mod templatecontext_discover;
