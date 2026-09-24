use super::*;

impl Javascript<'_> {
    pub(super) fn comments(&mut self, root: Syntax<'_>) {
        let mut pending = vec![root];
        let mut document_refs: HashMap<String, String> = HashMap::new();
        while let Some(comment) = pending.pop() {
            if comment.kind() != "comment" {
                pending.extend(children(comment).into_iter().rev());
                continue;
            }
            let raw = self.e.text(comment);
            let doc = raw.starts_with("/**");
            let text = raw
                .trim_start_matches('/')
                .trim_start_matches('*')
                .trim_end_matches("*/")
                .lines()
                .map(|line| line.trim().trim_start_matches('*').trim())
                .collect::<Vec<_>>()
                .join("\n");
            let lower = text.to_ascii_lowercase();
            let marked = lower.lines().any(|line| {
                [
                    "note:",
                    "important:",
                    "hack:",
                    "why:",
                    "rationale:",
                    "todo:",
                    "fixme:",
                ]
                .iter()
                .any(|marker| line.trim_start().starts_with(marker))
            });
            let rationale = marked
                || [
                    "rationale:",
                    "decision:",
                    "because ",
                    "trade-off",
                    "tradeoff",
                    "we chose ",
                ]
                .iter()
                .any(|m| lower.contains(m));
            let citations = citations(raw);
            let has_reference = !citations.is_empty()
                || text.contains("@see")
                || text.contains("@link")
                || text.contains("](");
            if !doc && !rationale && !has_reference {
                continue;
            }
            let mut owner = self
                .e
                .facts
                .nodes
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "module" | "class" | "function" | "method")
                        && n.metadata["start_byte"]
                            .as_u64()
                            .is_some_and(|p| p <= comment.start_byte() as u64)
                        && n.metadata["end_byte"]
                            .as_u64()
                            .is_some_and(|p| p >= comment.end_byte() as u64)
                })
                .min_by_key(|n| {
                    n.metadata["end_byte"].as_u64().unwrap_or(u64::MAX)
                        - n.metadata["start_byte"].as_u64().unwrap_or(0)
                })
                .map(|n| n.id.clone())
                .unwrap_or_else(|| self.e.scopes[0].owner.clone());
            if doc && let Some(mut next) = comment.next_named_sibling() {
                if next.kind() == "export_statement" {
                    next = next
                        .child_by_field_name("declaration")
                        .or_else(|| next.child_by_field_name("value"))
                        .unwrap_or(next);
                }
                if matches!(next.kind(), "lexical_declaration" | "variable_declaration")
                    && let Some(value) = next
                        .named_child(0)
                        .and_then(|v| v.child_by_field_name("value"))
                        .filter(|n| {
                            matches!(n.kind(), "arrow_function" | "function_expression" | "class")
                        })
                {
                    next = value;
                }
                if (self.e.source[comment.end_byte()..next.start_byte()]
                    .trim()
                    .is_empty()
                    || comment.next_named_sibling().is_some_and(|n| {
                        self.e.source[comment.end_byte()..n.start_byte()]
                            .trim()
                            .is_empty()
                    }))
                    && let Some(node) = self.e.facts.nodes.iter().find(|n| {
                        n.kind != "module"
                            && n.metadata["start_byte"].as_u64() == Some(next.start_byte() as u64)
                    })
                {
                    owner = node.id.clone();
                }
            }
            let scope = self
                .e
                .scopes
                .iter()
                .position(|s| s.owner == owner)
                .unwrap_or(0);
            let label: String = text
                .lines()
                .find(|line| !line.is_empty())
                .unwrap_or("Source documentation")
                .chars()
                .take(160)
                .collect();
            let child = self.e.define(
                comment,
                scope,
                &label,
                if rationale {
                    "rationale"
                } else {
                    "documentation"
                },
                None,
                false,
            );
            let node = self.e.facts.nodes.last_mut().unwrap();
            node.metadata["evidence"] = raw.into();
            node.metadata["provenance"] = if rationale {
                "source_rationale_marker"
            } else {
                "source_doc_comment"
            }
            .into();
            if rationale {
                self.e.facts.edges.last_mut().unwrap().relation = "explains".into();
            }
            let mut links = HashSet::new();
            for event in pulldown_cmark::Parser::new(&text) {
                if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link {
                    dest_url, ..
                }) = event
                {
                    links.insert(dest_url.into_string());
                }
            }
            for line in text.lines() {
                let mut words = line.split_whitespace();
                while let Some(word) = words.next() {
                    if matches!(word.trim_start_matches('{'), "@see" | "@link")
                        && let Some(target) = words.next()
                    {
                        links.insert(
                            target
                                .trim_matches(|c| matches!(c, '}' | '`' | '<' | '>'))
                                .to_owned(),
                        );
                    }
                }
            }
            for (start, end, canonical) in citations {
                let start = comment.start_byte() + start;
                let end = comment.start_byte() + end;
                let line = 1 + self.e.source[..start]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count() as u32;
                let target = if let Some(id) = document_refs.get(&canonical) {
                    id.clone()
                } else {
                    self.e.define(
                        comment,
                        0,
                        &canonical,
                        "doc_ref",
                        Some(format!("docref:{}:{canonical}", self.e.facts.path)),
                        false,
                    );
                    let doc = self.e.facts.nodes.last_mut().unwrap();
                    doc.line = Some(line);
                    doc.end_line = Some(line);
                    doc.metadata["start_byte"] = start.into();
                    doc.metadata["end_byte"] = end.into();
                    let column = self.e.source[..start].rsplit('\n').next().unwrap().len();
                    doc.metadata["start_column"] = column.into();
                    doc.metadata["end_column"] = (column + end - start).into();
                    doc.metadata["citation_id"] = canonical.clone().into();
                    doc.metadata["spelling"] = self.e.source[start..end].into();
                    let id = doc.id.clone();
                    let edge = self.e.facts.edges.last_mut().unwrap();
                    edge.relation = "cites".into();
                    edge.line = Some(line);
                    document_refs.insert(canonical.clone(), id.clone());
                    id
                };
                self.e.facts.edges.push(Edge {
                    id: format!("cites:{}:{start}:{canonical}", self.e.scopes[child].owner),
                    source: self.e.scopes[child].owner.clone(),
                    target,
                    relation: "cites".into(),
                    directed: true,
                    file: Some(self.e.facts.path.clone()),
                    line: Some(line),
                    confidence: "static".into(),
                    metadata: serde_json::json!({"spelling": &self.e.source[start..end]}),
                });
            }
            let mut links: Vec<_> = links.into_iter().collect();
            links.sort();
            for target in links {
                let path = target.split('#').next().unwrap_or("");
                let local = !path.contains(':')
                    && !path.starts_with('/')
                    && !path.contains('\\')
                    && matches!(
                        path.rsplit('.').next(),
                        Some("md" | "mdx" | "rst" | "adoc" | "txt")
                    );
                let keys = if local {
                    relative_path(
                        self.e.facts.path.rsplit_once('/').map_or("", |(d, _)| d),
                        path,
                    )
                    .map(|p| vec![format!("file:{p}")])
                    .unwrap_or_default()
                } else {
                    vec![]
                };
                self.e.reference(comment, child, target, "references", keys, "literal source-comment document reference; unresolved identifiers are not guessed");
            }
        }
    }
    pub(super) fn require_target<'t>(
        &self,
        node: Syntax<'t>,
    ) -> Option<(Syntax<'t>, Option<String>, Option<String>)> {
        let (call, imported) = if node.kind() == "member_expression" {
            let property = node.child_by_field_name("property")?;
            (
                node.child_by_field_name("object")?,
                Some(identifier(self.e.text(property))),
            )
        } else {
            (node, None)
        };
        if call.kind() != "call_expression" {
            return None;
        }
        let function = call.child_by_field_name("function")?;
        if function.kind() != "identifier" || identifier(self.e.text(function)) != "require" {
            return None;
        }
        let args = children(call.child_by_field_name("arguments")?)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .collect::<Vec<_>>();
        let module = if args.len() == 1 && args[0].kind() == "string" {
            self.string(args[0])
        } else {
            None
        };
        Some((call, imported, module))
    }
    pub(super) fn require_declaration(
        &mut self,
        name: Syntax<'_>,
        value: Syntax<'_>,
        binding_scope: usize,
        scope: usize,
    ) -> bool {
        let Some((_, imported, module)) = self.require_target(value) else {
            return false;
        };
        let modules = module.map(|m| self.modules(&m)).unwrap_or_default();
        let mut bindings = vec![];
        if name.kind() == "identifier" {
            bindings.push((self.e.text(name).to_owned(), imported));
        } else if name.kind() == "object_pattern" && imported.is_none() {
            for item in children(name) {
                if item.kind() == "shorthand_property_identifier_pattern" {
                    let local = self.e.text(item).to_owned();
                    bindings.push((local.clone(), Some(identifier(&local))));
                } else if item.kind() == "pair_pattern" {
                    let Some(key) = item.child_by_field_name("key") else {
                        continue;
                    };
                    let Some(value) = item
                        .child_by_field_name("value")
                        .filter(|n| n.kind() == "identifier")
                    else {
                        continue;
                    };
                    let key = match key.kind() {
                        "property_identifier" => Some(identifier(self.e.text(key))),
                        "string" => self.string(key),
                        _ => None,
                    };
                    if let Some(key) = key {
                        bindings.push((self.e.text(value).into(), Some(key)));
                    }
                }
            }
            // Defaults, rest and computed destructuring are not fixed imported symbols.
            let previous: HashSet<_> = self.e.scopes[binding_scope]
                .bindings
                .keys()
                .cloned()
                .collect();
            self.pattern(name, binding_scope, false);
            for (local, _) in &bindings {
                if previous.contains(&identifier(local)) {
                    continue;
                }
                self.e.scopes[binding_scope]
                    .bindings
                    .remove(&identifier(local));
            }
        } else {
            self.pattern(name, binding_scope, false);
            return true;
        }
        for (local, imported) in bindings {
            self.e.bind(
                binding_scope,
                &local,
                Binding::Require {
                    modules: modules.clone(),
                    imported,
                },
            );
            self.require_bindings.push((scope, binding_scope, local));
        }
        true
    }
    pub(super) fn export_value(&mut self, value: Syntax<'_>, name: String, scope: usize) {
        if matches!(
            value.kind(),
            "function_expression" | "arrow_function" | "generator_function" | "method_definition"
        ) {
            let first = self.e.facts.nodes.len();
            self.function(value, scope, None);
            let node = &mut self.e.facts.nodes[first];
            if node.label.starts_with("<anonymous@") {
                node.label = name.clone();
            }
            self.cjs_exports.push(CjsExport {
                name,
                target: ExportTarget::Node(node.id.clone()),
            });
        } else if matches!(value.kind(), "identifier" | "shorthand_property_identifier") {
            self.cjs_exports.push(CjsExport {
                name,
                target: ExportTarget::Local(identifier(self.e.text(value))),
            });
        } else {
            self.cjs_exports.push(CjsExport {
                name,
                target: ExportTarget::Local(String::new()),
            });
            self.visit(value, scope);
        }
    }
    pub(super) fn commonjs_export(&mut self, node: Syntax<'_>, scope: usize) -> bool {
        let Some(left) = node.child_by_field_name("left") else {
            return false;
        };
        let parts = self
            .dotted(left)
            .map(|p| p.iter().map(|s| identifier(s)).collect::<Vec<_>>());
        let Some(parts) = parts else {
            if left.kind() == "subscript_expression"
                && left
                    .child_by_field_name("object")
                    .and_then(|o| self.dotted(o))
                    .is_some_and(|p| p == ["exports"] || p == ["module", "exports"])
            {
                self.cjs_dynamic = true;
            }
            return false;
        };
        let (whole, name) = match parts
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["module", "exports"] => (true, "default".to_owned()),
            ["module", "exports", name] => (false, (*name).to_owned()),
            ["exports", name] => {
                if self.cjs_whole > 0 {
                    return false;
                }
                (false, (*name).to_owned())
            }
            _ => return false,
        };
        let top_level = node.parent().is_some_and(|p| {
            p.kind() == "expression_statement" && p.parent().is_some_and(|p| p.kind() == "program")
        });
        if scope != 0 || !top_level || node.kind() != "assignment_expression" {
            self.cjs_dynamic = true;
            return false;
        }
        let Some(value) = node.child_by_field_name("right") else {
            return false;
        };
        if whole {
            self.cjs_whole += 1;
            if self.cjs_whole > 1 {
                self.cjs_dynamic = true;
            }
            self.cjs_exports.clear();
            self.cjs_forward.clear();
            if value.kind() == "object" {
                for member in children(value) {
                    match member.kind() {
                        "pair" => {
                            let Some(key) = member.child_by_field_name("key") else {
                                continue;
                            };
                            let name = match key.kind() {
                                "property_identifier" => Some(identifier(self.e.text(key))),
                                "string" => self.string(key),
                                _ => None,
                            };
                            if let (Some(name), Some(value)) =
                                (name, member.child_by_field_name("value"))
                            {
                                self.export_value(value, name, scope);
                            } else {
                                self.cjs_dynamic = true;
                                self.visit(member, scope);
                            }
                        }
                        "shorthand_property_identifier" => {
                            self.export_value(member, identifier(self.e.text(member)), scope)
                        }
                        "method_definition" => {
                            if let Some(name) = member
                                .child_by_field_name("name")
                                .filter(|n| n.kind() == "property_identifier")
                            {
                                self.export_value(member, identifier(self.e.text(name)), scope);
                            } else {
                                self.cjs_dynamic = true;
                                self.visit(member, scope);
                            }
                        }
                        "comment" => {}
                        _ => {
                            self.cjs_dynamic = true;
                            self.visit(member, scope);
                        }
                    }
                }
                return true;
            }
            if let Some((_, None, Some(module))) = self.require_target(value) {
                self.cjs_forward.push(module);
                self.visit(value, scope);
                return true;
            }
        }
        self.export_value(value, name, scope);
        true
    }
}
