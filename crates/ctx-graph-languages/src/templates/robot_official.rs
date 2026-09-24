use super::*;

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
    let adapter = ctx_graph_ingest::CommandAdapter {
        program: executable
            .to_str()
            .context("Robot Python path must be UTF-8")?
            .into(),
        args: vec![
            "-I".into(),
            "-B".into(),
            "-c".into(),
            include_str!("../../assets/robot_parser.py").into(),
        ],
        output_file: false,
    };
    let request =
        serde_json::to_vec(&json!({"schema_version": 1, "path": path, "source": source}))?;
    let raw = ctx_graph_ingest::run_command(&adapter, None, Some(&request), 10, 8 * 1024 * 1024)
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
