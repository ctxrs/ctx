use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyAppendTrustContract {
    StrictPrefixAuthentication,
    /// Provider-owned rollout files retain one trusted object and promise only
    /// append growth. This contract is versioned so broadening it requires an
    /// explicit compatibility decision.
    AppendOnlySameObjectV1,
}

/// Content-free physical membership observed at admission or at the terminal
/// fence. Source hints are optional and are used only to recognize a deleted
/// logical source that reappears at a new physical route under frozen mode.
#[derive(Debug)]
pub struct JsonlFamilyMembershipObservation<E: JsonlFamilyError> {
    root_missing: bool,
    routes: BTreeMap<PathBuf, JsonlFamilyMembershipRoute<E>>,
    source_hints: HashMap<PathBuf, SourceKey>,
}

#[derive(Debug)]
struct JsonlFamilyMembershipRoute<E: JsonlFamilyError> {
    authority: Arc<ProviderSourceRoot<E>>,
    authority_path: PathBuf,
}

type UnobservedMembershipRoutes = BTreeMap<PathBuf, BTreeSet<PathBuf>>;

impl<E: JsonlFamilyError> JsonlFamilyMembershipObservation<E> {
    pub fn observe(root: &Path, opening: &JsonlFamilyInventory<E>) -> JsonlResult<Self, E> {
        if opening.root_missing {
            return match open_provider_source_path::<E>(root) {
                Err(error) if error.is_not_found() => Ok(Self {
                    root_missing: true,
                    routes: BTreeMap::new(),
                    source_hints: HashMap::new(),
                }),
                Ok(_) => Err(E::source_changed()),
                Err(error) => Err(error),
            };
        }

        let absolute_root = std::path::absolute(root)?;
        if let Some(member) = opening
            .members
            .iter()
            .find(|member| member.source_path() == absolute_root)
        {
            return Self::observe_member(member, opening);
        }
        Self::observe_authorities(opening)
    }

    pub fn observe_authorities(opening: &JsonlFamilyInventory<E>) -> JsonlResult<Self, E> {
        let mut state = JsonlFamilyMembershipState::default();
        let unobserved_routes = unobserved_membership_routes(opening)?;
        for authority in &opening.authorities {
            let directory = authority.directory()?;
            observe_membership_directory(&directory, 0, &mut state, &unobserved_routes)?;
            authority.revalidate_same_object()?;
        }
        Self::from_routes(state.routes, opening)
    }

    fn observe_member(
        member: &JsonlFamilyInventoryMember<E>,
        opening: &JsonlFamilyInventory<E>,
    ) -> JsonlResult<Self, E> {
        let (source_path, authority_path, authority) = match member {
            JsonlFamilyInventoryMember::Accepted { leaf, .. } => (
                leaf.source_path.as_path(),
                leaf.authority_path.as_path(),
                Arc::clone(&leaf.authority),
            ),
            JsonlFamilyInventoryMember::Quarantined { leaf, .. } => (
                leaf.source_path.as_path(),
                leaf.authority_path.as_path(),
                Arc::clone(exact_member_authority(
                    &opening.authorities,
                    &leaf.source_path,
                    &leaf.authority_path,
                )?),
            ),
            JsonlFamilyInventoryMember::Pending { leaf, .. } => (
                leaf.source_path.as_path(),
                leaf.authority_path.as_path(),
                Arc::clone(exact_member_authority(
                    &opening.authorities,
                    &leaf.source_path,
                    &leaf.authority_path,
                )?),
            ),
        };
        check_membership_path::<E>(source_path)?;
        if authority_path.components().count()
            > PROVIDER_JSONL_INVENTORY_MAX_DEPTH.saturating_add(1)
        {
            return Err(E::invalid_payload(
                "JSONL membership path depth exceeds the provider inventory bound".to_owned(),
            ));
        }
        let mut routes = BTreeMap::new();
        if matches!(
            member,
            JsonlFamilyInventoryMember::Quarantined { leaf, .. } if leaf.observation.is_none()
        ) {
            observe_unopened_member(&authority, authority_path)?;
        } else {
            let opened = authority.open_file(authority_path)?;
            opened.revalidate_same_object()?;
        }
        routes.insert(
            source_path.to_path_buf(),
            JsonlFamilyMembershipRoute {
                authority,
                authority_path: authority_path.to_path_buf(),
            },
        );
        Self::from_routes(routes, opening)
    }

