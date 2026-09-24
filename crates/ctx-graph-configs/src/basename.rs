use super::*;

pub(super) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

pub(super) fn json_config(path: &str) -> bool {
    let n = basename(path).to_ascii_lowercase();
    JSON_NAMES.contains(&n.as_str())
        || [
            ".eslintrc.json",
            ".prettierrc.json",
            ".babelrc.json",
            "tsconfig.json",
            "jsconfig.json",
        ]
        .iter()
        .any(|s| n.ends_with(s))
        || ((n.starts_with("tsconfig.") || n.starts_with("jsconfig.")) && n.ends_with(".json"))
}

pub(super) fn package_key(ecosystem: &str, name: &str) -> String {
    let name = if ecosystem == "python" {
        name.to_ascii_lowercase().replace(['_', '.'], "-")
    } else {
        name.to_owned()
    };
    format!("package:{ecosystem}:{name}")
}

pub(super) fn local_target(path: &str, target: &str) -> Option<String> {
    let target = target.replace('\\', "/");
    if target.starts_with('/')
        || target.contains(':')
        || target.contains('$')
        || target.contains('*')
    {
        return None;
    }
    relative_path(directory(path), &target)
}

pub(super) fn path_keys(path: &str, target: &str) -> Vec<String> {
    local_target(path, target)
        .map(|p| vec![format!("config:file:{p}")])
        .unwrap_or_default()
}

pub(super) fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => vec![],
    }
}

pub(super) fn toml_value(item: &toml_edit::Item) -> Value {
    if let Some(t) = item.as_table_like() {
        return Value::Object(t.iter().map(|(k, v)| (k.into(), toml_value(v))).collect());
    }
    if let Some(a) = item.as_array_of_tables() {
        return Value::Array(
            a.iter()
                .map(|t| toml_value(&toml_edit::Item::Table(t.clone())))
                .collect(),
        );
    }
    fn value(v: &toml_edit::Value) -> Value {
        match v {
            toml_edit::Value::String(s) => json!(s.value()),
            toml_edit::Value::Boolean(b) => json!(b.value()),
            toml_edit::Value::Integer(n) => json!(n.value()),
            toml_edit::Value::Float(n) => json!(n.value()),
            toml_edit::Value::Array(a) => Value::Array(a.iter().map(value).collect()),
            toml_edit::Value::InlineTable(t) => {
                Value::Object(t.iter().map(|(k, v)| (k.into(), value(v))).collect())
            }
            _ => Value::Null,
        }
    }
    item.as_value().map(value).unwrap_or(Value::Null)
}

pub(super) fn cargo_manifest_path(directory: &str) -> String {
    if directory.is_empty() {
        "Cargo.toml".into()
    } else {
        format!("{directory}/Cargo.toml")
    }
}

pub(super) fn cargo_package_key(manifest: &str, name: &str) -> String {
    format!("cargo:manifest:{manifest}:{name}")
}

pub(super) fn cargo_directory(manifest: &str, path: &str) -> Option<String> {
    if path.is_empty()
        || path.starts_with(['/', '~'])
        || path.contains(['\\', ':', '$', '%', '*', '?', '[', ']'])
    {
        return None;
    }
    relative_path(directory(manifest), path)
}

pub(super) fn cargo_patterns(manifest: &str, value: &Value) -> Option<Vec<globset::GlobMatcher>> {
    if value.is_null() {
        return Some(vec![]);
    }
    value
        .as_array()?
        .iter()
        .map(|pattern| {
            let pattern = pattern.as_str()?;
            if pattern.is_empty()
                || pattern.starts_with(['/', '~'])
                || pattern.contains(['\\', ':', '$', '%', '{', '}'])
            {
                return None;
            }
            let mut wildcard = false;
            for component in pattern.split('/') {
                if component == ".." && wildcard {
                    return None;
                }
                wildcard |= component.contains(['*', '?', '[']);
            }
            let pattern = relative_path(directory(manifest), pattern)?;
            globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .ok()
                .map(|g| g.compile_matcher())
        })
        .collect()
}

