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
pub fn parse_robot_official(
    path: &str,
    source: &str,
    hash: &str,
    python: &Path,
) -> Result<FileFacts> {
    ensure!(
        relative_source(path) && (path.ends_with(".robot") || path.ends_with(".resource")),
        "official Robot parsing requires a normalized relative .robot or .resource path"
    );
    let mut out = Template::new(path, source, hash, "robot");
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut out.f, None, "Source exceeds the 4 MiB indexing limit");
        return Ok(out.f);
    }
    // Do not canonicalize: venv executables may be symlinks to system Python.
    let executable = if python.is_relative() && python.components().count() > 1 {
        std::env::current_dir()?.join(python)
    } else {
        python.to_owned()
    };
    let adapter = crate::ingest::CommandAdapter {
        program: executable
            .to_str()
            .context("Robot Python path must be UTF-8")?
            .into(),
        args: vec![
            "-I".into(),
            "-B".into(),
            "-c".into(),
            include_str!("../assets/robot_parser.py").into(),
        ],
        output_file: false,
    };
    let request =
        serde_json::to_vec(&json!({"schema_version": 1, "path": path, "source": source}))?;
    let raw = crate::ingest::run_command(&adapter, None, Some(&request), 10, 8 * 1024 * 1024)
        .context("explicit Robot parser failed")?;
    let model: RobotModel = serde_json::from_str(&raw).context("invalid Robot parser response")?;
    model.validate(source)?;
    if let Some(failure) = &model.failure {
        bail!(
            "{}",
            match failure.as_str() {
                "missing_dependency" => "selected Python requires installed Robot Framework 7.5.x",
                "unsupported_version" => "official Robot adapter supports Robot Framework 7.5.x",
                "limit" => "official Robot parser exceeded its static extraction limit",
                _ => "official Robot parser could not produce a valid static model",
            }
        );
    }
    if !model.diagnostics.is_empty() {
        for issue in &model.diagnostics {
            out.diagnostic_at(issue.span.start, match issue.code.as_str() {
                "embedded_syntax" => "Robot Framework reported invalid embedded keyword syntax",
                _ => "Robot Framework reported invalid syntax or an unsupported language declaration",
            });
        }
        return Ok(out.f);
    }
    let root = out.node(
        path.rsplit('/').next().unwrap(),
        "module",
        0..source.len(),
        Some(format!("template:file:{path}")),
        None,
    );
    out.f.nodes[0].metadata["parser"] = json!("robotframework");
    out.f.nodes[0].metadata["parser_version"] = json!(model.robot_version);
    out.f.nodes[0].metadata["coverage"] = json!("static-model");
    out.f.nodes[0].metadata["languages"] = json!(model.languages);
    let mut definitions = Vec::with_capacity(model.definitions.len());
    for definition in &model.definitions {
        let key = (definition.kind == RobotDefinitionKind::Keyword)
            .then(|| format!("robot:keyword:{path}:{}", robot_normalize(&definition.name)));
        let id = out.node(
            &definition.name,
            if key.is_some() { "keyword" } else { "test" },
            definition.span.range(),
            key.clone(),
            Some(&root),
        );
        if definition.embedded {
            out.f.nodes.last_mut().unwrap().metadata["embedded_arguments"] = json!(true);
        }
        definitions.push((id, key));
    }
    let mut resources = Vec::new();
    let mut unknown_resource = false;
    let mut namespaces: HashMap<String, Option<String>> = HashMap::new();
    for import in &model.imports {
        let named_library = import.kind == RobotImportKind::Library
            && !import.name.contains(['/', '\\', '$', '@', '&', '%'])
            && !import.name.ends_with(".py");
        let imported = (!named_library && !robot_dynamic_import(&import.name))
            .then(|| robot_import(path, &import.name))
            .flatten();
        if import.kind == RobotImportKind::Resource {
            if let Some(file) = &imported {
                resources.push(file.clone());
                let stem = Path::new(file)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                namespaces
                    .entry(robot_normalize(stem))
                    .and_modify(|value| {
                        if value.as_ref() != Some(file) {
                            *value = None;
                        }
                    })
                    .or_insert_with(|| Some(file.clone()));
            } else {
                unknown_resource = true;
            }
        } else if import.kind == RobotImportKind::Library {
            let name = import.alias.as_deref().unwrap_or(&import.name);
            let stem = name
                .rsplit('/')
                .next()
                .unwrap_or(name)
                .trim_end_matches(".py");
            namespaces.insert(robot_normalize(stem), None);
        }
        if named_library {
            if ROBOT_STANDARD_LIBRARIES.contains(&import.name.as_str()) {
                continue;
            }
            let key = format!("robot:library:{path}:{}", import.name);
            out.node(
                &import.name,
                "library",
                import.span.range(),
                Some(key.clone()),
                Some(&root),
            );
            let node = out.f.nodes.last_mut().unwrap();
            node.metadata["external"] = json!(true);
            node.metadata["alias"] = json!(import.alias);
            out.reference(&root, &import.name, "imports", import.span.start, vec![key]);
        } else {
            out.reference(
                &root,
                &import.name,
                "imports",
                import.span.start,
                imported
                    .as_deref()
                    .map(robot_file_key)
                    .into_iter()
                    .collect(),
            );
        }
    }
    resources.sort();
    resources.dedup();
    for call in &model.calls {
        let owner = call
            .owner
            .map(|id| definitions[id].0.as_str())
            .unwrap_or(&root);
        let keys = if let Some(target) = call.target {
            definitions[target].1.clone().into_iter().collect()
        } else if call.ambiguous || robot_dynamic(&call.name) {
            vec![]
        } else {
            let mut keys = Vec::new();
            for name in &call.alternatives {
                let (file, keyword) = if let Some((prefix, keyword)) = name.rsplit_once('.') {
                    (
                        namespaces
                            .get(&robot_normalize(prefix))
                            .and_then(Option::as_ref),
                        keyword,
                    )
                } else {
                    (
                        if resources.len() == 1 && !unknown_resource {
                            resources.first()
                        } else {
                            None
                        },
                        name.as_str(),
                    )
                };
                if let Some(file) = file {
                    let key = format!("robot:keyword:{file}:{}", robot_normalize(keyword));
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
            keys
        };
        out.reference(owner, &call.name, "calls", call.span.start, keys);
    }
    Ok(out.f)
}

fn robot_dynamic(name: &str) -> bool {
    ["${", "@{", "&{", "%{"]
        .iter()
        .any(|marker| name.contains(marker))
}

fn robot_dynamic_import(name: &str) -> bool {
    // robot_import handles the three allowed ${...} path anchors itself.
    ["@{", "&{", "%{"]
        .iter()
        .any(|marker| name.contains(marker))
}

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
impl RobotSpan {
    fn range(&self) -> Range<usize> {
        self.start..self.end
    }
    fn valid(&self, source: &str) -> bool {
        self.start <= self.end
            && self.end <= source.len()
            && source.is_char_boundary(self.start)
            && source.is_char_boundary(self.end)
    }
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

impl RobotModel {
    fn validate(&self, source: &str) -> Result<()> {
        let text = |s: &str| !s.is_empty() && s.len() <= 4096 && !s.contains('\0');
        ensure!(
            self.schema_version == 1
                && self.robot_version.len() <= 64
                && self.failure.as_ref().is_none_or(|s| text(s))
                && self.languages.len() <= 64
                && self.languages.iter().all(|s| text(s) && s.len() <= 64)
                && self.definitions.len()
                    + self.imports.len()
                    + self.calls.len()
                    + self.diagnostics.len()
                    <= 20_000,
            "invalid Robot parser protocol or extraction limit"
        );
        ensure!(
            self.failure.is_some()
                || self.robot_version == "7.5"
                || self
                    .robot_version
                    .strip_prefix("7.5.")
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())),
            "unsupported Robot parser protocol version"
        );
        for (id, definition) in self.definitions.iter().enumerate() {
            ensure!(
                definition.id == id && text(&definition.name) && definition.span.valid(source),
                "invalid Robot definition or source span"
            );
        }
        for import in &self.imports {
            ensure!(
                text(&import.name)
                    && import.alias.as_ref().is_none_or(|s| text(s))
                    && import.span.valid(source),
                "invalid Robot import or source span"
            );
        }
        for call in &self.calls {
            ensure!(
                text(&call.name)
                    && call.span.valid(source)
                    && (1..=2).contains(&call.alternatives.len())
                    && call.alternatives[0] == call.name
                    && call.alternatives.iter().all(|s| text(s))
                    && call.owner.is_none_or(|id| id < self.definitions.len())
                    && call.target.is_none_or(|id| !call.ambiguous
                        && self
                            .definitions
                            .get(id)
                            .is_some_and(|d| d.kind == RobotDefinitionKind::Keyword)),
                "invalid Robot call, owner or target"
            );
        }
        for issue in &self.diagnostics {
            ensure!(
                matches!(issue.code.as_str(), "syntax" | "embedded_syntax")
                    && issue.span.valid(source),
                "invalid Robot diagnostic or source span"
            );
        }
        Ok(())
    }
}