    fn from_routes(
        routes: BTreeMap<PathBuf, JsonlFamilyMembershipRoute<E>>,
        opening: &JsonlFamilyInventory<E>,
    ) -> JsonlResult<Self, E> {
        let source_hints = opening
            .members
            .iter()
            .filter(|member| routes.contains_key(member.source_path()))
            .filter_map(|member| {
                member
                    .source()
                    .map(|source| (member.source_path().to_path_buf(), source.clone()))
            })
            .collect();
        Ok(Self {
            root_missing: false,
            routes,
            source_hints,
        })
    }

    pub fn unbound_routes(
        &self,
    ) -> impl Iterator<Item = (&Path, Arc<ProviderSourceRoot<E>>, &Path)> {
        self.routes
            .iter()
            .filter(|(path, _)| !self.source_hints.contains_key(*path))
            .map(|(path, route)| {
                (
                    path.as_path(),
                    Arc::clone(&route.authority),
                    route.authority_path.as_path(),
                )
            })
    }

    pub fn bind_source_hint(&mut self, path: PathBuf, source: SourceKey) {
        if self.routes.contains_key(&path) {
            self.source_hints.insert(path, source);
        }
    }

    pub(super) fn admits(
        &self,
        current: &Self,
        mode: JsonlFamilyInventoryMode,
        expected_sources: &HashMap<[u8; 32], TerminalSourceEvidence<E>>,
        owned_sources: &HashMap<[u8; 32], SourceKey>,
    ) -> bool {
        if self.root_missing != current.root_missing {
            return false;
        }
        match mode {
            JsonlFamilyInventoryMode::Exact => self.routes.keys().eq(current.routes.keys()),
            JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions => {
                current.source_hints.values().all(|source| {
                    let digest = source.exact_descriptor_digest();
                    !owned_sources
                        .get(&digest)
                        .is_some_and(|owned| owned.exact_descriptor_eq(source))
                        || expected_sources.contains_key(&digest)
                })
            }
        }
    }
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyMembershipRoute<E> {
    fn clone(&self) -> Self {
        Self {
            authority: Arc::clone(&self.authority),
            authority_path: self.authority_path.clone(),
        }
    }
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyMembershipObservation<E> {
    fn clone(&self) -> Self {
        Self {
            root_missing: self.root_missing,
            routes: self.routes.clone(),
            source_hints: self.source_hints.clone(),
        }
    }
}

struct JsonlFamilyMembershipState<E: JsonlFamilyError> {
    directories: usize,
    entries: usize,
    routes: BTreeMap<PathBuf, JsonlFamilyMembershipRoute<E>>,
}

impl<E: JsonlFamilyError> Default for JsonlFamilyMembershipState<E> {
    fn default() -> Self {
        Self {
            directories: 0,
            entries: 0,
            routes: BTreeMap::new(),
        }
    }
}

fn observe_membership_directory<E: JsonlFamilyError>(
    directory: &ProviderSourceDirectory<E>,
    depth: usize,
    state: &mut JsonlFamilyMembershipState<E>,
    unobserved_routes: &UnobservedMembershipRoutes,
) -> JsonlResult<(), E> {
    if depth > PROVIDER_JSONL_INVENTORY_MAX_DEPTH {
        return Err(E::invalid_payload(
            "JSONL membership directory depth exceeds the provider inventory bound".to_owned(),
        ));
    }
    state.directories = state.directories.saturating_add(1);
    if state.directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
        return Err(E::invalid_payload(
            "JSONL membership directory count exceeds the provider inventory bound".to_owned(),
        ));
    }

    // Bound enumeration before the platform helper allocates the child list.
    let remaining = PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
        .checked_sub(state.entries)
        .ok_or_else(|| {
            E::invalid_payload(
                "JSONL membership entry count exceeds the provider inventory bound".to_owned(),
            )
        })?;
    let children = directory.entries(remaining)?;
    state.entries = state
        .entries
        .checked_add(children.len())
        .ok_or_else(|| E::invalid_payload("JSONL membership entry count overflowed".to_owned()))?;

    for name in children {
        let authority_path = directory.relative_path().join(&name);
        let authority = directory.authority_root();
        let source_path = authority.named_path().join(&authority_path);
        check_membership_path::<E>(&source_path)?;
        if unobserved_routes
            .get(authority.named_path())
            .is_some_and(|routes| routes.contains(&authority_path))
        {
            if state
                .routes
                .insert(
                    source_path,
                    JsonlFamilyMembershipRoute {
                        authority: Arc::new(authority),
                        authority_path,
                    },
                )
                .is_some()
            {
                return Err(E::invalid_payload(
                    "JSONL membership contains a duplicate authority route".to_owned(),
                ));
            }
            continue;
        }
        let opened = match directory.open_child(&name) {
            Ok(opened) => opened,
            // Admission never admits a link-like or non-regular route (a
            // selected transcript that is a link fails admission), so
            // skipping here only ever drops non-route entries or a route that
            // changed into a link after admission; that change drops out of
            // the observed route set and fails the membership comparison as a
            // source change.
            Err(error) if error.is_ignorable_membership_entry() => {
                continue;
            }
            Err(error) => return Err(error),
        };
        match opened {
            OpenedProviderSourcePath::Directory(child) => {
                observe_membership_directory(
                    &child,
                    depth.saturating_add(1),
                    state,
                    unobserved_routes,
                )?;
            }
            OpenedProviderSourcePath::File(opened)
                if source_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "json" | "jsonl"))
                    || source_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.ends_with(".jsonl.zstd") || name.ends_with(".jsonl.zst")
                        }) =>
            {
                opened.revalidate_same_object_leaf()?;
                if state
                    .routes
                    .insert(
                        source_path,
                        JsonlFamilyMembershipRoute {
                            authority: Arc::new(authority),
                            authority_path,
                        },
                    )
                    .is_some()
                {
                    return Err(E::invalid_payload(
                        "JSONL membership contains a duplicate authority route".to_owned(),
                    ));
                }
            }
            OpenedProviderSourcePath::File(_) => {}
        }
    }
    // The root directory capability predates admission, so its exact metadata
    // stamp legitimately changes when frozen-mode writers add or remove a
    // child. The retained authority fence below proves root identity; exact
    // inventories additionally compare the root's full admission stamp before
    // and after this walk. Descendant directories were opened by this walk and
    // can therefore use an exact enumeration fence.
    if depth > 0 {
        directory.revalidate()?;
    }
    Ok(())
}

