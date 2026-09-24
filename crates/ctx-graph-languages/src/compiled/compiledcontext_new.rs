use super::*;

impl CompiledContext {
    pub fn new(files: &[FileFacts], units: &BTreeMap<String, String>) -> Self {
        let mut result = Self::default();
        let mut inventory = CompiledInventory {
            nodes: BTreeMap::new(),
            units: units.clone(),
            parents: BTreeMap::new(),
            types: BTreeMap::new(),
            bases: BTreeMap::new(),
        };
        let files: Vec<_> = files
            .iter()
            .filter(|f| {
                f.nodes.first().is_some_and(|n| {
                    matches!(
                        n.metadata["language"].as_str(),
                        Some("cpp" | "java" | "csharp" | "kotlin" | "swift")
                    )
                })
            })
            .collect();
        for f in &files {
            for n in &f.nodes {
                inventory.nodes.insert(n.id.clone(), n.clone());
            }
            for edge in &f.edges {
                if edge.relation == "contains" {
                    inventory
                        .parents
                        .insert(edge.target.clone(), edge.source.clone());
                }
            }
        }
        for n in inventory.nodes.values().filter(|n| type_node(n)) {
            inventory.bases.insert(n.id.clone(), Some(vec![]));
            for key in node_keys(n) {
                let key = context_key(&n.file, &key);
                inventory
                    .types
                    .entry((inventory.unit(n), key.clone()))
                    .or_default()
                    .push(n.id.clone());
                if key.starts_with("swift:") && key.contains(":export:") {
                    inventory
                        .types
                        .entry((String::new(), key))
                        .or_default()
                        .push(n.id.clone());
                }
            }
        }
        for group in inventory.types.values_mut() {
            group.sort();
            group.dedup();
        }
        // Publish declaration identities independently of current call sites.
        // A new caller must not require rewriting an unchanged provider file.
        for node in inventory.nodes.values() {
            let canonical_type = type_node(node)
                && node
                    .binding_key
                    .as_ref()
                    .and_then(|key| inventory.canonical(node, key))
                    .as_deref()
                    == Some(node.id.as_str());
            if canonical_type
                || (matches!(node.kind.as_str(), "method" | "declaration")
                    && node.metadata["member_accessible"] == true
                    && node.metadata["declaration_certain"] == true)
            {
                result
                    .aliases
                    .entry(node.id.clone())
                    .or_default()
                    .push(inventory.navigation_key(&node.id));
            }
        }
        let mut implementers: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        for f in &files {
            for r in &f.references {
                if !matches!(r.relation.as_str(), "inherits" | "implements") {
                    continue;
                }
                let Some(owner) = inventory.nodes.get(&r.source) else {
                    continue;
                };
                let target = r
                    .candidate_keys
                    .iter()
                    .find_map(|k| inventory.canonical(owner, k));
                if let Some(target) = target {
                    if let Some(Some(bases)) = inventory.bases.get_mut(&r.source) {
                        bases.push(target.clone());
                    }
                    let interface =
                        inventory.nodes[&target].kind == "interface" && owner.kind != "interface";
                    if interface {
                        result.relations.insert(r.id.clone(), "implements".into());
                        implementers
                            .entry(target)
                            .or_default()
                            .insert(r.source.clone());
                    }
                } else {
                    inventory.bases.insert(r.source.clone(), None);
                }
            }
        }
        for group in inventory.types.values() {
            if group.len() > 1
                && group
                    .iter()
                    .all(|id| inventory.nodes[id].metadata["partial"] == true)
            {
                let primary = &group[0];
                for part in &group[1..] {
                    result.link(
                        &inventory,
                        part,
                        primary,
                        "partial_of",
                        "explicit partial declarations in one proven compilation unit",
                    );
                }
            }
        }
        // An extension's access defaults are useful only after its target type
        // and module have been proved, not from the extended spelling alone.
        for f in &files {
            for r in f.references.iter().filter(|r| r.relation == "extends") {
                let Some(extension) = inventory
                    .nodes
                    .get(&r.source)
                    .filter(|n| n.kind == "extension")
                else {
                    continue;
                };
                let Some(target) = r
                    .candidate_keys
                    .iter()
                    .find_map(|k| inventory.canonical(extension, k))
                else {
                    continue;
                };
                let target = &inventory.nodes[&target];
                if target.metadata["swift_exported"] != true
                    || extension.metadata["swift_module"].as_str().is_none()
                    || extension.metadata["swift_module"] != target.metadata["swift_module"]
                {
                    continue;
                }
                for member in inventory
                    .nodes
                    .values()
                    .filter(|n| inventory.parents.get(&n.id) == Some(&extension.id))
                {
                    if member.metadata["dynamic_dispatch"] != false
                        || member.metadata["member_accessible"] != true
                        || member.metadata["declaration_certain"] != true
                        || !(member.metadata["explicit_public"] == true
                            || (member.metadata["explicit_access"].is_null()
                                && extension.metadata["explicit_public"] == true))
                    {
                        continue;
                    }
                    for key in node_keys(member) {
                        if let Some((kind, symbol)) =
                            key.strip_prefix("swift:").and_then(|k| k.split_once(':'))
                            && let Some(symbol) = symbol.strip_prefix(&format!(
                                "module:{}:{}:",
                                member.metadata["swift_module"].as_str().unwrap().len(),
                                member.metadata["swift_module"].as_str().unwrap()
                            ))
                        {
                            result.aliases.entry(member.id.clone()).or_default().push(
                                swift_export_key(
                                    kind,
                                    member.metadata["swift_module"].as_str().unwrap(),
                                    symbol,
                                ),
                            );
                        }
                    }
                }
            }
        }
        for f in &files {
            for r in &f.references {
                let Some(source) = inventory.nodes.get(&r.source) else {
                    continue;
                };
                if r.relation == "calls" {
                    for key in &r.candidate_keys {
                        if let Some(target) = inventory.canonical(source, key)
                            && inventory.group(&target).len() > 1
                        {
                            let candidate = inventory.navigation_key(&target);
                            result
                                .aliases
                                .entry(target)
                                .or_default()
                                .push(candidate.clone());
                            result.candidates.insert(r.id.clone(), vec![candidate]);
                            break;
                        }
                        let key = context_key(&f.path, key);
                        let Some((owner, name)) = key.rsplit_once('.') else {
                            continue;
                        };
                        let static_call = owner.contains(":static:");
                        let base_call = owner.contains(":base:");
                        let type_key = owner
                            .replacen(":member:", ":symbol:", 1)
                            .replacen(":base:", ":symbol:", 1)
                            .replacen(":static:", ":symbol:", 1);
                        let kotlin_static = static_call && key.starts_with("kotlin:");
                        let cpp_qualified = r.label.contains("::")
                            && matches!(
                                f.nodes
                                    .first()
                                    .and_then(|n| n.metadata["language"].as_str()),
                                Some("cpp")
                            );
                        let type_key = if cpp_qualified {
                            type_key.replacen(":header-static:", ":header-symbol:", 1)
                        } else {
                            type_key
                        };
                        let Some(owner) = inventory.canonical(source, &type_key) else {
                            // A duplicate receiver must not fall through to a unique
                            // method key (one duplicate may lack that method).
                            if (kotlin_static || cpp_qualified)
                                && inventory
                                    .types
                                    .contains_key(&(inventory.unit(source), type_key))
                            {
                                result.candidates.insert(r.id.clone(), vec![]);
                                break;
                            }
                            continue;
                        };
                        // Keep qualified-only C++ definitions usable when the header
                        // supplied no member declaration. A known declaration, however,
                        // must enforce its access and dispatch evidence before fallback.
                        if kotlin_static
                            || (cpp_qualified
                                && inventory.nodes.values().any(|n| {
                                    n.label == name
                                        && inventory
                                            .parents
                                            .get(&n.id)
                                            .is_some_and(|p| inventory.group(&owner).contains(p))
                                }))
                        {
                            result.candidates.insert(r.id.clone(), vec![]);
                        }
                        let target = if base_call {
                            inventory
                                .bases
                                .get(&owner)
                                .and_then(Option::as_ref)
                                .filter(|bases| bases.len() == 1)
                                .and_then(|bases| {
                                    inventory.member(&bases[0], name, false, &mut HashSet::new())
                                })
                        } else {
                            inventory.member(&owner, name, static_call, &mut HashSet::new())
                        };
                        let Some(target) = target else {
                            // Keep declaration evidence separate from callable candidates.
                            // base/super calls retain their existing single-base boundary.
                            let declaration_owner = if base_call {
                                inventory
                                    .bases
                                    .get(&owner)
                                    .and_then(Option::as_ref)
                                    .filter(|bases| bases.len() == 1)
                                    .map(|bases| bases[0].as_str())
                            } else {
                                Some(owner.as_str())
                            };
                            if let Some(targets) = declaration_owner.and_then(|owner| {
                                inventory.declared_members(
                                    owner,
                                    name,
                                    static_call,
                                    &mut HashSet::new(),
                                )
                            }) {
                                for target in targets {
                                    let node = &inventory.nodes[&target];
                                    if node.metadata["swift_module"].is_string()
                                        && node.metadata["swift_module"]
                                            != source.metadata["swift_module"]
                                        && node.metadata["swift_exported"] != true
                                    {
                                        continue;
                                    }
                                    result.link_reference(
                                        &inventory,
                                        r,
                                        &target,
                                        "declared_member",
                                        "accessible declaration on a proven receiver-type branch; no overload selection or runtime dispatch claim",
                                    );
                                }
                            }
                            continue;
                        };
                        let node = &inventory.nodes[&target];
                        if node.metadata["swift_module"].is_string()
                            && node.metadata["swift_module"] != source.metadata["swift_module"]
                            && node.metadata["swift_exported"] != true
                        {
                            continue;
                        }
                        result.link_reference(&inventory,r,&target,"declared_member","written receiver type identifies this declaration; runtime dispatch is unresolved when virtual");
                        if node.metadata["dynamic_dispatch"] == false && node.binding_key.is_some()
                        {
                            let candidate = inventory.navigation_key(&target);
                            result
                                .aliases
                                .entry(target.clone())
                                .or_default()
                                .push(candidate.clone());
                            result.candidates.insert(r.id.clone(), vec![candidate]);
                        }
                        break;
                    }
                } else if !matches!(r.relation.as_str(), "declared_member" | "partial_of") {
                    for key in &r.candidate_keys {
                        if let Some(target) = inventory.canonical(source, key)
                            && inventory.group(&target).len() > 1
                        {
                            let candidate = inventory.navigation_key(&target);
                            result
                                .aliases
                                .entry(target)
                                .or_default()
                                .push(candidate.clone());
                            result.candidates.insert(r.id.clone(), vec![candidate]);
                            break;
                        }
                    }
                }
            }
        }
        for (interface, implementers) in implementers {
            if !inventory.nodes[&interface].id.starts_with("csharp:") || implementers.len() != 1 {
                continue;
            }
            let implementer = implementers.into_iter().next().unwrap();
            for method in inventory
                .nodes
                .values()
                .filter(|n| inventory.parents.get(&n.id) == Some(&interface) && n.kind == "method")
            {
                if let Some(target) =
                    inventory.member(&implementer, &method.label, false, &mut HashSet::new())
                    && inventory.parents.get(&target) != Some(&interface)
                {
                    result.link(&inventory,&method.id,&target,"implemented_by","one declared implementation in this compilation unit; not a runtime dispatch guarantee");
                }
            }
        }
        let mut proof = vec![];
        for f in &files {
            let mut nodes:Vec<_>=f.nodes.iter().map(|n|json!({"kind":n.kind,"name":n.qualified_name,"keys":node_keys(n),"parent":inventory.parents.get(&n.id).and_then(|id|inventory.nodes.get(id)).and_then(|n|n.qualified_name.as_ref()),"partial":n.metadata["partial"],"public":n.metadata["cross_project_public"],"explicit_public":n.metadata["explicit_public"],"explicit_access":n.metadata["explicit_access"],"certain":n.metadata["declaration_certain"],"parameterless":n.metadata["parameterless"],"accessible":n.metadata["member_accessible"],"dynamic":n.metadata["dynamic_dispatch"],"static":n.metadata["static"],"companion":n.metadata["companion"],"swift_module":n.metadata["swift_module"],"swift_exported":n.metadata["swift_exported"]})).collect();
            nodes.sort_by_key(serde_json::Value::to_string);
            let mut relations:Vec<_>=f.references.iter().filter(|r|matches!(r.relation.as_str(),"inherits"|"implements"|"extends"|"delegates_to")).map(|r|json!({"owner":inventory.nodes.get(&r.source).and_then(|n|n.qualified_name.as_ref()),"relation":r.relation,"keys":r.candidate_keys})).collect();
            relations.sort_by_key(serde_json::Value::to_string);
            proof.push(json!({"file":f.path,"unit":units.get(&f.path),"nodes":nodes,"relations":relations}));
        }
        proof.sort_by_key(serde_json::Value::to_string);
        result.fingerprint = blake3::hash(serde_json::to_string(&proof).unwrap().as_bytes())
            .to_hex()
            .to_string();
        for aliases in result.aliases.values_mut() {
            aliases.sort();
            aliases.dedup();
        }
        for refs in result.references.values_mut() {
            refs.sort_by(|a, b| a.id.cmp(&b.id));
            refs.dedup_by(|a, b| a.id == b.id);
        }
        result
    }
    pub(super) fn link(
        &mut self,
        inventory: &CompiledInventory,
        source: &str,
        target: &str,
        relation: &str,
        reason: &str,
    ) {
        let node = &inventory.nodes[source];
        let reference = crate::model::Reference {
            id: format!("compiled:{relation}:{source}:{target}"),
            source: source.into(),
            label: inventory.nodes[target].label.clone(),
            relation: relation.into(),
            file: node.file.clone(),
            line: node.line.unwrap_or(1),
            candidate_keys: vec![],
            reason: reason.into(),
        };
        self.link_reference(inventory, &reference, target, relation, reason);
    }
    pub(super) fn link_reference(
        &mut self,
        inventory: &CompiledInventory,
        original: &crate::model::Reference,
        target: &str,
        relation: &str,
        reason: &str,
    ) {
        let key = inventory.navigation_key(target);
        self.aliases
            .entry(target.into())
            .or_default()
            .push(key.clone());
        let mut reference = original.clone();
        reference.id = format!("compiled:{relation}:{}:{target}", original.id);
        reference.relation = relation.into();
        reference.candidate_keys = vec![key];
        reference.reason = reason.into();
        self.references
            .entry(reference.file.clone())
            .or_default()
            .push(reference);
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn apply(&self, facts: &mut FileFacts) {
        for node in &mut facts.nodes {
            if let Some(extra) = self.aliases.get(&node.id) {
                let mut aliases: Vec<String> = node.metadata["binding_aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                aliases.extend(extra.iter().cloned());
                aliases.sort();
                aliases.dedup();
                node.metadata["binding_aliases"] = json!(aliases);
            }
        }
        for reference in &mut facts.references {
            if let Some(keys) = self.candidates.get(&reference.id) {
                reference.candidate_keys = keys.clone();
            }
            if let Some(relation) = self.relations.get(&reference.id) {
                reference.relation = relation.clone();
            }
        }
        if let Some(extra) = self.references.get(&facts.path) {
            let old: HashSet<_> = facts.references.iter().map(|r| r.id.clone()).collect();
            facts
                .references
                .extend(extra.iter().filter(|r| !old.contains(&r.id)).cloned());
        }
        for node in &mut facts.nodes {
            if let Some(types) = node
                .metadata
                .get_mut("type_references")
                .and_then(serde_json::Value::as_array_mut)
            {
                for evidence in types {
                    if let Some(r) = facts
                        .references
                        .iter()
                        .find(|r| evidence["reference_id"] == r.id)
                    {
                        evidence["candidate_keys"] = json!(r.candidate_keys);
                    }
                }
            }
        }
    }
}