struct Template<'a> {
    f: FileFacts,
    source: &'a str,
    language: &'a str,
    lines: Vec<usize>,
}
impl<'a> Template<'a> {
    fn new(path: &str, source: &'a str, hash: &str, language: &'a str) -> Self {
        Self {
            f: FileFacts {
                path: path.into(),
                hash: hash.into(),
                module: module_path(path),
                nodes: vec![],
                edges: vec![],
                references: vec![],
                diagnostics: vec![],
            },
            source,
            language,
            lines: std::iter::once(0)
                .chain(source.match_indices('\n').map(|(p, _)| p + 1))
                .collect(),
        }
    }
    fn line(&self, byte: usize) -> u32 {
        self.lines.partition_point(|p| *p <= byte) as u32
    }
    fn node(
        &mut self,
        label: &str,
        kind: &str,
        range: Range<usize>,
        key: Option<String>,
        owner: Option<&str>,
    ) -> String {
        let id = format!("template:{}:{kind}@{}:{label}", self.f.path, range.start);
        self.f.nodes.push(Node { id: id.clone(), label: label.into(), kind: kind.into(), file: self.f.path.clone(), line: Some(self.line(range.start)), end_line: Some(self.line(range.end.saturating_sub(1).max(range.start))), qualified_name: Some(label.into()), binding_key: key,
            metadata: json!({"language": self.language, "start_byte": range.start, "end_byte": range.end, "start_column": range.start - self.lines[self.line(range.start) as usize - 1]}) });
        if let Some(owner) = owner {
            self.f.edges.push(Edge {
                id: format!("contains:{id}"),
                source: owner.into(),
                target: id.clone(),
                relation: "contains".into(),
                directed: true,
                file: Some(self.f.path.clone()),
                line: Some(self.line(range.start)),
                confidence: "static".into(),
                metadata: serde_json::Value::Null,
            });
        }
        id
    }
    fn diagnostic_at(&mut self, byte: usize, message: &str) {
        let line = self.line(byte);
        diagnostic(&mut self.f, Some(line), message);
    }
    fn root(&self) -> String {
        self.f.nodes[0].id.clone()
    }
    fn reference(
        &mut self,
        owner: &str,
        label: &str,
        relation: &str,
        byte: usize,
        keys: Vec<String>,
    ) {
        self.f.references.push(Reference {
            id: format!("{relation}:{owner}@{byte}:{}", self.f.references.len()),
            source: owner.into(),
            label: label.into(),
            relation: relation.into(),
            file: self.f.path.clone(),
            line: self.line(byte),
            candidate_keys: keys,
            reason: "static target is unavailable, external, dynamic, or ambiguous".into(),
        });
    }
    fn markup(&mut self, lang: &str) -> Result<()> {
        let mut visible = self.source.as_bytes().to_vec();
        if lang == "blade" {
            mask_comments(&mut visible, self.source, "{{--", "--}}");
        }
        if matches!(lang, "razor" | "cshtml") {
            mask_comments(&mut visible, self.source, "@*", "*@");
        }
        let mut scripts: Vec<(Range<usize>, String)> = vec![];
        if lang == "astro"
            && self
                .source
                .trim_start_matches('\u{feff}')
                .starts_with("---")
        {
            let start = self
                .source
                .find('\n')
                .map(|p| p + 1)
                .unwrap_or(self.source.len());
            let mut pos = start;
            let mut end = None;
            for line in self.source[start..].split_inclusive('\n') {
                if line.trim() == "---" {
                    end = Some((pos, pos + line.len()));
                    break;
                }
                pos += line.len();
            }
            if let Some((end, after)) = end {
                scripts.push((start..end, "ts".into()));
                blank(&mut visible, 0..after);
            } else {
                diagnostic(&mut self.f, Some(1), "Unclosed Astro frontmatter");
                blank(&mut visible, 0..self.source.len());
            }
        }
        // Scan opening tags without treating quoted '>' or brace expressions as tag boundaries.
        let mut tags = vec![];
        let markup_text = String::from_utf8(visible.clone())?;
        let mut pos = 0;
        while pos < visible.len() {
            if visible[pos..].starts_with(b"<!--") {
                let end = markup_text[pos + 4..]
                    .find("-->")
                    .map(|n| pos + 4 + n + 3)
                    .unwrap_or(visible.len());
                blank(&mut visible, pos..end);
                pos = end;
                continue;
            }
            if visible[pos] != b'<' {
                pos += 1;
                continue;
            }
            let text = &markup_text;
            let Some(tag) = tag_at(text, pos) else {
                pos += 1;
                continue;
            };
            pos = tag.range.end;
            if tag.name.eq_ignore_ascii_case("script") || tag.name.eq_ignore_ascii_case("style") {
                let close = format!("</{}", tag.name.to_ascii_lowercase());
                let closing = find_close_tag(text, pos, &close);
                if let Some((end, after)) = closing {
                    if tag.name.eq_ignore_ascii_case("script") {
                        if let Some(src) = tag.attrs.iter().find(|a| a.name == "src") {
                            self.reference(
                                &self.root(),
                                &src.value,
                                "imports",
                                src.range.start,
                                js_modules(&self.f.path, &src.value),
                            );
                        }
                        let ty = tag
                            .attrs
                            .iter()
                            .find(|a| a.name == "lang")
                            .map(|a| a.value.clone())
                            .unwrap_or("js".into());
                        let script_type = tag
                            .attrs
                            .iter()
                            .find(|a| a.name == "type")
                            .map(|a| a.value.as_str());
                        if script_type.is_none_or(|s| {
                            matches!(s, "module" | "text/javascript" | "application/javascript")
                        }) && matches!(
                            ty.as_str(),
                            "js" | "jsx" | "ts" | "tsx" | "javascript" | "typescript"
                        ) {
                            scripts.push((pos..end, ty));
                        }
                    }
                    blank(&mut visible, tag.range.start..after);
                    pos = after;
                } else {
                    self.diagnostic_at(tag.range.start, "Unclosed script/style block");
                    blank(&mut visible, tag.range.start..self.source.len());
                    break;
                }
            } else {
                tags.push(tag);
            }
        }
        let mut bindings: HashMap<String, Vec<String>> = HashMap::new();
        if matches!(lang, "vue" | "svelte" | "astro") {
            self.f.nodes[0].binding_key = Some(format!("javascript:module:{}", self.f.path));
            let name = module_path(&self.f.path)
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned();
            self.node(
                &name,
                "component",
                0..self.source.len(),
                Some(format!("javascript:{}:default", self.f.path)),
                Some(&self.root()),
            );
            let extension = if scripts
                .iter()
                .any(|(_, l)| matches!(l.as_str(), "jsx" | "tsx"))
            {
                "tsx"
            } else if scripts
                .iter()
                .any(|(_, lang)| matches!(lang.as_str(), "ts" | "typescript"))
            {
                "ts"
            } else {
                "js"
            };
            let masked = mask_ranges(
                self.source,
                &scripts.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>(),
            );
            let virtual_path = format!("{}.{}", self.f.path, extension);
            let mut facts = super::javascript::parse(&virtual_path, &masked, &self.f.hash)?;
            self.remap_js(&mut facts, &virtual_path);
            for r in &facts.references {
                if r.relation == "imports"
                    && let Some((_, local)) = r.label.split_once(" as ")
                {
                    bindings
                        .entry(local.into())
                        .or_default()
                        .extend(r.candidate_keys.clone());
                }
            }
            for n in &facts.nodes {
                if n.label == "default" {
                    continue;
                }
                if n.qualified_name.as_deref() == Some(n.label.as_str())
                    && let Some(key) = &n.binding_key
                {
                    bindings
                        .entry(n.label.clone())
                        .or_default()
                        .push(key.clone());
                }
            }
            // Ask the existing lexical resolver about template names, including
            // reassigned imports and namespace component selectors. Probe facts
            // are never published: their source coordinates are synthetic.
            for tag in &tags {
                if tag.name.chars().next().is_some_and(char::is_uppercase)
                    && tag.name.split('.').all(identifier)
                {
                    bindings.entry(tag.name.clone()).or_default();
                }
            }
            let mut probe = masked.clone();
            probe.push_str("\n;\n");
            for name in bindings.keys().filter(|s| s.split('.').all(identifier)) {
                probe.push_str(name);
                probe.push_str("();\n");
            }
            let mut resolved = super::javascript::parse(&virtual_path, &probe, &self.f.hash)?;
            self.remap_js(&mut resolved, &virtual_path);
            for keys in bindings.values_mut() {
                keys.clear();
            }
            for r in resolved.references {
                if r.relation == "calls" && r.line > self.line(self.source.len()) {
                    bindings.insert(r.label, r.candidate_keys);
                }
            }
            // The component owns the implicit default export, not a second alias.
            for n in &mut facts.nodes {
                if n.binding_key.as_deref() == Some(&format!("javascript:{}:default", self.f.path))
                {
                    n.binding_key = None;
                }
            }
            self.append(facts);
        }
        if matches!(lang, "razor" | "cshtml") {
            self.razor(&mut visible, &mut bindings)?;
        }
        if lang == "blade" {
            self.blade(&visible);
        }
        for tag in tags {
            if tag.range.clone().any(|p| visible[p] == b'<') {
                let component = tag.name.starts_with("livewire:")
                    || tag.name.starts_with("x-")
                    || tag.name.chars().next().is_some_and(char::is_uppercase)
                    || (matches!(lang, "vue" | "svelte" | "astro") && tag.name.contains('-'));
                if component {
                    let key_name = if lang == "vue" {
                        pascal(&tag.name)
                    } else {
                        tag.name.clone()
                    };
                    let keys = bindings
                        .get(&tag.name)
                        .or_else(|| bindings.get(&key_name))
                        .cloned()
                        .unwrap_or_default();
                    self.reference(
                        &self.root(),
                        &tag.name,
                        "uses_component",
                        tag.range.start,
                        keys,
                    );
                }
                for attr in tag.attrs {
                    if !attr.braced && lang != "svelte" {
                        blank(&mut visible, attr.range.clone());
                    }
                    let event = attr.name.starts_with('@')
                        || attr.name.starts_with("v-on:")
                        || attr.name.starts_with("on:")
                        || attr.name.starts_with("wire:")
                        || (attr.name.starts_with("on") && attr.braced);
                    if event {
                        let value = attr.value.trim().trim_start_matches('@').trim();
                        let name = if lang == "svelte" {
                            value
                                .strip_prefix('{')
                                .and_then(|v| v.strip_suffix('}'))
                                .unwrap_or(value)
                                .trim()
                        } else {
                            value
                        };
                        let bare = name.split('(').next().unwrap_or(name).trim();
                        if identifier(bare) {
                            self.reference(
                                &self.root(),
                                bare,
                                "binds_method",
                                attr.range.start,
                                bindings.get(bare).cloned().unwrap_or_default(),
                            );
                        } else {
                            self.reference(
                                &self.root(),
                                name,
                                "binds_method",
                                attr.range.start,
                                vec![],
                            );
                        }
                    }
                }
            }
        }
        if matches!(lang, "vue" | "svelte" | "astro") {
            // Parse only brace-delimited template expressions, never literal text or comments.
            let text = std::str::from_utf8(&visible).expect("masked UTF-8");
            let mut pos = 0;
            let mut expressions = 0;
            while let Some(p) = text[pos..].find('{') {
                expressions += 1;
                if expressions > 128 {
                    self.diagnostic_at(
                        pos,
                        "Template expression limit reached; remaining expressions omitted",
                    );
                    break;
                }
                let start = pos + p;
                if lang == "vue" && !text[start..].starts_with("{{") {
                    pos = start + 1;
                    continue;
                }
                let Some(end) = balanced(text, start, b'{', b'}') else {
                    break;
                };
                let mut body = start + 1;
                if text[body..end - 1].starts_with('{') {
                    body += 1;
                }
                for prefix in ["#await ", "#if ", ":else if ", "@html ", "@const "] {
                    if text[body..end - 1].starts_with(prefix) {
                        body += prefix.len();
                        break;
                    }
                }
                let tail = if text[start..end].starts_with("{{") {
                    end.saturating_sub(2)
                } else {
                    end - 1
                };
                if body < tail && !text[body..tail].starts_with(['#', '/', ':']) {
                    let expr = mask_ranges(self.source, std::slice::from_ref(&(body..tail)));
                    let virtual_path = format!("{}.ts", self.f.path);
                    let mut facts = super::javascript::parse(&virtual_path, &expr, &self.f.hash)?;
                    self.remap_js(&mut facts, &virtual_path);
                    // Expressions are not declarations; publish only recovered imports/calls.
                    for mut r in facts.references {
                        r.source = self.root();
                        if r.candidate_keys.is_empty() {
                            r.candidate_keys = bindings.get(&r.label).cloned().unwrap_or_default();
                        }
                        r.id = format!("template-expression:{}:{}", start, r.id);
                        self.f.references.push(r);
                    }
                }
                pos = end;
            }
        }
        Ok(())
    }
    fn remap_js(&self, facts: &mut FileFacts, virtual_path: &str) {
        let old_root = facts.nodes.first().map(|n| n.id.clone());
        let remap = |s: &str| s.replace(virtual_path, &self.f.path);
        for n in &mut facts.nodes {
            n.id = remap(&n.id);
            n.file = self.f.path.clone();
            n.binding_key = n.binding_key.as_deref().map(remap);
        }
        let owner = |s: &str| {
            if Some(s) == old_root.as_deref() {
                self.root()
            } else {
                remap(s)
            }
        };
        for e in &mut facts.edges {
            e.id = remap(&e.id);
            e.source = owner(&e.source);
            e.target = owner(&e.target);
            e.file = Some(self.f.path.clone());
        }
        for r in &mut facts.references {
            r.id = remap(&r.id);
            r.source = owner(&r.source);
            r.file = self.f.path.clone();
            r.candidate_keys = r.candidate_keys.iter().map(|k| remap(k)).collect();
        }
        for d in &mut facts.diagnostics {
            d.file = self.f.path.clone();
        }
        if old_root.is_some() {
            facts.nodes.remove(0);
        }
    }
    fn append(&mut self, f: FileFacts) {
        self.f.nodes.extend(f.nodes);
        self.f.edges.extend(f.edges);
        self.f.references.extend(f.references);
        self.f.diagnostics.extend(f.diagnostics);
    }
    fn blade(&mut self, visible: &[u8]) {
        let text = std::str::from_utf8(visible).unwrap();
        for (p, _) in text.match_indices("@include") {
            let rest = text[p + 8..].trim_start();
            if let Some(rest) = rest.strip_prefix('(') {
                let rest = rest.trim_start();
                if let Some(q @ ('\'' | '"')) = rest.chars().next()
                    && let Some(end) = rest[1..].find(q)
                {
                    let view = &rest[1..end + 1];
                    self.reference(
                        &self.root(),
                        view,
                        "includes",
                        p,
                        vec![format!(
                            "template:file:resources/views/{}.blade.php",
                            view.replace('.', "/")
                        )],
                    );
                }
            }
        }
    }
    fn razor(
        &mut self,
        visible: &mut [u8],
        bindings: &mut HashMap<String, Vec<String>>,
    ) -> Result<()> {
        let text = std::str::from_utf8(visible)?.to_owned();
        let mut namespaces = vec![];
        let mut aliases = HashMap::new();
        let mut pos = 0;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim();
            if let Some(value) = trimmed.strip_prefix("@using ") {
                let value = value.trim_end_matches(';').trim();
                let target = if let Some((alias, ty)) = value.split_once('=') {
                    aliases.insert(alias.trim().to_owned(), ty.trim().to_owned());
                    ty.trim()
                } else if let Some(ty) = value.strip_prefix("static ") {
                    ty.trim()
                } else {
                    namespaces.push(value.to_owned());
                    value
                };
                self.reference(
                    &self.root(),
                    target,
                    "imports",
                    pos,
                    vec![format!("csharp:symbol:{target}")],
                );
            }
            pos += line.len();
        }
        let type_keys = |ty: &str| {
            let base = ty.split(['<', '[', '?']).next().unwrap_or(ty).trim();
            if let Some(alias) = aliases.get(base) {
                vec![format!("csharp:symbol:{alias}")]
            } else if base.contains('.') {
                vec![format!("csharp:symbol:{base}")]
            } else {
                namespaces
                    .iter()
                    .map(|ns| format!("csharp:symbol:{ns}.{base}"))
                    .chain(std::iter::once(format!("csharp:symbol:{base}")))
                    .collect()
            }
        };
        let component_name = module_path(&self.f.path)
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        let namespace = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("@namespace "))
            .map(str::trim);
        let qualified = namespace
            .map(|ns| format!("{ns}.{component_name}"))
            .unwrap_or(component_name.clone());
        self.node(
            &component_name,
            "component",
            0..self.source.len(),
            Some(format!("csharp:symbol:{qualified}")),
            Some(&self.root()),
        );
        let mut at = 0;
        while let Some(offset) = text[at..].find('<') {
            at += offset;
            if let Some(tag) = tag_at(&text, at) {
                if tag.name.chars().next().is_some_and(char::is_uppercase) {
                    bindings.insert(tag.name.clone(), type_keys(&tag.name));
                }
                at = tag.range.end;
            } else {
                at += 1;
            }
        }
        pos = 0;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim();
            for (directive, relation) in [("@inherits ", "inherits"), ("@model ", "uses_type")] {
                if let Some(ty) = trimmed.strip_prefix(directive) {
                    self.reference(&self.root(), ty.trim(), relation, pos, type_keys(ty.trim()));
                }
            }
            if let Some(value) = trimmed.strip_prefix("@inject ")
                && let Some((ty, name)) = value.rsplit_once(char::is_whitespace)
            {
                let start = pos + line.find(name).unwrap_or(0);
                let field = self.node(
                    name,
                    "field",
                    start..start + name.len(),
                    None,
                    Some(&self.root()),
                );
                self.reference(&field, ty.trim(), "uses_type", pos, type_keys(ty.trim()));
            }
            if let Some(route) = trimmed.strip_prefix("@page ") {
                self.f.nodes[0].metadata["route"] = json!(route.trim_matches('"'));
            }
            pos += line.len();
        }
        for directive in ["@code", "@functions"] {
            for (at, _) in text.match_indices(directive) {
                let after = at + directive.len();
                let start = after + text[after..].len() - text[after..].trim_start().len();
                if text.as_bytes().get(start) != Some(&b'{') {
                    continue;
                }
                let Some(end) = balanced(&text, start, b'{', b'}') else {
                    self.diagnostic_at(at, "Unclosed Razor code block");
                    continue;
                };
                // Use the C# grammar on a class wrapper. Only real source nodes are retained.
                let prefix = "class Template {";
                let code = format!("{}{}\n}}", prefix, &text[start + 1..end - 1]);
                let mut temporary = super::common::Extractor::new(
                    &self.f.path,
                    &code,
                    &self.f.hash,
                    "csharp",
                    self.f.module.clone(),
                )
                .facts;
                let parsed = tree(tree_sitter_c_sharp::LANGUAGE.into(), &code, &mut temporary)?;
                if let Some(parsed) = parsed {
                    let class = children(parsed.root_node())
                        .into_iter()
                        .find(|n| n.kind() == "class_declaration");
                    if let Some(body) = class.and_then(|n| n.child_by_field_name("body")) {
                        for method in children(body)
                            .into_iter()
                            .filter(|n| n.kind() == "method_declaration")
                        {
                            let Some(name) = method.child_by_field_name("name") else {
                                continue;
                            };
                            let name = &code[name.byte_range()];
                            let offset = |p: usize| start + 1 + p - prefix.len();
                            let key = format!("template:method:{}:{name}", self.f.path);
                            let id = self.node(
                                name,
                                "method",
                                offset(method.start_byte())..offset(method.end_byte()),
                                Some(key.clone()),
                                Some(&self.root()),
                            );
                            bindings.entry(name.into()).or_default().push(key);
                            let mut pending = vec![method];
                            while let Some(n) = pending.pop() {
                                if n.kind() == "invocation_expression"
                                    && let Some(target) = n.child_by_field_name("function")
                                {
                                    let label = &code[target.byte_range()];
                                    self.reference(
                                        &id,
                                        label,
                                        "calls",
                                        offset(n.start_byte()),
                                        if identifier(label) {
                                            vec![format!("template:method:{}:{label}", self.f.path)]
                                        } else {
                                            vec![]
                                        },
                                    );
                                }
                                pending.extend(children(n));
                            }
                        }
                    }
                } else {
                    self.diagnostic_at(
                        start,
                        "Invalid C# in Razor code block; block facts omitted",
                    );
                }
                blank(visible, at..end);
            }
        }
        Ok(())
    }
    fn xaml(&mut self) {
        let mut reader = Reader::from_str(self.source);
        let mut stack: Vec<String> = vec![];
        let mut namespaces = HashMap::new();
        let mut namespace_stack = vec![];
        let mut element_names: Vec<String> = vec![];
        let mut root_context: Option<String> = None;
        let mut nested_context = false;
        let mut has_data_context = false;
        let mut prism_autowire = None;
        let mut class = None;
        let mut roots = 0;
        let mut failure = None;
        loop {
            let start = reader.buffer_position() as usize;
            let event = reader.read_event();
            let end = reader.buffer_position() as usize;
            match event {
                Ok(Event::Start(tag) | Event::Empty(tag)) => {
                    let empty = self.source[start..end].trim_end().ends_with("/>");
                    let name = String::from_utf8_lossy(tag.name().as_ref()).into_owned();
                    let mut attrs = vec![];
                    for attr in tag.attributes() {
                        match attr.ok().and_then(|a| {
                            a.decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .ok()
                            .map(|v| {
                                (
                                    String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                                    v.into_owned(),
                                )
                            })
                        }) {
                            Some(pair) => attrs.push(pair),
                            None => {
                                failure = Some("Invalid XAML attribute or entity");
                                break;
                            }
                        }
                    }
                    if failure.is_some() {
                        break;
                    }
                    if stack.is_empty() {
                        roots += 1;
                    }
                    if roots > 1 || stack.len() > 256 {
                        failure = Some("Invalid XAML root or excessive nesting");
                        break;
                    }
                    let previous_namespaces = namespaces.clone();
                    for (k, v) in &attrs {
                        if let Some(prefix) = k.strip_prefix("xmlns:") {
                            namespaces.insert(prefix.to_owned(), v.clone());
                        }
                    }
                    has_data_context |= name.ends_with(".DataContext") || name == "DataContext";
                    for (attr, value) in &attrs {
                        let local = attr.rsplit(':').next().unwrap_or(attr);
                        if local == "DataContext" {
                            has_data_context = true;
                            nested_context |= !stack.is_empty();
                        }
                        if local == "ViewModelLocator.AutoWireViewModel"
                            && attr
                                .split_once(':')
                                .and_then(|(prefix, _)| namespaces.get(prefix))
                                .is_some_and(|ns| ns == "http://prismlibrary.com/")
                        {
                            prism_autowire = Some(value.eq_ignore_ascii_case("true"));
                        }
                    }
                    let mut owner = stack.last().cloned().unwrap_or_else(|| self.root());
                    if let Some((_, name)) = attrs.iter().find(|(k, _)| k == "x:Class") {
                        class = Some(name.clone());
                        self.reference(
                            &owner,
                            name,
                            "code_behind",
                            start,
                            vec![format!("csharp:symbol:{name}")],
                        );
                    }
                    if let Some((_, control)) =
                        attrs.iter().find(|(k, _)| k == "x:Name" || k == "Name")
                    {
                        owner = self.node(
                            control,
                            "control",
                            start..end,
                            Some(format!("xaml:control:{}:{control}", self.f.path)),
                            Some(&owner),
                        );
                    }
                    if let Some((prefix, ty)) = name.split_once(':')
                        && let Some(ns) = namespaces
                            .get(prefix)
                            .and_then(|s| s.strip_prefix("clr-namespace:"))
                            .map(|s| s.split(';').next().unwrap())
                    {
                        let key = format!("csharp:symbol:{ns}.{ty}");
                        if element_names
                            .last()
                            .is_some_and(|n| n.ends_with(".DataContext"))
                        {
                            if stack.len() == 2 {
                                root_context = Some(key.clone());
                            } else {
                                nested_context = true;
                            }
                            self.reference(&owner, &name, "data_context", start, vec![key.clone()]);
                        }
                        self.reference(&owner, &name, "uses_type", start, vec![key]);
                    }
                    for (attr, value) in &attrs {
                        if attr == "x:Class"
                            || attr == "Name"
                            || attr == "x:Name"
                            || attr.starts_with("xmlns")
                        {
                            continue;
                        }
                        if value.starts_with("{Binding") || value.starts_with("{x:Bind") {
                            let args = value
                                .split_once(' ')
                                .map(|(_, v)| v.trim_end_matches('}'))
                                .unwrap_or("");
                            let head = args.split(',').next().unwrap_or("").trim();
                            let path = head.strip_prefix("Path=").unwrap_or(head);
                            if !path.is_empty() && !path.contains('=') {
                                self.reference(
                                    &owner,
                                    path,
                                    if attr == "Command" {
                                        "binds_command"
                                    } else {
                                        "binds"
                                    },
                                    start,
                                    vec![],
                                );
                            }
                        }
                        for prefix in ["{StaticResource ", "{DynamicResource "] {
                            if let Some(at) = value.find(prefix) {
                                let key =
                                    value[at + prefix.len()..].split('}').next().unwrap().trim();
                                self.reference(
                                    &owner,
                                    key,
                                    "uses_resource",
                                    start,
                                    vec![format!("xaml:resource:{}:{key}", self.f.path)],
                                );
                            }
                        }
                        if attr == "x:Key" {
                            self.node(
                                value,
                                "resource",
                                start..end,
                                Some(format!("xaml:resource:{}:{value}", self.f.path)),
                                Some(&owner),
                            );
                        }
                        if matches!(
                            attr.as_str(),
                            "Click"
                                | "Loaded"
                                | "Unloaded"
                                | "TextChanged"
                                | "SelectionChanged"
                                | "Checked"
                                | "Unchecked"
                                | "Tapped"
                                | "KeyDown"
                                | "KeyUp"
                                | "MouseDown"
                                | "MouseUp"
                                | "SizeChanged"
                                | "Closing"
                                | "Closed"
                                | "GotFocus"
                                | "LostFocus"
                        ) && identifier(value)
                        {
                            self.reference(
                                &owner,
                                value,
                                "binds_method",
                                start,
                                class
                                    .as_ref()
                                    .map(|c| vec![format!("csharp:symbol:{c}.{value}")])
                                    .unwrap_or_default(),
                            );
                        }
                        if value.starts_with("{d:DesignInstance ") {
                            let ty = value
                                .trim_start_matches("{d:DesignInstance ")
                                .split([',', '}'])
                                .next()
                                .unwrap_or("")
                                .trim()
                                .trim_start_matches("Type=");
                            let keys = ty
                                .split_once(':')
                                .and_then(|(p, n)| {
                                    namespaces
                                        .get(p)
                                        .and_then(|s| s.strip_prefix("clr-namespace:"))
                                        .map(|s| {
                                            vec![format!(
                                                "csharp:symbol:{}.{}",
                                                s.split(';').next().unwrap(),
                                                n
                                            )]
                                        })
                                })
                                .unwrap_or_default();
                            if stack.is_empty() {
                                root_context = keys.first().cloned();
                            } else {
                                nested_context = true;
                            }
                            self.reference(&owner, ty, "data_context", start, keys);
                        }
                    }
                    if !empty {
                        stack.push(owner);
                        element_names.push(name);
                        namespace_stack.push(previous_namespaces);
                    } else {
                        namespaces = previous_namespaces;
                    }
                }
                Ok(Event::End(_)) => {
                    element_names.pop();
                    namespaces = namespace_stack.pop().unwrap_or_default();
                    if stack.pop().is_none() {
                        failure = Some("Unexpected XAML closing element");
                        break;
                    }
                }
                Ok(Event::DocType(_)) => {
                    failure = Some("DOCTYPE is not supported in XAML");
                    break;
                }
                Ok(Event::Text(t))
                    if stack.is_empty() && t.iter().any(|b| !b.is_ascii_whitespace()) =>
                {
                    failure = Some("Text outside XAML root");
                    break;
                }
                Ok(Event::Eof) => {
                    if !stack.is_empty() || roots != 1 {
                        failure = Some("Incomplete XAML document");
                    }
                    break;
                }
                Err(_) => {
                    failure = Some("Malformed XAML document");
                    break;
                }
                _ => {}
            }
        }
        self.f.nodes[0].metadata["xaml"] = json!({
            "class": class, "has_data_context": has_data_context,
            "explicit_context": root_context, "nested_context": nested_context,
            "prism_autowire": prism_autowire,
        });
        if !nested_context && let Some(context) = root_context {
            for reference in &mut self.f.references {
                if matches!(reference.relation.as_str(), "binds" | "binds_command")
                    && identifier(&reference.label)
                {
                    reference.candidate_keys = vec![format!("{context}.{}", reference.label)];
                }
            }
        }
        if let Some(message) = failure {
            self.f.nodes.clear();
            self.f.edges.clear();
            self.f.references.clear();
            diagnostic(&mut self.f, None, message);
        }
    }
    fn robot(&mut self) {
        self.f.nodes[0].label = self.f.path.rsplit('/').next().unwrap().into();
        self.f.nodes[0].metadata["coverage"] = json!("static-subset");
        diagnostic(
            &mut self.f,
            None,
            "Robot static subset: English tables, definitions, imports, fixtures and literal keyword calls; bounded literal variables in import paths only; no runtime variable evaluation, embedded-argument keywords, localized syntax or library introspection",
        );
        let mut section = String::new();
        let mut owner = self.root();
        let mut pos = 0;
        let mut suite_template: Option<String> = None;
        let mut current_template = None;
        let mut resources = vec![];
        let mut calls = vec![];
        let variables = robot_variables(self.source);
        let mut definition_index: Option<usize> = None;
        for line in self.source.split_inclusive('\n') {
            let cells = robot_cells(line);
            let trimmed = cells.first().map_or("", |(_, s)| *s);
            if trimmed.starts_with("***") && trimmed.ends_with("***") {
                section = robot_normalize(trimmed.trim_matches('*').trim());
                owner = self.root();
                definition_index = None;
                pos += line.len();
                continue;
            }
            if cells.is_empty() || cells[0].1.starts_with('#') {
                pos += line.len();
                continue;
            }
            let first = cells[0].1;
            let row_at = pos + cells[0].0;
            let indent = if line.trim_start().starts_with('|') {
                line.trim_start()[1..]
                    .split('|')
                    .next()
                    .is_some_and(|s| s.trim().is_empty())
            } else {
                line.starts_with([' ', '\t'])
            };
            if matches!(section.as_str(), "testcases" | "tasks" | "keywords") && !indent {
                let kind = if section == "keywords" {
                    "keyword"
                } else {
                    "test"
                };
                let key = (kind == "keyword" && !first.contains("${"))
                    .then(|| format!("robot:keyword:{}:{}", self.f.path, robot_normalize(first)));
                owner = self.node(
                    first,
                    kind,
                    row_at..pos + line.trim_end().len(),
                    key,
                    Some(&self.root()),
                );
                definition_index = Some(self.f.nodes.len() - 1);
                current_template = if kind == "keyword" {
                    None
                } else {
                    suite_template.clone()
                };
                pos += line.len();
                continue;
            }
            if let Some(index) = definition_index {
                self.f.nodes[index].end_line = Some(self.line(pos));
                self.f.nodes[index].metadata["end_byte"] = json!(pos + line.trim_end().len());
            }
            let name = robot_normalize(first);
            if section == "settings"
                && matches!(name.as_str(), "resource" | "library" | "variables")
            {
                if let Some((_, target)) = cells.get(1) {
                    let expanded = robot_expand(target, &variables);
                    let named_library = name == "library"
                        && !target.contains(['/', '\\', '$', '%'])
                        && !target.ends_with(".py");
                    if named_library {
                        if !ROBOT_STANDARD_LIBRARIES.contains(target) {
                            let key = format!("robot:library:{}:{target}", self.f.path);
                            self.node(
                                target,
                                "library",
                                row_at..pos + line.trim_end().len(),
                                Some(key.clone()),
                                Some(&self.root()),
                            );
                            self.f.nodes.last_mut().unwrap().metadata["external"] = json!(true);
                            self.reference(&self.root(), target, "imports", row_at, vec![key]);
                        }
                    } else {
                        let path = expanded
                            .as_deref()
                            .and_then(|s| robot_import(&self.f.path, s));
                        let keys = path
                            .as_ref()
                            .map(|p| vec![robot_file_key(p)])
                            .unwrap_or_default();
                        self.reference(&self.root(), target, "imports", row_at, keys);
                        if name == "resource"
                            && let Some(path) = path
                        {
                            resources.push(path);
                        }
                    }
                }
            } else if matches!(
                name.as_str(),
                "suitesetup"
                    | "suiteteardown"
                    | "testsetup"
                    | "testteardown"
                    | "testtemplate"
                    | "[setup]"
                    | "[teardown]"
                    | "[template]"
            ) {
                if let Some((at, keyword)) = cells.get(1) {
                    let template = matches!(name.as_str(), "testtemplate" | "[template]");
                    if template {
                        let value =
                            (!keyword.eq_ignore_ascii_case("NONE")).then(|| keyword.to_string());
                        if name == "testtemplate" {
                            suite_template = value;
                        } else {
                            current_template = value;
                        }
                    }
                    if !keyword.eq_ignore_ascii_case("NONE") {
                        calls.push((owner.clone(), keyword.to_string(), pos + at));
                    }
                }
            } else if matches!(section.as_str(), "testcases" | "tasks" | "keywords") && indent {
                if first.starts_with('[') || first == "..." {
                    pos += line.len();
                    continue;
                }
                if let Some(template) = &current_template {
                    calls.push((owner.clone(), template.clone(), row_at));
                } else if !matches!(
                    first,
                    "FOR"
                        | "END"
                        | "IF"
                        | "ELSE"
                        | "ELSE IF"
                        | "TRY"
                        | "EXCEPT"
                        | "FINALLY"
                        | "WHILE"
                        | "RETURN"
                        | "BREAK"
                        | "CONTINUE"
                        | "VAR"
                ) {
                    let call = cells.iter().find(|(_, s)| {
                        !(s.starts_with(['$', '@', '&'])
                            && s.trim_end_matches('=').trim_end().ends_with('}'))
                    });
                    if let Some((at, keyword)) = call {
                        calls.push((owner.clone(), keyword.to_string(), pos + at));
                    }
                }
            }
            pos += line.len();
        }
        for (owner, keyword, at) in calls {
            let (qualifier, name) = keyword
                .rsplit_once('.')
                .map_or((None, keyword.as_str()), |(q, n)| (Some(q), n));
            let mut names = vec![robot_normalize(name)];
            if let Some((prefix, rest)) = name.split_once(' ')
                && ["given", "when", "then", "and", "but"]
                    .contains(&prefix.to_ascii_lowercase().as_str())
            {
                names.push(robot_normalize(rest));
            }
            let mut keys = vec![];
            if !name.contains(['$', '@', '&', '%']) {
                for name in names {
                    for file in std::iter::once(&self.f.path).chain(resources.iter()) {
                        if qualifier.is_none_or(|q| {
                            module_path(file)
                                .rsplit('/')
                                .next()
                                .is_some_and(|stem| robot_normalize(stem) == robot_normalize(q))
                        }) {
                            keys.push(format!("robot:keyword:{file}:{name}"));
                        }
                    }
                }
            }
            if qualifier.is_none() && resources.len() > 1 {
                let local = keys
                    .iter()
                    .find(|key| {
                        self.f
                            .nodes
                            .iter()
                            .any(|n| n.binding_key.as_ref() == Some(key))
                    })
                    .cloned();
                keys = local.into_iter().collect();
            }
            self.reference(&owner, &keyword, "calls", at, keys);
        }
    }
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

