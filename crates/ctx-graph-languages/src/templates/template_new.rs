use super::*;

impl<'a> Template<'a> {
    pub(super) fn new(path: &str, source: &'a str, hash: &str, language: &'a str) -> Self {
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
    pub(super) fn line(&self, byte: usize) -> u32 {
        self.lines.partition_point(|p| *p <= byte) as u32
    }
    pub(super) fn node(
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
    pub(super) fn diagnostic_at(&mut self, byte: usize, message: &str) {
        let line = self.line(byte);
        diagnostic(&mut self.f, Some(line), message);
    }
    pub(super) fn root(&self) -> String {
        self.f.nodes[0].id.clone()
    }
    pub(super) fn reference(
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
    pub(super) fn markup(&mut self, lang: &str) -> Result<()> {
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
            let mut facts = super::super::javascript::parse(&virtual_path, &masked, &self.f.hash)?;
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
            let mut resolved =
                super::super::javascript::parse(&virtual_path, &probe, &self.f.hash)?;
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
                    let mut facts =
                        super::super::javascript::parse(&virtual_path, &expr, &self.f.hash)?;
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
    pub(super) fn remap_js(&self, facts: &mut FileFacts, virtual_path: &str) {
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
    pub(super) fn append(&mut self, f: FileFacts) {
        self.f.nodes.extend(f.nodes);
        self.f.edges.extend(f.edges);
        self.f.references.extend(f.references);
        self.f.diagnostics.extend(f.diagnostics);
    }
    pub(super) fn blade(&mut self, visible: &[u8]) {
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
    pub(super) fn razor(
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
                let mut temporary = super::super::common::Extractor::new(
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
}