pub(super) fn toml_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let doc: toml_edit::DocumentMut = source.parse()?;
    let data = toml_value(doc.as_item());
    if basename(&f.0.path) == "Cargo.toml" {
        let root = f.root("cargo", json!({"workspace":data["workspace"], "package":data["package"], "lib":data["lib"], "bin":data["bin"], "dependencies":data["dependencies"], "target":data["target"]}));
        let Some(name) = data["package"]["name"].as_str() else {
            return Ok(());
        };
        let owner = f.package(&root, "cargo", name, data["package"]["version"].as_str());
        let mut tables = vec![&data["dependencies"]];
        if let Some(targets) = data["target"].as_object() {
            tables.extend(targets.values().map(|v| &v["dependencies"]));
        }
        for table in tables {
            if let Some(deps) = table.as_object() {
                for (alias, spec) in deps {
                    let name = spec["package"].as_str().unwrap_or(alias);
                    if spec["workspace"].as_bool() == Some(true) {
                        f.reference(
                            &owner,
                            alias,
                            "depends_on",
                            vec![format!(
                                "cargo:workspace-dependency:{}:{alias}",
                                directory(&f.0.path)
                            )],
                            1,
                        );
                    } else if let Some(path) = spec["path"].as_str() {
                        let keys = local_target(&f.0.path, path)
                            .map(|p| vec![cargo_package_key(&cargo_manifest_path(&p), name)])
                            .unwrap_or_default();
                        f.reference(&owner, name, "depends_on", keys, 1);
                    } else {
                        f.dependency(&owner, "cargo", name, 1);
                    }
                }
            }
        }
    } else {
        let root = f.root("python-manifest", json!({}));
        let p = &data["project"];
        let poetry = &data["tool"]["poetry"];
        let Some(name) = p["name"].as_str().or_else(|| poetry["name"].as_str()) else {
            return Ok(());
        };
        let owner = f.package(
            &root,
            "python",
            name,
            p["version"].as_str().or_else(|| poetry["version"].as_str()),
        );
        for spec in strings(&p["dependencies"]) {
            let name = spec
                .trim()
                .split(|c: char| c.is_whitespace() || "<>=!~;[(".contains(c))
                .next()
                .unwrap_or("");
            f.dependency(&owner, "python", name, 1);
        }
        if let Some(deps) = poetry["dependencies"].as_object() {
            for name in deps.keys().filter(|n| !n.eq_ignore_ascii_case("python")) {
                f.dependency(&owner, "python", name, 1);
            }
        }
    }
    Ok(())
}