fn blank(bytes: &mut [u8], range: Range<usize>) {
    for b in &mut bytes[range] {
        if !matches!(*b, b'\n' | b'\r') {
            *b = b' ';
        }
    }
}
fn mask_ranges(source: &str, ranges: &[Range<usize>]) -> String {
    let mut out = source.as_bytes().to_vec();
    blank(&mut out, 0..source.len());
    for range in ranges {
        out[range.clone()].copy_from_slice(&source.as_bytes()[range.clone()]);
    }
    String::from_utf8(out).expect("ranges end at character boundaries")
}
fn mask_comments(bytes: &mut [u8], source: &str, open: &str, close: &str) {
    let mut pos = 0;
    while let Some(at) = source[pos..].find(open) {
        let start = pos + at;
        let end = source[start + open.len()..]
            .find(close)
            .map(|p| start + open.len() + p + close.len())
            .unwrap_or(source.len());
        blank(bytes, start..end);
        pos = end;
    }
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
fn tag_at(text: &str, start: usize) -> Option<Tag> {
    let b = text.as_bytes();
    let mut pos = start + 1;
    if !b.get(pos)?.is_ascii_alphabetic() {
        return None;
    }
    while b
        .get(pos)
        .is_some_and(|c| c.is_ascii_alphanumeric() || b":._-".contains(c))
    {
        pos += 1;
    }
    let name = text[start + 1..pos].to_owned();
    let mut attrs = vec![];
    loop {
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if *b.get(pos)? == b'>' {
            return Some(Tag {
                name,
                range: start..pos + 1,
                attrs,
            });
        }
        if b.get(pos..pos + 2) == Some(b"/>") {
            return Some(Tag {
                name,
                range: start..pos + 2,
                attrs,
            });
        }
        if b[pos] == b'{' {
            pos = balanced(text, pos, b'{', b'}')?;
            continue;
        }
        let key_start = pos;
        while b
            .get(pos)
            .is_some_and(|c| !c.is_ascii_whitespace() && !b"=>/".contains(c))
        {
            pos += 1;
        }
        if pos == key_start {
            return None;
        }
        let key = text[key_start..pos].to_owned();
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if b.get(pos) != Some(&b'=') {
            attrs.push(Attribute {
                name: key,
                value: String::new(),
                range: key_start..pos,
                braced: false,
            });
            continue;
        }
        pos += 1;
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let start_value = pos;
        let mut braced = false;
        let value = match *b.get(pos)? {
            quote @ (b'\'' | b'"') => {
                pos += 1;
                let from = pos;
                while *b.get(pos)? != quote {
                    pos += 1;
                }
                let s = text[from..pos].to_owned();
                pos += 1;
                s
            }
            b'{' => {
                braced = true;
                let end = balanced(text, pos, b'{', b'}')?;
                let s = text[pos + 1..end - 1].to_owned();
                pos = end;
                s
            }
            _ => {
                while b
                    .get(pos)
                    .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>')
                {
                    pos += 1;
                }
                text[start_value..pos].trim_end_matches('/').into()
            }
        };
        attrs.push(Attribute {
            name: key,
            value,
            range: start_value..pos,
            braced,
        });
    }
}
fn find_close_tag(text: &str, start: usize, needle: &str) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let n = needle.as_bytes();
    let mut pos = start;
    while pos + n.len() < b.len() {
        if b[pos..pos + n.len()].eq_ignore_ascii_case(n)
            && (b[pos + n.len()].is_ascii_whitespace() || b[pos + n.len()] == b'>')
        {
            let end = text[pos + n.len()..].find('>')? + pos + n.len() + 1;
            return Some((pos, end));
        }
        pos += 1;
    }
    None
}
// Bounded lexical delimiters; quoted strings and ordinary comments cannot close a block.
fn balanced(text: &str, start: usize, open: u8, close: u8) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0;
    let mut pos = start;
    while pos < b.len() {
        match b[pos] {
            quote @ (b'\'' | b'"' | b'`') => {
                pos += 1;
                while pos < b.len() {
                    if b[pos] == b'\\' {
                        pos += 2;
                    } else if b[pos] == quote {
                        pos += 1;
                        break;
                    } else {
                        pos += 1;
                    }
                }
            }
            b'/' if b.get(pos + 1) == Some(&b'/') => {
                while pos < b.len() && b[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'/' if b.get(pos + 1) == Some(&b'*') => {
                pos += 2;
                while pos + 1 < b.len() && &b[pos..pos + 2] != b"*/" {
                    pos += 1;
                }
                pos += 2;
            }
            c if c == open => {
                depth += 1;
                if depth > 256 {
                    return None;
                }
                pos += 1;
            }
            c if c == close => {
                depth -= 1;
                pos += 1;
                if depth == 0 {
                    return Some(pos);
                }
            }
            _ => pos += 1,
        }
    }
    None
}
fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c == '$' || c.is_alphabetic() || (i > 0 && c.is_numeric()))
}
fn pascal(name: &str) -> String {
    name.split('-')
        .map(|s| {
            let mut chars = s.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}
fn js_modules(path: &str, import: &str) -> Vec<String> {
    if import.starts_with('.') {
        relative_path(path.rsplit_once('/').map_or("", |(p, _)| p), import)
            .map(|p| vec![format!("javascript:module:{p}")])
            .unwrap_or_default()
    } else {
        vec![format!("javascript:import-module:{import}")]
    }
}
fn robot_normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}
fn robot_import(path: &str, raw: &str) -> Option<String> {
    let base = path.rsplit_once('/').map_or("", |(p, _)| p);
    let mut result = String::new();
    let mut rest = raw;
    let mut execution_root = false;
    while let Some(at) = rest.find("${") {
        result.push_str(&rest[..at]);
        let end = rest[at + 2..].find('}')? + at + 2;
        match robot_normalize(&rest[at + 2..end]).as_str() {
            "curdir" if result.is_empty() => result.push('.'),
            "execdir" if result.is_empty() => {
                execution_root = true;
                result.push('.');
            }
            "/" => result.push('/'),
            _ => return None,
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    if result.starts_with('/') || result.contains(['%', '\\']) {
        return None;
    }
    relative_path(if execution_root { "" } else { base }, &result)
}
fn robot_cells(line: &str) -> Vec<(usize, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim_start().starts_with('|') {
        let mut pos = line.find('|').unwrap() + 1;
        let mut cells = vec![];
        for s in line[pos..].split('|') {
            let leading = s.len() - s.trim_start().len();
            let value = s.trim();
            if !value.is_empty() {
                cells.push((pos + leading, value));
            }
            pos += s.len() + 1;
        }
        return cells;
    }
    let b = line.as_bytes();
    let mut pos = 0;
    let mut cells = vec![];
    while pos < b.len() {
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let start = pos;
        while pos < b.len() && b[pos] != b'\t' && !(b[pos] == b' ' && b.get(pos + 1) == Some(&b' '))
        {
            pos += 1;
        }
        if pos > start {
            let value = line[start..pos].trim_end();
            if value.starts_with('#') {
                break;
            }
            cells.push((start, value));
        }
    }
    cells
}

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

fn toolkit_using(
    root: tree_sitter::Node<'_>,
    member: tree_sitter::Node<'_>,
    source: &str,
    namespace: &str,
) -> bool {
    let has = |node| {
        children(node).into_iter().any(|n| {
            n.kind() == "using_directive"
                && source[n.byte_range()]
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .strip_prefix("using ")
                    .or_else(|| {
                        source[n.byte_range()]
                            .trim()
                            .trim_end_matches(';')
                            .trim()
                            .strip_prefix("global using ")
                    })
                    .is_some_and(|s| s.trim() == namespace)
        })
    };
    if has(root) {
        return true;
    }
    let mut parent = member.parent();
    while let Some(node) = parent {
        if has(node) {
            return true;
        }
        parent = node.parent();
    }
    false
}
fn event_signature(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return false;
    };
    let parameters: Vec<_> = children(parameters)
        .into_iter()
        .filter(|n| n.kind() == "parameter")
        .collect();
    if parameters.len() != 2 {
        return false;
    }
    let types: Vec<_> = parameters
        .iter()
        .filter_map(|n| n.child_by_field_name("type"))
        .map(|n| source[n.byte_range()].replace(' ', ""))
        .collect();
    types.len() == 2
        && matches!(types[0].trim_end_matches('?'), "object" | "System.Object")
        && types[1]
            .split('<')
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("")
            .ends_with("EventArgs")
}
fn qualify(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.into()
    } else {
        format!("{prefix}.{name}")
    }
}
fn relative_source(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && !path.split('/').any(|s| matches!(s, "" | "." | ".."))
}
fn in_project(path: &str, root: &str) -> bool {
    relative_source(path)
        && (root.is_empty()
            || (relative_source(root)
                && path.strip_prefix(root).is_some_and(|s| s.starts_with('/'))))
}
fn project_key(project: &TemplateProject<'_>, category: &str, name: &str) -> String {
    format!("xaml:project:{}:{category}:{name}", project.root)
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
fn viewmodel_names(view: &str) -> Vec<String> {
    if view == "MainWindow" {
        return vec!["MainWindowViewModel".into(), "MainViewModel".into()];
    }
    for suffix in ["UserControl", "View", "Page", "Control"] {
        if let Some(stem) = view.strip_suffix(suffix).filter(|s| !s.is_empty()) {
            return vec![format!("{stem}ViewModel")];
        }
    }
    vec![]
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
fn robot_file_key(path: &str) -> String {
    if let Some(stem) = path.strip_suffix(".py") {
        let stem = stem
            .strip_prefix("src/")
            .unwrap_or(stem)
            .trim_end_matches("/__init__");
        format!(
            "module:{}",
            if stem == "__init__" {
                String::new()
            } else {
                stem.replace('/', ".")
            }
        )
    } else {
        format!("template:file:{path}")
    }
}
fn robot_variables(source: &str) -> HashMap<String, Option<String>> {
    let mut variables = HashMap::new();
    let mut in_variables = false;
    for line in source.lines() {
        let cells = robot_cells(line);
        let Some((_, first)) = cells.first() else {
            continue;
        };
        if first.starts_with("***") {
            in_variables = robot_normalize(first.trim_matches('*')) == "variables";
            continue;
        }
        if !in_variables || !first.starts_with("${") || !first.ends_with('}') {
            continue;
        }
        let name = robot_normalize(&first[2..first.len() - 1]);
        let value = (cells.len() == 2 && !cells[1].1.contains(['\\', '@', '&', '%']))
            .then(|| cells[1].1.to_owned());
        variables
            .entry(name)
            .and_modify(|v| *v = None)
            .or_insert(value);
    }
    variables
}
fn robot_expand(raw: &str, variables: &HashMap<String, Option<String>>) -> Option<String> {
    let mut result = raw.to_owned();
    for _ in 0..8 {
        let mut next = String::new();
        let mut rest = result.as_str();
        let mut replaced = false;
        while let Some(at) = rest.find("${") {
            next.push_str(&rest[..at]);
            let end = rest[at + 2..].find('}')? + at + 2;
            let name = robot_normalize(&rest[at + 2..end]);
            if matches!(name.as_str(), "curdir" | "execdir" | "/") {
                next.push_str(&rest[at..=end]);
            } else {
                next.push_str(variables.get(&name)?.as_deref()?);
                replaced = true;
            }
            rest = &rest[end + 1..];
        }
        next.push_str(rest);
        if next.len() > 4096 {
            return None;
        }
        if !replaced {
            return Some(next);
        }
        result = next;
    }
    None
}