fn unobserved_membership_routes<E: JsonlFamilyError>(
    opening: &JsonlFamilyInventory<E>,
) -> JsonlResult<UnobservedMembershipRoutes, E> {
    let mut routes = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();
    for member in &opening.members {
        let JsonlFamilyInventoryMember::Quarantined { leaf, .. } = member else {
            continue;
        };
        if leaf.observation.is_some() {
            continue;
        }
        let authority = exact_member_authority(
            &opening.authorities,
            &leaf.source_path,
            &leaf.authority_path,
        )?;
        routes
            .entry(authority.named_path().to_path_buf())
            .or_default()
            .insert(leaf.authority_path.clone());
    }
    Ok(routes)
}

fn observe_unopened_member<E: JsonlFamilyError>(
    authority: &ProviderSourceRoot<E>,
    authority_path: &Path,
) -> JsonlResult<(), E> {
    let name = authority_path.file_name().ok_or_else(|| {
        E::invalid_payload("unobserved JSONL membership path has no file name".to_owned())
    })?;
    let parent = authority_path.parent().unwrap_or_else(|| Path::new(""));
    let directory = authority.open_directory(parent)?;
    let children = directory.entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES)?;
    if !children.iter().any(|child| child == name) {
        return Err(E::source_changed());
    }
    directory.revalidate()?;
    authority.revalidate_same_object()?;
    Ok(())
}

fn check_membership_path<E: JsonlFamilyError>(path: &Path) -> JsonlResult<(), E> {
    if path.as_os_str().as_encoded_bytes().len() > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
        return Err(E::invalid_payload(
            "JSONL membership path exceeds the provider inventory bound".to_owned(),
        ));
    }
    Ok(())
}
