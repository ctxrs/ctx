use super::*;

impl PythonContext {
    pub fn from_facts(facts: &[FileFacts]) -> Self {
        let mut context = Self::default();
        let mut inventory: Vec<_> = facts.iter().collect();
        inventory.sort_by(|a, b| a.path.cmp(&b.path));
        for facts in inventory {
            // Failed Python parses still reserve module identity, so a second
            // root's same module cannot silently win over an invalid source.
            let root = facts
                .nodes
                .iter()
                .find(|n| n.kind == "module" && n.id.starts_with("python:"));
            if root.is_none() && !facts.path.ends_with(".py") {
                continue;
            }
            let mut bindings = BTreeSet::new();
            for node in &facts.nodes {
                if node.kind == "class"
                    && let Some(name) = &node.qualified_name
                {
                    context
                        .classes
                        .entry(format!("python:{}:{name}", facts.module))
                        .or_default()
                        .push(PythonClass {
                            bases: serde_json::from_value(node.metadata["python_bases"].clone())
                                .unwrap_or_default(),
                            members: serde_json::from_value(
                                node.metadata["python_members"].clone(),
                            )
                            .unwrap_or_default(),
                            uncertain: python_definition_key(node).is_none()
                                || node.metadata["python_literal_bases"] != true
                                || node.metadata["python_class_uncertain"] == true,
                            receiver_writes: serde_json::from_value(
                                node.metadata["python_receiver_writes"].clone(),
                            )
                            .unwrap_or_default(),
                        });
                }
                bindings.extend(python_definition_key(node).map(str::to_owned));
                for aliases in [
                    node.metadata.get("binding_aliases"),
                    node.metadata["python_pending_binding"].get("aliases"),
                ] {
                    if node.kind == "method" {
                        context.methods.extend(
                            aliases
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned),
                        );
                    }
                    bindings.extend(
                        aliases
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned),
                    );
                }
            }
            let metadata = root.map_or(&Value::Null, |n| &n.metadata);
            let exports = metadata
                .get("python_exports")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, entry)| {
                    Some((name.clone(), entry.get("target")?.as_str()?.to_owned()))
                })
                .collect();
            let blocked = metadata
                .get("python_blocked_exports")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            context
                .modules
                .entry(facts.module.clone())
                .or_default()
                .push(PythonModule {
                    exports,
                    definitions: serde_json::from_value(metadata["python_definitions"].clone())
                        .unwrap_or_default(),
                    stars: serde_json::from_value(metadata["python_stars"].clone())
                        .unwrap_or_default(),
                    all: metadata["python_all"].clone(),
                    blocked,
                    uncertain: root.is_none()
                        || metadata
                            .get("python_uncertain")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                });
            context.bindings.extend(bindings);
        }
        // Keep even invalid descendant hierarchies: a conflicting C3 order
        // must not certify calls on an ancestor's implicit receiver.
        for (child, variants) in &context.classes {
            for info in variants {
                for base in info.bases.iter().flatten() {
                    if let Some((base, _)) = context.resolve(base, &mut BTreeSet::new())
                        && context.classes.contains_key(&base)
                    {
                        context
                            .children
                            .entry(base)
                            .or_default()
                            .insert(child.clone());
                    }
                }
            }
        }
        // Stamp binding context, not reference usage or source locations.
        // Ordinary terminal definitions/body edits keep per-file parsing; Store
        // already rebinds references when those defining keys change.
        let mut hash = blake3::Hasher::new();
        hash.update(b"python-import-context-7");
        for (module, files) in &context.modules {
            if files.len() == 1 && files[0].exports.is_empty() && files[0].stars.is_empty() {
                continue;
            }
            let mut variants: Vec<_> = files
                .iter()
                .map(|file| {
                    let resolved: BTreeMap<_, _> = file
                        .exports
                        .keys()
                        .map(|name| {
                            (
                                name,
                                context.resolve(
                                    &format!("python:{module}:{name}"),
                                    &mut BTreeSet::new(),
                                ),
                            )
                        })
                        .collect();
                    let stars = (!file.stars.is_empty()).then(|| {
                        context
                            .star_bindings(
                                file,
                                &mut BTreeSet::from([module.clone()]),
                                &mut BTreeMap::new(),
                            )
                            .map(|bindings| {
                                bindings
                                    .into_iter()
                                    .map(|(name, target)| {
                                        let resolved = target.and_then(|_| {
                                            context.resolve(
                                                &format!("python:{module}:{name}"),
                                                &mut BTreeSet::new(),
                                            )
                                        });
                                        (name, resolved)
                                    })
                                    .collect::<BTreeMap<_, _>>()
                            })
                    });
                    json!([file.exports, resolved, file.stars, stars]).to_string()
                })
                .collect();
            variants.sort();
            let input = json!([module, variants]).to_string();
            hash.update(&(input.len() as u64).to_le_bytes());
            hash.update(input.as_bytes());
        }
        // Fingerprint the available member routes, never their call sites.
        // Adding/removing an inherited call is only a body edit. Inherited
        // endpoints and suppressed aliases still need consumer refresh when the
        // binding context changes; unchanged terminal keys rebind in Store.
        let mut member_keys: BTreeSet<_> = context
            .bindings
            .iter()
            .filter(|key| key.starts_with("python-member:"))
            .cloned()
            .collect();
        let mut mros = BTreeMap::new();
        for class in context.classes.keys() {
            let mro = context.mro(class, &mut BTreeSet::new(), &mut mros);
            // Unknown/changed bases can change a cleared lookup into a direct
            // key even without inherited methods. Ordinary root classes still
            // use Store's terminal-key addition/deletion handling.
            let root_class = mro.as_ref().is_some_and(|order| {
                order.len() == 2 && order[0] == *class && order[1] == "python-builtin:object"
            });
            if !root_class {
                let input = json!(["class", class, mro]).to_string();
                hash.update(&(input.len() as u64).to_le_bytes());
                hash.update(input.as_bytes());
            }
            if let Some(mro) = mro {
                for ancestor in mro {
                    for info in context.classes.get(&ancestor).into_iter().flatten() {
                        for name in info.members.keys() {
                            member_keys.insert(format!(
                                "python-member:{}.{name}",
                                class.strip_prefix("python:").unwrap()
                            ));
                        }
                    }
                }
            }
        }
        for key in member_keys {
            let resolved = context
                .resolve(&key, &mut BTreeSet::new())
                .map(|(key, _)| key);
            if resolved.as_deref() != Some(&key) {
                let input = json!([key, resolved]).to_string();
                hash.update(&(input.len() as u64).to_le_bytes());
                hash.update(input.as_bytes());
            }
            if let Some((class, member)) = key
                .strip_prefix("python-member:")
                .and_then(|k| k.rsplit_once('.'))
            {
                let class = format!("python:{class}");
                if context.classes.contains_key(&class) {
                    let receiver = context.receiver_target(&class, member, false);
                    let super_target = context.receiver_target(&class, member, true);
                    // Only exceptions to ordinary member lookup and available
                    // super routes affect consumers. Never hash receiver sites,
                    // method bodies, byte offsets, or ordinary root definitions.
                    if receiver != resolved || super_target.is_some() {
                        let input = json!(["receiver", key, receiver, super_target]).to_string();
                        hash.update(&(input.len() as u64).to_le_bytes());
                        hash.update(input.as_bytes());
                    }
                }
            }
        }
        context.fingerprint = hash.finalize().to_hex().to_string();
        context
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Publish verified star-dependent bindings and rewrite references to their
    /// defining keys; native IDs and ownership never move to a forwarding module.
    /// Reparse callers when the context fingerprint changes, including on export
    /// deletion or ambiguity.
    pub fn apply(&self, facts: &mut FileFacts) {
        if !facts
            .nodes
            .iter()
            .any(|n| n.kind == "module" && n.id.starts_with("python:"))
        {
            return;
        }
        for node in &mut facts.nodes {
            let Some(pending) = node.metadata.get("python_pending_binding").cloned() else {
                continue;
            };
            let publish = pending["name"]
                .as_str()
                .is_some_and(|name| self.unshadowed_binding(&facts.module, name));
            node.binding_key = if publish {
                pending["key"].as_str().map(str::to_owned)
            } else {
                None
            };
            let metadata = node.metadata.as_object_mut().unwrap();
            metadata.remove("binding_aliases");
            if publish
                && pending["aliases"]
                    .as_array()
                    .is_some_and(|aliases| !aliases.is_empty())
            {
                metadata.insert("binding_aliases".into(), pending["aliases"].clone());
            }
        }
        let mut expanded = Vec::new();
        for reference in &facts.references {
            if reference.relation != "imports" || reference.label != "*" {
                continue;
            }
            let Some(module) = reference
                .candidate_keys
                .first()
                .and_then(|k| k.strip_prefix("module:"))
            else {
                continue;
            };
            if let Some(exports) =
                self.star_exports(module, &mut BTreeSet::new(), &mut BTreeMap::new())
            {
                for (name, target) in exports {
                    let Some(target) = target else { continue };
                    let mut symbol = reference.clone();
                    symbol.id = format!("{}:{name}", symbol.id);
                    symbol.label = name;
                    symbol.candidate_keys = vec![target];
                    if !facts.references.iter().any(|r| r.id == symbol.id) {
                        expanded.push(symbol);
                    }
                }
            }
        }
        // A subclass can override an implicit receiver's target. Keep the
        // declaration reachable without claiming that it is the runtime callee.
        for reference in &facts.references {
            if reference.relation != "calls" {
                continue;
            }
            let Some(key) = reference.candidate_keys.first() else {
                continue;
            };
            let Some((class, member)) = key
                .strip_prefix("python-receiver:")
                .and_then(|rest| rest.rsplit_once('.'))
            else {
                continue;
            };
            let class = format!("python:{class}");
            if self.receiver_target(&class, member, false).is_some() {
                continue;
            }
            let Some(mro) = self.mro(&class, &mut BTreeSet::new(), &mut BTreeMap::new()) else {
                continue;
            };
            let Some(target) = self
                .receiver_in_mro(&mro, &class, member, false)
                .filter(|target| self.methods.contains(target))
            else {
                continue;
            };
            let mut declaration = reference.clone();
            declaration.id.push_str(":declared_member");
            declaration.relation = "declared_member".into();
            declaration.candidate_keys = vec![target];
            declaration.reason =
                "receiver declaration; subclass dispatch remains unresolved".into();
            if !facts.references.iter().any(|r| r.id == declaration.id) {
                expanded.push(declaration);
            }
        }
        facts.references.extend(expanded);
        for reference in &mut facts.references {
            let mut keys = Vec::new();
            for key in &reference.candidate_keys {
                match self.resolve(key, &mut BTreeSet::new()) {
                    Some((target, claimed)) => {
                        // Calling an imported module is not a statically known
                        // function call, even when its module identity is known.
                        if (reference.relation != "calls" || !target.starts_with("module:"))
                            && !keys.contains(&target)
                        {
                            keys.push(target);
                        }
                        if claimed {
                            break;
                        }
                    }
                    None => {
                        keys.clear();
                        break;
                    }
                }
            }
            reference.candidate_keys = keys;
        }
    }

    // The bool means an explicit binding owns this name, preventing a missing
    // named re-export from falling through to an unrelated same-named submodule.
    pub(super) fn resolve(&self, key: &str, seen: &mut BTreeSet<String>) -> Option<(String, bool)> {
        if seen.len() >= 64 || !seen.insert(key.to_owned()) {
            return None;
        }
        if let Some(rest) = key
            .strip_prefix("python-receiver:")
            .or_else(|| key.strip_prefix("python-super:"))
        {
            let (class, member) = rest.rsplit_once('.')?;
            return self
                .receiver_target(
                    &format!("python:{class}"),
                    member,
                    key.starts_with("python-super:"),
                )
                .map(|target| (target, true));
        }
        if let Some(local) = key.strip_prefix("python-local:") {
            let (module, local) = local.split_once(':')?;
            let (name, target) = local.split_once(':')?;
            if !self.unshadowed_binding(module, name) {
                return None;
            }
            return self.resolve(target, seen);
        }
        if let Some(module) = key.strip_prefix("module:") {
            if self
                .modules
                .get(module)
                .is_some_and(|files| files.len() != 1)
            {
                return None;
            }
            return Some((key.into(), self.bindings.contains(key)));
        }
        let (member, rest) = if let Some(rest) = key.strip_prefix("python-member:") {
            (true, rest)
        } else if let Some(rest) = key.strip_prefix("python:") {
            (false, rest)
        } else {
            return Some((key.into(), false));
        };
        let Some((module, name)) = rest.split_once(':') else {
            return Some((key.into(), false));
        };
        let (head, suffix) = name
            .split_once('.')
            .map_or((name, None), |(head, tail)| (head, Some(tail)));
        let mut known_name = false;
        if let Some(files) = self.modules.get(module) {
            if files.len() != 1 {
                return None;
            }
            let info = &files[0];
            if info.uncertain || info.blocked.contains(head) {
                // A terminal name keeps its original lookup key. The Store
                // removes/rebinds that definition when this module changes,
                // without reparsing unrelated callers. Claim the name to stop
                // an invalid named import falling back to a sibling submodule.
                let missing_member = member
                    && !self.bindings.contains(key)
                    && name.rsplit_once('.').is_some_and(|(class, _)| {
                        !self
                            .classes
                            .contains_key(&format!("python:{module}:{class}"))
                    });
                return ((!member && suffix.is_none()) || missing_member)
                    .then(|| (key.into(), true));
            }
            let stars = self.star_bindings(
                info,
                &mut BTreeSet::from([module.to_owned()]),
                &mut BTreeMap::new(),
            )?;
            let star = stars.get(head);
            if star.is_some()
                && (info.exports.contains_key(head) || info.definitions.contains_key(head))
            {
                return None;
            }
            let target = match star {
                Some(target) => Some(target.as_ref()?),
                None => info.exports.get(head),
            };
            if let Some(target) = target {
                let forwarded = match (target.strip_prefix("module:"), suffix) {
                    (Some(module), Some(tail)) => Self::module_member(module, tail),
                    (Some(_), None) => target.clone(),
                    (None, Some(tail)) => {
                        let symbol = target.strip_prefix("python:")?;
                        format!(
                            "{}:{symbol}.{tail}",
                            if member { "python-member" } else { "python" }
                        )
                    }
                    (None, None) => target.clone(),
                };
                return self.resolve(&forwarded, seen).map(|(key, _)| (key, true));
            }
            known_name = self.bindings.contains(&format!("python:{module}:{head}"));
            if !info.stars.is_empty() && !known_name {
                return None;
            }
        }
        if member && let Some((class, method)) = name.rsplit_once('.') {
            let class_key = format!("python:{module}:{class}");
            if self.classes.contains_key(&class_key) {
                let mro = self.mro(&class_key, &mut BTreeSet::new(), &mut BTreeMap::new())?;
                for class in mro {
                    if class == "python-builtin:object" {
                        continue;
                    }
                    let info = self.classes.get(&class)?.first()?;
                    if let Some(target) = info.members.get(method) {
                        let target = target.as_ref()?;
                        let alias = format!("python-member:{}", target.strip_prefix("python:")?);
                        // A same-spelled submodule alias cannot prove an inherited member.
                        if alias != key && self.bindings.contains(key) {
                            return None;
                        }
                        return self.bindings.contains(&alias).then_some((alias, true));
                    }
                }
                // Preserve an unavailable terminal lookup for Store to bind if
                // the method is added later. A submodule alias with that spelling
                // cannot stand in for the verified class's missing member.
                return (!self.bindings.contains(key)).then(|| (key.into(), true));
            }
        }
        if self.bindings.contains(key) || known_name {
            return Some((key.into(), true));
        }
        // `from package import child` can import a visible submodule, including
        // namespace packages. Never select one when a named binding owns child.
        let submodule = format!("{module}.{head}");
        if self.modules.contains_key(&submodule) {
            let target = match suffix {
                Some(tail) => Self::module_member(&submodule, tail),
                None => format!("module:{submodule}"),
            };
            return self.resolve(&target, seen).map(|(key, _)| (key, true));
        }
        Some((key.into(), false))
    }

    pub(super) fn receiver_target(
        &self,
        class: &str,
        member: &str,
        super_call: bool,
    ) -> Option<String> {
        let mut cache = BTreeMap::new();
        let mro = self.mro(class, &mut BTreeSet::new(), &mut cache)?;
        let target = self.receiver_in_mro(&mro, class, member, super_call)?;
        let mut pending: Vec<_> = self.children.get(class).into_iter().flatten().collect();
        let mut seen = BTreeSet::new();
        while let Some(child) = pending.pop() {
            if !seen.insert(child) {
                continue;
            }
            let mro = self.mro(child, &mut BTreeSet::new(), &mut cache)?;
            if self
                .receiver_in_mro(&mro, class, member, super_call)
                .as_ref()
                != Some(&target)
            {
                return None;
            }
            pending.extend(self.children.get(child).into_iter().flatten());
        }
        Some(target)
    }

    pub(super) fn receiver_in_mro(
        &self,
        mro: &[String],
        class: &str,
        member: &str,
        super_call: bool,
    ) -> Option<String> {
        for ancestor in mro {
            for info in self.classes.get(ancestor).into_iter().flatten() {
                if info.receiver_writes.contains("__class__")
                    || info.receiver_writes.contains("*")
                    || (!super_call
                        && (info.receiver_writes.contains(member)
                            || info.members.contains_key("__getattribute__")
                            || info.members.contains_key("__getattr__")))
                {
                    return None;
                }
            }
        }
        let start = if super_call {
            mro.iter().position(|key| key == class)? + 1
        } else {
            0
        };
        for ancestor in &mro[start..] {
            if ancestor == "python-builtin:object" {
                continue;
            }
            let info = self.classes.get(ancestor)?.first()?;
            if let Some(target) = info.members.get(member) {
                let alias = format!(
                    "python-member:{}",
                    target.as_ref()?.strip_prefix("python:")?
                );
                return self.methods.contains(&alias).then_some(alias);
            }
        }
        // A missing ordinary member keeps a terminal key so Store can bind an
        // added declaration without reparsing all unrelated Python files. An
        // existing alias cannot establish a member absent from this MRO.
        let key = format!(
            "python-member:{}.{}",
            class.strip_prefix("python:").unwrap(),
            member
        );
        (!super_call && !self.bindings.contains(&key)).then_some(key)
    }

    pub(super) fn unshadowed_binding(&self, module: &str, name: &str) -> bool {
        let Some(files) = self.modules.get(module) else {
            return false;
        };
        files.len() == 1
            && !files[0].uncertain
            && self
                .star_bindings(
                    &files[0],
                    &mut BTreeSet::from([module.to_owned()]),
                    &mut BTreeMap::new(),
                )
                .is_some_and(|stars| !stars.contains_key(name))
    }

    // Keep blocked names in the map: absence and an explicitly unknown export
    // have different meanings when a consumer also has other star imports.
    pub(super) fn star_bindings(
        &self,
        info: &PythonModule,
        seen: &mut BTreeSet<String>,
        cache: &mut BTreeMap<String, Option<PythonExports>>,
    ) -> Option<PythonExports> {
        let mut names = BTreeMap::new();
        for module in &info.stars {
            for (name, target) in self.star_exports(module, seen, cache)? {
                names
                    .entry(name)
                    .and_modify(|value| *value = None)
                    .or_insert(target);
            }
        }
        Some(names)
    }

    pub(super) fn star_exports(
        &self,
        module: &str,
        seen: &mut BTreeSet<String>,
        cache: &mut BTreeMap<String, Option<PythonExports>>,
    ) -> Option<PythonExports> {
        if let Some(result) = cache.get(module) {
            return result.clone();
        }
        if seen.len() >= 64 || !seen.insert(module.to_owned()) {
            return None;
        }
        let result = (|| {
            let files = self.modules.get(module)?;
            if files.len() != 1 {
                return None;
            }
            let info = &files[0];
            if info.uncertain || info.all == false {
                return None;
            }
            let mut names = self.star_bindings(info, seen, cache)?;
            for (name, target) in info.definitions.iter().chain(&info.exports) {
                names
                    .entry(name.clone())
                    .and_modify(|value| *value = None)
                    .or_insert_with(|| Some(target.clone()));
            }
            for name in &info.blocked {
                names.insert(name.clone(), None);
            }
            if let Some(all) = info.all.as_array() {
                Some(
                    all.iter()
                        .filter_map(Value::as_str)
                        .map(|name| (name.to_owned(), names.get(name).cloned().flatten()))
                        .collect(),
                )
            } else {
                names.retain(|name, _| !name.starts_with('_'));
                Some(names)
            }
        })();
        seen.remove(module);
        cache.insert(module.to_owned(), result.clone());
        result
    }

    // C3 linearization: never select a convenient base when the written order
    // is inconsistent, cyclic, incomplete, or depends on a custom metaclass.
    pub(super) fn mro(
        &self,
        key: &str,
        seen: &mut BTreeSet<String>,
        cache: &mut BTreeMap<String, Option<Vec<String>>>,
    ) -> Option<Vec<String>> {
        if let Some(result) = cache.get(key) {
            return result.clone();
        }
        if key == "python-builtin:object" {
            return Some(vec![key.to_owned()]);
        }
        if seen.len() >= 64 || !seen.insert(key.to_owned()) {
            return None;
        }
        let result = (|| {
            let (resolved, _) = self.resolve(key, &mut BTreeSet::new())?;
            if resolved != key {
                return self.mro(&resolved, seen, cache);
            }
            let classes = self.classes.get(key)?;
            if classes.len() != 1 || classes[0].uncertain {
                return None;
            }
            let info = &classes[0];
            let mut bases = Vec::new();
            let mut sequences = Vec::new();
            for base in &info.bases {
                let (base, _) = self.resolve(base.as_ref()?, &mut BTreeSet::new())?;
                if bases.contains(&base) {
                    return None;
                }
                sequences.push(self.mro(&base, seen, cache)?);
                bases.push(base);
            }
            if bases.is_empty() {
                bases.push("python-builtin:object".into());
            }
            sequences.push(bases);
            let mut result = vec![key.to_owned()];
            while sequences.iter().any(|s| !s.is_empty()) {
                let head = sequences
                    .iter()
                    .filter_map(|s| s.first())
                    .find(|candidate| {
                        sequences
                            .iter()
                            .all(|s| !s.iter().skip(1).any(|n| n == *candidate))
                    })?
                    .clone();
                result.push(head.clone());
                for sequence in &mut sequences {
                    if sequence.first() == Some(&head) {
                        sequence.remove(0);
                    }
                }
            }
            Some(result)
        })();
        seen.remove(key);
        cache.insert(key.to_owned(), result.clone());
        result
    }
}