pub(super) fn apm_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let data: Value = serde_yaml_ng::from_str(source)?;
    let root = f.root("apm", json!({}));
    let Some(name) = data["name"].as_str() else {
        return Ok(());
    };
    let owner = f.package(&root, "apm", name, data["version"].as_str());
    match &data["dependencies"] {
        Value::Object(d) => {
            for name in d.keys() {
                f.dependency(&owner, "apm", name, 1);
            }
        }
        Value::Array(d) => {
            for item in d {
                if let Some(name) = item.as_str().or_else(|| {
                    item.as_object()
                        .and_then(|o| o.keys().next().map(String::as_str))
                }) {
                    f.dependency(&owner, "apm", name, 1);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn go_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let lines: Vec<_> = source
        .lines()
        .map(|s| s.split("//").next().unwrap_or("").trim())
        .collect();
    let name = lines
        .iter()
        .find_map(|s| s.strip_prefix("module ").map(str::trim))
        .unwrap_or("")
        .trim_matches('"');
    let root = f.root("go-manifest", json!({"module":name}));
    if name.is_empty() {
        return Ok(());
    }
    let owner = f.package(&root, "go", name, None);
    let mut block = false;
    for (i, s) in lines.iter().enumerate() {
        if let Some(tail) = s.strip_prefix("require") {
            let tail = tail.trim();
            block = tail.starts_with('(');
            if !block && let Some(dep) = tail.split_whitespace().next() {
                f.dependency(&owner, "go", dep.trim_matches('"'), i as u32 + 1);
            }
        } else if *s == ")" {
            block = false;
        } else if block && let Some(dep) = s.split_whitespace().next() {
            f.dependency(&owner, "go", dep.trim_matches('"'), i as u32 + 1);
        }
    }
    Ok(())
}

pub(super) fn jsonc(source: &str) -> Result<String> {
    let b = source.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                } else if b[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if b.get(i..i + 2) == Some(b"//") {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b.get(i..i + 2) == Some(b"/*") {
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
            while i + 1 < b.len() && &b[i..i + 2] != b"*/" {
                if b[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            if i + 1 >= b.len() {
                bail!("unterminated comment");
            }
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
        } else {
            i += 1;
        }
    }
    i = 0;
    while i < out.len() {
        if out[i] == b'"' {
            i += 1;
            while i < out.len() {
                if out[i] == b'\\' {
                    i += 2;
                } else if out[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            if out[i] == b','
                && out[i + 1..]
                    .iter()
                    .find(|c| !c.is_ascii_whitespace())
                    .is_some_and(|c| matches!(c, b'}' | b']'))
            {
                out[i] = b' ';
            }
            i += 1;
        }
    }
    Ok(String::from_utf8(out)?)
}

pub(super) fn json_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let data: Value = serde_json::from_str(&jsonc(source)?)?;
    if !data.is_object() {
        bail!("configuration must be an object");
    }
    let root = f.root("json-config", json!({"compilerOptions":data["compilerOptions"],"imports":data["imports"],"exports":data["exports"],"workspaces":data["workspaces"],"name":data["name"]}));
    let ecosystem = if basename(&f.0.path) == "composer.json" {
        "composer"
    } else {
        "npm"
    };
    let owner = data["name"]
        .as_str()
        .map(|name| f.package(&root, ecosystem, name, data["version"].as_str()))
        .unwrap_or_else(|| root.clone());
    let mut pending = vec![(&data, root.clone(), String::new(), 0)];
    let mut count = 0;
    while let Some((value, parent, pointer, depth)) = pending.pop() {
        if depth > 6 {
            continue;
        }
        let Some(object) = value.as_object() else {
            continue;
        };
        for (name, value) in object {
            if count >= 500 {
                diagnostic(&mut f.0, None, "Configuration key limit reached");
                return Ok(());
            }
            count += 1;
            let pointer = format!("{pointer}/{}", name.replace('~', "~0").replace('/', "~1"));
            let key = f.node(
                name,
                "config_key",
                Some(format!("config:key:{}#{pointer}", f.0.path)),
                1,
                json!({"pointer":pointer}),
            );
            f.edge(&parent, &key, "contains", 1);
            if matches!(
                name.as_str(),
                "dependencies"
                    | "devDependencies"
                    | "peerDependencies"
                    | "optionalDependencies"
                    | "bundleDependencies"
                    | "bundledDependencies"
                    | "require"
                    | "require-dev"
            ) {
                if let Some(deps) = value.as_object() {
                    let npm_group = matches!(
                        name.as_str(),
                        "dependencies"
                            | "devDependencies"
                            | "peerDependencies"
                            | "optionalDependencies"
                    );
                    for (name, specifier) in deps {
                        f.dependency(&owner, ecosystem, name, 1);
                        // A manifest declaration is useful evidence even when the
                        // package source is not part of this index. Keep it owned
                        // by this manifest, distinct from an installed package.
                        if depth == 0
                            && basename(&f.0.path) == "package.json"
                            && npm_group
                            && specifier.is_string()
                        {
                            let binding = format!("npm:dependency:{}:{name}", f.0.path);
                            if !f
                                .0
                                .nodes
                                .iter()
                                .any(|n| n.binding_key.as_deref() == Some(&binding))
                            {
                                f.node(
                                    name,
                                    "dependency",
                                    Some(binding),
                                    1,
                                    json!({"ecosystem":"npm","declared":true}),
                                );
                                let dependency = format!(
                                    "npm-dependency:{}:{}:{name}",
                                    f.0.path.len(),
                                    f.0.path
                                );
                                f.0.nodes.last_mut().unwrap().id = dependency.clone();
                                f.edge(&owner, &dependency, "depends_on", 1);
                            }
                        }
                    }
                }
                for name in strings(value) {
                    f.dependency(&owner, ecosystem, &name, 1);
                }
            }
            if matches!(name.as_str(), "extends" | "$ref" | "$schema") {
                for target in strings(value) {
                    let keys = if target.starts_with('#') {
                        vec![format!("config:key:{}{target}", f.0.path)]
                    } else if target.starts_with('.') {
                        let (p, frag) = target.split_once('#').unwrap_or((&target, ""));
                        local_target(&f.0.path, p)
                            .map(|p| {
                                vec![if frag.is_empty() {
                                    format!("config:file:{p}")
                                } else {
                                    format!("config:key:{p}#{frag}")
                                }]
                            })
                            .unwrap_or_default()
                    } else {
                        vec![]
                    };
                    f.reference(
                        &key,
                        &target,
                        if name == "extends" {
                            "extends"
                        } else {
                            "references"
                        },
                        keys,
                        1,
                    );
                }
            }
            if value.is_object() {
                pending.push((value, key, pointer, depth + 1));
            }
        }
    }
    Ok(())
}

pub(super) fn xml(source: &str) -> Result<Vec<Xml>> {
    let mut reader = Reader::from_str(source);
    let mut nodes: Vec<Xml> = vec![];
    let mut stack = vec![];
    let mut offset = 0;
    let mut current_line = 1;
    loop {
        let start = reader.buffer_position() as usize;
        current_line += source[offset..start]
            .bytes()
            .filter(|b| *b == b'\n')
            .count() as u32;
        offset = start;
        match reader.read_event()? {
            Event::DocType(_) => bail!("DTD is unsupported"),
            Event::Start(ref e) | Event::Empty(ref e) => {
                if stack.len() >= 128 || nodes.len() >= 20000 {
                    bail!("XML size limit");
                }
                let name = String::from_utf8(e.local_name().as_ref().to_vec())?;
                let mut attrs = BTreeMap::new();
                for a in e.attributes() {
                    let a = a?;
                    attrs.insert(
                        String::from_utf8(a.key.local_name().as_ref().to_vec())?,
                        a.decoded_and_normalized_value(
                            quick_xml::XmlVersion::Implicit1_0,
                            reader.decoder(),
                        )?
                        .into_owned(),
                    );
                }
                let index = nodes.len();
                nodes.push(Xml {
                    name,
                    attrs,
                    text: String::new(),
                    parent: stack.last().copied(),
                    line: current_line,
                });
                // Empty events consume their closing delimiter in this same event.
                if !source[start..reader.buffer_position() as usize]
                    .trim_end()
                    .ends_with("/>")
                {
                    stack.push(index);
                }
            }
            Event::End(_) => {
                if stack.pop().is_none() {
                    bail!("unmatched XML end");
                }
            }
            Event::Text(t) => {
                if let Some(&i) = stack.last() {
                    nodes[i]
                        .text
                        .push_str(&quick_xml::escape::unescape(&t.decode()?)?);
                }
            }
            Event::CData(t) => {
                if let Some(&i) = stack.last() {
                    nodes[i].text.push_str(&t.decode()?);
                }
            }
            Event::GeneralRef(reference) => {
                let Some(&index) = stack.last() else {
                    bail!("reference outside XML element");
                };
                if let Some(c) = reference.resolve_char_ref()? {
                    nodes[index].text.push(c);
                } else if let Some(text) =
                    quick_xml::escape::resolve_predefined_entity(&reference.decode()?)
                {
                    nodes[index].text.push_str(text);
                } else {
                    bail!("undeclared XML entity");
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() || nodes.iter().filter(|n| n.parent.is_none()).count() != 1 {
        bail!("invalid XML document");
    }
    Ok(nodes)
}

pub(super) fn attr<'a>(node: &'a Xml, key: &str) -> Option<&'a str> {
    node.attrs
        .get(key)
        .or_else(|| node.attrs.get(&key.to_ascii_lowercase()))
        .map(String::as_str)
}

pub(super) fn child_text<'a>(nodes: &'a [Xml], parent: usize, name: &str) -> Option<&'a str> {
    nodes
        .iter()
        .find(|n| n.parent == Some(parent) && n.name == name)
        .map(|n| n.text.trim())
        .filter(|s| !s.is_empty())
}

pub(super) fn xml_config(f: &mut Facts, source: &str, ecosystem: &str) -> Result<()> {
    let nodes = xml(source)?;
    let root = f.root(ecosystem, json!({}));
    if ecosystem == "maven" {
        let Some(artifact) = child_text(&nodes, 0, "artifactId") else {
            return Ok(());
        };
        let parent = nodes
            .iter()
            .position(|n| n.parent == Some(0) && n.name == "parent");
        let group = child_text(&nodes, 0, "groupId")
            .or_else(|| parent.and_then(|p| child_text(&nodes, p, "groupId")));
        let name = group
            .map(|g| format!("{g}:{artifact}"))
            .unwrap_or_else(|| artifact.into());
        let owner = f.package(&root, "maven", &name, child_text(&nodes, 0, "version"));
        for (i, n) in nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.name == "dependency")
        {
            if let Some(a) = child_text(&nodes, i, "artifactId") {
                let name = child_text(&nodes, i, "groupId")
                    .map(|g| format!("{g}:{a}"))
                    .unwrap_or_else(|| a.into());
                if !name.contains("${") {
                    f.dependency(&owner, "maven", &name, n.line);
                }
            }
        }
        return Ok(());
    }
    if f.0.path.ends_with(".slnx") {
        let mut owners = HashMap::from([(0, root.clone())]);
        let mut projects = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            let parent = n
                .parent
                .and_then(|p| owners.get(&p))
                .unwrap_or(&root)
                .clone();
            if n.name == "Folder" {
                if let Some(name) = attr(n, "Name") {
                    let id = f.node(name, "solution_folder", None, n.line, json!({}));
                    f.edge(&parent, &id, "contains", n.line);
                    owners.insert(i, id);
                }
            } else if n.name == "Project"
                && let Some(path) = attr(n, "Path")
            {
                let label = basename(path)
                    .rsplit_once('.')
                    .map_or(basename(path), |(s, _)| s);
                let id = f.node(
                    label,
                    "project_reference",
                    None,
                    n.line,
                    json!({"path":local_target(&f.0.path,path)}),
                );
                f.edge(&parent, &id, "contains", n.line);
                f.reference(&id, path, "references", path_keys(&f.0.path, path), n.line);
                projects.insert(path.replace('\\', "/"), id.clone());
                owners.insert(i, id);
            }
        }
        for n in &nodes {
            if n.name == "BuildDependency"
                && let (Some(owner), Some(path)) =
                    (n.parent.and_then(|p| owners.get(&p)), attr(n, "Project"))
            {
                if let Some(target) = projects.get(&path.replace('\\', "/")) {
                    f.edge(owner, target, "depends_on", n.line);
                } else {
                    f.reference(
                        owner,
                        path,
                        "depends_on",
                        path_keys(&f.0.path, path),
                        n.line,
                    );
                }
            }
        }
        return Ok(());
    }
    for (i, n) in nodes.iter().enumerate() {
        if matches!(n.name.as_str(), "TargetFramework" | "TargetFrameworks") {
            for tfm in n
                .text
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.contains('$'))
            {
                let id = f.node(tfm, "framework", None, n.line, json!({}));
                f.edge(&root, &id, "references", n.line);
            }
        }
        if n.name == "PackageReference"
            && let Some(name) = attr(n, "Include").or_else(|| attr(n, "Update"))
        {
            let version = attr(n, "Version").or_else(|| child_text(&nodes, i, "Version"));
            let id = f.node(
                name,
                "package_reference",
                None,
                n.line,
                json!({"ecosystem":"nuget","version":version,"condition":attr(n,"Condition")}),
            );
            f.edge(&root, &id, "imports", n.line);
            f.reference(
                &id,
                name,
                "references",
                vec![package_key("nuget", name)],
                n.line,
            );
        }
        if n.name == "ProjectReference"
            && let Some(path) = attr(n, "Include")
        {
            f.reference(&root, path, "imports", path_keys(&f.0.path, path), n.line);
        }
    }
    if let Some(sdk) = nodes.first().and_then(|n| attr(n, "Sdk")) {
        let id = f.node(sdk, "sdk", None, 1, json!({}));
        f.edge(&root, &id, "references", 1);
    }
    Ok(())
}
