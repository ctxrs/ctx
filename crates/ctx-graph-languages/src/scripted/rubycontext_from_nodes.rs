use super::*;

impl RubyContext {
    pub fn from_nodes(nodes: &[Node]) -> Self {
        let mut context = Self {
            unsafe_lookup: false,
            owners: HashMap::new(),
            methods: HashMap::new(),
            method_owners: HashMap::new(),
            self_calls: HashSet::new(),
        };
        let mut roots = HashSet::new();
        let mut files = HashSet::new();
        for node in nodes.iter().filter(|n| n.metadata["language"] == "ruby") {
            files.insert(node.file.as_str());
            if node.id == format!("ruby:{}:module", node.file) {
                roots.insert(node.file.as_str());
                context.unsafe_lookup |= node.metadata["ruby_lookup_schema"] != 2
                    || node.metadata["ruby_lookup_unsafe"] != false;
                context.self_calls.extend(
                    node.metadata["ruby_self_calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|id| id.as_str().map(str::to_owned)),
                );
            }
            let Some(key) = &node.binding_key else {
                continue;
            };
            if Self::method_parts(key).is_some() {
                context.method_owners.insert(node.id.clone(), key.clone());
            }
            if let Some(owner) = key.strip_prefix("ruby:type:") {
                let entry = context
                    .owners
                    .entry(owner.into())
                    .or_insert_with(|| RubyOwner {
                        count: 0,
                        class: node.kind == "class",
                        bases: node.metadata["ruby_bases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|k| k.as_str().map(str::to_owned))
                            .collect(),
                        dynamic_base: node.metadata["ruby_dynamic_base"] != false,
                        instance_barrier: node.metadata["ruby_instance_barrier"] != false,
                        singleton_barrier: node.metadata["ruby_singleton_barrier"] != false,
                        visibility: node.metadata["ruby_visibility_overrides"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(key, value)| (key.clone(), value == "public"))
                            .collect(),
                    });
                entry.count += 1;
            }
            let aliases = node.metadata["binding_aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str());
            for key in std::iter::once(key.as_str()).chain(aliases) {
                if Self::method_parts(key).is_some() {
                    let entry = context.methods.entry(key.into()).or_insert((0, true, true));
                    entry.0 += 1;
                    entry.1 &= node.metadata["ruby_direct_method"] == true;
                    entry.2 &= node.metadata["ruby_visibility"] == "public"
                        || (node.metadata["ruby_module_function"] == true
                            && key.starts_with("ruby:singleton:"));
                }
            }
        }
        context.unsafe_lookup |= roots != files;
        context
    }
    pub(super) fn method_parts(key: &str) -> Option<(bool, &str, &str)> {
        if let Some(key) = key.strip_prefix("ruby:instance:") {
            let (owner, method) = key.rsplit_once('#')?;
            Some((false, owner, method))
        } else {
            let (owner, method) = key.strip_prefix("ruby:singleton:")?.rsplit_once('.')?;
            Some((true, owner, method))
        }
    }
    pub(super) fn select_owner<'b>(
        &self,
        keys: impl IntoIterator<Item = &'b str>,
    ) -> Option<String> {
        for key in keys {
            if self.owners.contains_key(key) {
                return Some(key.into());
            }
            // A missing nested constant must not fall through an enclosing class
            // whose inherited constants could provide a different binding.
            let mut prefix = key;
            while let Some((parent, _)) = prefix.rsplit_once("::") {
                if self.owners.get(parent).is_some_and(|o| {
                    o.count != 1 || o.dynamic_base || !o.bases.is_empty() || o.instance_barrier
                }) {
                    return None;
                }
                prefix = parent;
            }
        }
        None
    }
    /// None preserves ordinary resolution; Some is authoritative, including barriers.
    pub fn inherited_keys(&self, reference: &Reference) -> Option<Vec<String>> {
        if reference.relation == "inherits" {
            let keys = reference
                .candidate_keys
                .iter()
                .map(|key| key.strip_prefix("ruby:type:"))
                .collect::<Option<Vec<_>>>()?;
            if self.unsafe_lookup {
                return Some(vec![]);
            }
            return Some(
                self.select_owner(keys)
                    .filter(|owner| {
                        self.owners
                            .get(owner)
                            .is_some_and(|owner| owner.count == 1 && owner.class)
                    })
                    .map(|owner| vec![format!("ruby:type:{owner}")])
                    .unwrap_or_default(),
            );
        }
        if reference.relation != "calls" {
            return None;
        }
        let is_super = reference.label == "super" && reference.candidate_keys.is_empty();
        let super_key;
        let keys = if is_super {
            super_key = vec![self.method_owners.get(&reference.source)?.clone()];
            &super_key
        } else {
            &reference.candidate_keys
        };
        if keys.is_empty() {
            return None;
        }
        let parts: Vec<_> = keys
            .iter()
            .map(|key| Self::method_parts(key))
            .collect::<Option<_>>()?;
        let (singleton, _, method) = parts[0];
        if parts
            .iter()
            .any(|(kind, _, name)| *kind != singleton || *name != method)
        {
            return Some(vec![]);
        }
        if self.unsafe_lookup {
            return Some(vec![]);
        }
        let Some(mut owner) = self.select_owner(parts.iter().map(|(_, owner, _)| *owner)) else {
            return Some(vec![]);
        };
        let mut seen = HashSet::new();
        let mut visibility = None;
        let self_call = is_super || self.self_calls.contains(&reference.id);
        for depth in 0..64 {
            if !seen.insert(owner.clone()) {
                break;
            }
            let Some(class) = self.owners.get(&owner) else {
                break;
            };
            if class.count != 1
                || if singleton {
                    class.singleton_barrier
                } else {
                    class.instance_barrier
                }
            {
                break;
            }
            let key = if singleton {
                format!("ruby:singleton:{owner}.{method}")
            } else {
                format!("ruby:instance:{owner}#{method}")
            };
            if visibility.is_none() {
                visibility = class.visibility.get(&key).copied();
            }
            if let Some((count, direct, public)) = self.methods.get(&key)
                && !(is_super && depth == 0)
            {
                return Some(
                    if *count == 1 && *direct && (self_call || visibility.unwrap_or(*public)) {
                        vec![key]
                    } else {
                        vec![]
                    },
                );
            }
            if class.dynamic_base {
                break;
            }
            let Some(base) = self.select_owner(class.bases.iter().map(String::as_str)) else {
                break;
            };
            if !self.owners.get(&base).is_some_and(|owner| owner.class) {
                break;
            }
            owner = base;
        }
        Some(vec![])
    }
}
