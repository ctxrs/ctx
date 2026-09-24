use super::*;

impl<'a> Template<'a> {
    pub(super) fn xaml(&mut self) {
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
    pub(super) fn robot(&mut self) {
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
