use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use anyhow::{anyhow, Result};
use ctx_history_core::CaptureProvider;
use ctx_history_index_query::{CoreEventRecord, SessionRecord, VerifiedIndex};
use uuid::Uuid;

pub const MIN_COMPACT_REF_HEX_LEN: usize = 8;
pub const MAX_COMPACT_REF_HEX_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompactRefNamespace {
    Event,
    Session,
}

impl fmt::Display for CompactRefNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Event => "event",
            Self::Session => "session",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompactRefResolveError {
    #[error("{namespace} {id} was not found in the current or retained Core generation")]
    ExactNotFound {
        namespace: CompactRefNamespace,
        id: Uuid,
    },
    #[error(
        "{namespace} id prefix {reference:?} was not found in the current or retained Core generation"
    )]
    NotFound {
        namespace: CompactRefNamespace,
        reference: String,
    },
    #[error(
        "{namespace} id prefix {reference:?} is ambiguous between full IDs {first} and {second}"
    )]
    Ambiguous {
        namespace: CompactRefNamespace,
        reference: String,
        first: Uuid,
        second: Uuid,
    },
    #[error(
        "compact {namespace} reference probe for {prefix:?} returned {actual} matches; the maximum is two"
    )]
    ProbeLimitExceeded {
        namespace: CompactRefNamespace,
        prefix: String,
        actual: usize,
    },
    #[error(
        "cannot render compact {namespace} reference for {id}; the full ID is absent from the current and retained Core generations"
    )]
    RenderTargetMissing {
        namespace: CompactRefNamespace,
        id: Uuid,
    },
    #[error(
        "compact {namespace} reference for {id} was not prepared; include every rendered ID in the batch"
    )]
    ReferenceNotPrepared {
        namespace: CompactRefNamespace,
        id: Uuid,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactRefMap {
    events: BTreeMap<Uuid, String>,
    sessions: BTreeMap<Uuid, String>,
}

impl CompactRefMap {
    pub fn event(&self, id: Uuid) -> Result<&str> {
        self.get(CompactRefNamespace::Event, id)
    }

    pub fn session(&self, id: Uuid) -> Result<&str> {
        self.get(CompactRefNamespace::Session, id)
    }

    pub fn get(&self, namespace: CompactRefNamespace, id: Uuid) -> Result<&str> {
        let references = match namespace {
            CompactRefNamespace::Event => &self.events,
            CompactRefNamespace::Session => &self.sessions,
        };
        references
            .get(&id)
            .map(String::as_str)
            .ok_or_else(|| CompactRefResolveError::ReferenceNotPrepared { namespace, id }.into())
    }
}

/// Resolves and presents compact IDs against one pinned generation pair.
///
/// The caller owns both pins. `retained_peer` is the retained previous
/// generation, when one is already available; this helper never reads or
/// interprets generation pointers.
pub struct CompactRefResolver<'index> {
    current: &'index VerifiedIndex,
    retained_peer: Option<&'index VerifiedIndex>,
}

impl<'index> CompactRefResolver<'index> {
    pub const fn new(
        current: &'index VerifiedIndex,
        retained_peer: Option<&'index VerifiedIndex>,
    ) -> Self {
        Self {
            current,
            retained_peer,
        }
    }

    pub const fn current_index(&self) -> &'index VerifiedIndex {
        self.current
    }

    pub fn resolve_id(&self, namespace: CompactRefNamespace, reference: &str) -> Result<Uuid> {
        resolve_with_probe(namespace, reference, &mut |namespace, prefix| {
            self.matches_for_prefix(namespace, prefix)
        })
    }

    pub fn contains_exact(&self, namespace: CompactRefNamespace, id: Uuid) -> Result<bool> {
        Ok(self
            .matches_for_prefix(namespace, &id.simple().to_string())?
            .contains(&id))
    }

    /// Builds compact aliases only for the IDs the caller will render.
    /// Event and session IDs are abbreviated in independent namespaces.
    pub fn compact_refs<EventIds, SessionIds>(
        &self,
        event_ids: EventIds,
        session_ids: SessionIds,
    ) -> Result<CompactRefMap>
    where
        EventIds: IntoIterator<Item = Uuid>,
        SessionIds: IntoIterator<Item = Uuid>,
    {
        compact_refs_with_probe(event_ids, session_ids, &mut |namespace, prefix| {
            self.matches_for_prefix(namespace, prefix)
        })
    }

    fn matches_for_prefix(
        &self,
        namespace: CompactRefNamespace,
        prefix: &str,
    ) -> Result<Vec<Uuid>> {
        let mut matches = Vec::with_capacity(2);
        for index in std::iter::once(self.current).chain(self.retained_peer) {
            match namespace {
                CompactRefNamespace::Event => {
                    for event_id in index.event_ids_by_id_prefix(prefix)? {
                        push_distinct_match(&mut matches, event_id);
                        if matches.len() == 2 {
                            matches.sort_unstable();
                            return Ok(matches);
                        }
                    }
                }
                CompactRefNamespace::Session => {
                    for session_id in index.session_ids_by_id_prefix(prefix)? {
                        push_distinct_match(&mut matches, session_id);
                        if matches.len() == 2 {
                            matches.sort_unstable();
                            return Ok(matches);
                        }
                    }
                }
            }
        }
        matches.sort_unstable();
        Ok(matches)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingLookupKind {
    Event,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectorError {
    #[error("{kind} id prefix is shorter than {minimum} hexadecimal characters")]
    PrefixTooShort { kind: String, minimum: usize },
    #[error("{kind} id is neither a full UUID nor a hexadecimal prefix")]
    InvalidId { kind: String },
    #[error("provider session selector is empty")]
    EmptyProviderSession,
    #[error("session selectors are mutually exclusive")]
    ConflictingSessionSelectors,
    #[error("a session selector is required")]
    MissingSessionSelector,
    #[error("provider_key and source_id must be supplied together")]
    IncompleteCustomSourceSelector,
    #[error("{field} selector is empty")]
    EmptyCustomSourceSelector { field: &'static str },
    #[error("provider_key/source_id requires a provider session selector")]
    CustomSourceSelectorRequiresProviderSession,
    #[error("provider_key/source_id can only select custom history")]
    CustomSourceSelectorRequiresCustomProvider,
    #[error("provider session {provider_session_id:?} was not found in the Core generation")]
    ProviderSessionNotFound { provider_session_id: String },
    #[error(
        "provider session {provider_session_id:?} is ambiguous between sessions {first} and {second}"
    )]
    ProviderSessionAmbiguous {
        provider_session_id: String,
        first: Uuid,
        second: Uuid,
        first_route: Option<String>,
        second_route: Option<String>,
    },
    #[error("Core session {session_id} belongs to provider {actual}, not {requested}")]
    ProviderMismatch {
        session_id: Uuid,
        actual: String,
        requested: String,
    },
}

impl MissingLookupKind {
    const fn noun(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct MissingLookupError {
    kind: MissingLookupKind,
    requested: String,
    message: String,
}

impl MissingLookupError {
    fn exact(kind: MissingLookupKind, requested: impl Into<String>) -> Self {
        let requested = requested.into();
        let message = format!(
            "{} {requested} was not found in the Core generation",
            kind.noun()
        );
        Self {
            kind,
            requested,
            message,
        }
    }

    fn prefix(kind: MissingLookupKind, requested: impl Into<String>) -> Self {
        let requested = requested.into();
        let message = format!(
            "{} id prefix {requested:?} was not found in the Core generation",
            kind.noun()
        );
        Self {
            kind,
            requested,
            message,
        }
    }

    pub const fn kind(&self) -> MissingLookupKind {
        self.kind
    }

    pub fn requested(&self) -> &str {
        &self.requested
    }
}

pub fn resolve_core_event(index: &VerifiedIndex, id: &str) -> Result<CoreEventRecord> {
    let references = CompactRefResolver::new(index, None);
    resolve_core_event_with_refs(&references, id)
}

pub fn resolve_core_event_with_refs(
    references: &CompactRefResolver<'_>,
    id: &str,
) -> Result<CoreEventRecord> {
    let event_id = resolve_compact_id_for_lookup(references, CompactRefNamespace::Event, id)?;
    references
        .current_index()
        .core_event_by_id(event_id)?
        .ok_or_else(|| missing_resolved_lookup(MissingLookupKind::Event, id, event_id).into())
}

pub fn resolve_session(index: &VerifiedIndex, id: &str) -> Result<SessionRecord> {
    let references = CompactRefResolver::new(index, None);
    resolve_session_with_refs(&references, id)
}

pub fn resolve_session_with_refs(
    references: &CompactRefResolver<'_>,
    id: &str,
) -> Result<SessionRecord> {
    let session_id = resolve_compact_id_for_lookup(references, CompactRefNamespace::Session, id)?;
    references
        .current_index()
        .session_by_id(session_id)?
        .ok_or_else(|| missing_resolved_lookup(MissingLookupKind::Session, id, session_id).into())
}

pub fn validate_ctx_id(id: &str, kind: &str) -> Result<String> {
    let trimmed = id.trim();
    if Uuid::parse_str(trimmed).is_ok() {
        return Ok(trimmed.to_ascii_lowercase());
    }
    normalize_uuid_prefix(trimmed, kind)
}

pub fn validate_session_selector(
    id: Option<&str>,
    provider_session_id: Option<&str>,
) -> Result<()> {
    match (id, provider_session_id) {
        (Some(id), None) => {
            validate_ctx_id(id, "session")?;
            Ok(())
        }
        (None, Some(provider_session_id)) if provider_session_id.trim().is_empty() => {
            Err(SelectorError::EmptyProviderSession.into())
        }
        (None, Some(_)) => Ok(()),
        (Some(_), Some(_)) => Err(SelectorError::ConflictingSessionSelectors.into()),
        (None, None) => Err(SelectorError::MissingSessionSelector.into()),
    }
}

pub fn resolve_show_session(
    index: &VerifiedIndex,
    id: Option<&str>,
    provider_session_id: Option<&str>,
    provider: Option<CaptureProvider>,
    provider_key: Option<&str>,
    source_id: Option<&str>,
) -> Result<SessionRecord> {
    let references = CompactRefResolver::new(index, None);
    resolve_show_session_with_refs(
        &references,
        id,
        provider_session_id,
        provider,
        provider_key,
        source_id,
    )
}

pub fn resolve_show_session_with_refs(
    references: &CompactRefResolver<'_>,
    id: Option<&str>,
    provider_session_id: Option<&str>,
    provider: Option<CaptureProvider>,
    provider_key: Option<&str>,
    source_id: Option<&str>,
) -> Result<SessionRecord> {
    let index = references.current_index();
    validate_session_selector(id, provider_session_id)?;
    validate_custom_source_selector(id, provider_session_id, provider, provider_key, source_id)?;
    let effective_provider = if provider_key.is_some() {
        Some(CaptureProvider::Custom)
    } else {
        provider
    };
    let session = match (id, provider_session_id) {
        (Some(id), None) => resolve_session_with_refs(references, id)?,
        (None, Some(provider_session_id)) => select_show_provider_session(
            provider_session_id,
            index.sessions_by_provider_session_id(
                provider_session_id,
                effective_provider.map(CaptureProvider::as_str),
                provider_key,
                source_id,
            )?,
        )?,
        (Some(_), Some(_)) => return Err(SelectorError::ConflictingSessionSelectors.into()),
        (None, None) => return Err(SelectorError::MissingSessionSelector.into()),
    };
    if let Some(provider) = provider {
        if session.provider != provider.as_str() {
            return Err(SelectorError::ProviderMismatch {
                session_id: session.session_id.as_uuid(),
                actual: session.provider,
                requested: provider.as_str().to_owned(),
            }
            .into());
        }
    }
    Ok(session)
}

fn validate_custom_source_selector(
    id: Option<&str>,
    provider_session_id: Option<&str>,
    provider: Option<CaptureProvider>,
    provider_key: Option<&str>,
    source_id: Option<&str>,
) -> Result<()> {
    let (provider_key, source_id) = match (provider_key, source_id) {
        (None, None) => return Ok(()),
        (Some(provider_key), Some(source_id)) => (provider_key, source_id),
        _ => return Err(SelectorError::IncompleteCustomSourceSelector.into()),
    };
    for (field, value) in [("provider_key", provider_key), ("source_id", source_id)] {
        if value.trim().is_empty() {
            return Err(SelectorError::EmptyCustomSourceSelector { field }.into());
        }
    }
    if id.is_some() || provider_session_id.is_none() {
        return Err(SelectorError::CustomSourceSelectorRequiresProviderSession.into());
    }
    if provider.is_some_and(|provider| provider != CaptureProvider::Custom) {
        return Err(SelectorError::CustomSourceSelectorRequiresCustomProvider.into());
    }
    Ok(())
}

fn select_show_provider_session(
    provider_session_id: &str,
    matches: Vec<SessionRecord>,
) -> Result<SessionRecord> {
    match matches.as_slice() {
        [] => Err(SelectorError::ProviderSessionNotFound {
            provider_session_id: provider_session_id.to_owned(),
        }
        .into()),
        [session] => Ok(session.clone()),
        matches => Err(SelectorError::ProviderSessionAmbiguous {
            provider_session_id: provider_session_id.to_owned(),
            first: matches[0].session_id.as_uuid(),
            second: matches[1].session_id.as_uuid(),
            first_route: custom_session_route(&matches[0]),
            second_route: custom_session_route(&matches[1]),
        }
        .into()),
    }
}

fn custom_session_route(session: &SessionRecord) -> Option<String> {
    session
        .provider_key
        .as_deref()
        .zip(session.source_id.as_deref())
        .map(|(provider_key, source_id)| format!("{provider_key}/{source_id}"))
}

fn resolve_compact_id_for_lookup(
    references: &CompactRefResolver<'_>,
    namespace: CompactRefNamespace,
    id: &str,
) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(id.trim()) {
        return Ok(id);
    }
    match references.resolve_id(namespace, id) {
        Ok(id) => Ok(id),
        Err(error) => match error.downcast_ref::<CompactRefResolveError>() {
            Some(CompactRefResolveError::ExactNotFound { id, .. }) => {
                let kind = match namespace {
                    CompactRefNamespace::Event => MissingLookupKind::Event,
                    CompactRefNamespace::Session => MissingLookupKind::Session,
                };
                Err(MissingLookupError::exact(kind, id.to_string()).into())
            }
            Some(CompactRefResolveError::NotFound { reference, .. }) => {
                let kind = match namespace {
                    CompactRefNamespace::Event => MissingLookupKind::Event,
                    CompactRefNamespace::Session => MissingLookupKind::Session,
                };
                Err(MissingLookupError::prefix(kind, reference).into())
            }
            _ => Err(error),
        },
    }
}

fn missing_resolved_lookup(
    kind: MissingLookupKind,
    requested: &str,
    resolved: Uuid,
) -> MissingLookupError {
    if Uuid::parse_str(requested.trim()).is_ok() {
        MissingLookupError::exact(kind, resolved.to_string())
    } else {
        MissingLookupError::prefix(kind, requested.trim().to_ascii_lowercase())
    }
}

fn push_distinct_match(matches: &mut Vec<Uuid>, candidate: Uuid) {
    if !matches.contains(&candidate) {
        matches.push(candidate);
    }
}

fn normalize_uuid_prefix(value: &str, kind: &str) -> Result<String> {
    let prefix = value.trim();
    if prefix.len() < MIN_COMPACT_REF_HEX_LEN {
        return Err(SelectorError::PrefixTooShort {
            kind: kind.to_owned(),
            minimum: MIN_COMPACT_REF_HEX_LEN,
        }
        .into());
    }
    if prefix.contains('-')
        || !prefix
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(SelectorError::InvalidId {
            kind: kind.to_owned(),
        }
        .into());
    }
    Ok(prefix.to_ascii_lowercase())
}

fn resolve_with_probe<Probe>(
    namespace: CompactRefNamespace,
    reference: &str,
    probe: &mut Probe,
) -> Result<Uuid>
where
    Probe: FnMut(CompactRefNamespace, &str) -> Result<Vec<Uuid>>,
{
    let trimmed = reference.trim();
    if let Ok(id) = Uuid::parse_str(trimmed) {
        let matches = checked_probe(namespace, &id.simple().to_string(), probe)?;
        if matches.contains(&id) {
            return Ok(id);
        }
        return Err(CompactRefResolveError::ExactNotFound { namespace, id }.into());
    }

    let prefix = normalize_uuid_prefix(trimmed, &namespace.to_string())?;
    if prefix.len() > MAX_COMPACT_REF_HEX_LEN {
        return Err(anyhow!(
            "{namespace} id prefix must contain at most {MAX_COMPACT_REF_HEX_LEN} hex characters"
        ));
    }
    let matches = checked_probe(namespace, &prefix, probe)?;
    match matches.as_slice() {
        [] => Err(CompactRefResolveError::NotFound {
            namespace,
            reference: prefix,
        }
        .into()),
        [id] => Ok(*id),
        [first, second] => Err(CompactRefResolveError::Ambiguous {
            namespace,
            reference: prefix,
            first: *first,
            second: *second,
        }
        .into()),
        _ => Err(anyhow!(
            "compact reference probe returned an invalid match set"
        )),
    }
}

fn compact_refs_with_probe<EventIds, SessionIds, Probe>(
    event_ids: EventIds,
    session_ids: SessionIds,
    probe: &mut Probe,
) -> Result<CompactRefMap>
where
    EventIds: IntoIterator<Item = Uuid>,
    SessionIds: IntoIterator<Item = Uuid>,
    Probe: FnMut(CompactRefNamespace, &str) -> Result<Vec<Uuid>>,
{
    let events = event_ids.into_iter().collect::<BTreeSet<_>>();
    let sessions = session_ids.into_iter().collect::<BTreeSet<_>>();
    let mut cache = BTreeMap::new();
    let mut rendered = CompactRefMap::default();

    for id in events {
        let reference =
            shortest_unique_reference(CompactRefNamespace::Event, id, &mut cache, probe)?;
        rendered.events.insert(id, reference);
    }
    for id in sessions {
        let reference =
            shortest_unique_reference(CompactRefNamespace::Session, id, &mut cache, probe)?;
        rendered.sessions.insert(id, reference);
    }
    Ok(rendered)
}

fn shortest_unique_reference<Probe>(
    namespace: CompactRefNamespace,
    id: Uuid,
    cache: &mut BTreeMap<(CompactRefNamespace, String), Vec<Uuid>>,
    probe: &mut Probe,
) -> Result<String>
where
    Probe: FnMut(CompactRefNamespace, &str) -> Result<Vec<Uuid>>,
{
    let full = id.simple().to_string();
    for length in MIN_COMPACT_REF_HEX_LEN..=MAX_COMPACT_REF_HEX_LEN {
        let prefix = full[..length].to_owned();
        let key = (namespace, prefix.clone());
        let matches = if let Some(matches) = cache.get(&key) {
            matches.clone()
        } else {
            let matches = checked_probe(namespace, &prefix, probe)?;
            cache.insert(key, matches.clone());
            matches
        };
        if matches.as_slice() == [id] {
            return Ok(prefix);
        }
        if matches.is_empty() {
            break;
        }
    }
    Err(CompactRefResolveError::RenderTargetMissing { namespace, id }.into())
}

fn checked_probe<Probe>(
    namespace: CompactRefNamespace,
    prefix: &str,
    probe: &mut Probe,
) -> Result<Vec<Uuid>>
where
    Probe: FnMut(CompactRefNamespace, &str) -> Result<Vec<Uuid>>,
{
    let mut matches = probe(namespace, prefix)?;
    if matches.len() > 2 {
        return Err(CompactRefResolveError::ProbeLimitExceeded {
            namespace,
            prefix: prefix.to_owned(),
            actual: matches.len(),
        }
        .into());
    }
    matches.sort_unstable();
    matches.dedup();
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ProbeWorld {
        current_events: Vec<Uuid>,
        retained_events: Vec<Uuid>,
        current_sessions: Vec<Uuid>,
        retained_sessions: Vec<Uuid>,
        calls: Vec<(CompactRefNamespace, String)>,
    }

    impl ProbeWorld {
        fn probe(&mut self, namespace: CompactRefNamespace, prefix: &str) -> Result<Vec<Uuid>> {
            self.calls.push((namespace, prefix.to_owned()));
            let (current, retained) = match namespace {
                CompactRefNamespace::Event => (&self.current_events, &self.retained_events),
                CompactRefNamespace::Session => (&self.current_sessions, &self.retained_sessions),
            };
            let mut matches = Vec::with_capacity(2);
            for generation in [current, retained] {
                let mut generation_matches = generation
                    .iter()
                    .copied()
                    .filter(|id| id.simple().to_string().starts_with(prefix))
                    .collect::<Vec<_>>();
                generation_matches.sort_unstable();
                for id in generation_matches.into_iter().take(2) {
                    push_distinct_match(&mut matches, id);
                    if matches.len() == 2 {
                        matches.sort_unstable();
                        return Ok(matches);
                    }
                }
            }
            matches.sort_unstable();
            Ok(matches)
        }
    }

    fn id(value: &str) -> Uuid {
        Uuid::parse_str(value).unwrap()
    }

    #[test]
    fn input_accepts_full_or_unique_prefix_and_ambiguity_names_both_full_ids() {
        let first = id("aaaaaaaa-0000-8000-8000-000000000001");
        let second = id("aaaaaaaa-1000-8000-8000-000000000002");
        let mut full_world = ProbeWorld {
            current_events: vec![first],
            ..ProbeWorld::default()
        };
        let full = resolve_with_probe(
            CompactRefNamespace::Event,
            &first.to_string(),
            &mut |namespace, prefix| full_world.probe(namespace, prefix),
        )
        .unwrap();
        assert_eq!(full, first);

        let mut missing_full = ProbeWorld::default();
        let error = resolve_with_probe(
            CompactRefNamespace::Event,
            &second.to_string(),
            &mut |namespace, prefix| missing_full.probe(namespace, prefix),
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CompactRefResolveError>(),
            Some(CompactRefResolveError::ExactNotFound {
                namespace: CompactRefNamespace::Event,
                id,
            }) if *id == second
        ));

        let mut unique = ProbeWorld {
            current_events: vec![first],
            ..ProbeWorld::default()
        };
        assert_eq!(
            resolve_with_probe(
                CompactRefNamespace::Event,
                "  AAAAAAAA0  ",
                &mut |namespace, prefix| unique.probe(namespace, prefix),
            )
            .unwrap(),
            first
        );

        let mut ambiguous = ProbeWorld {
            current_events: vec![second, first],
            ..ProbeWorld::default()
        };
        let error = resolve_with_probe(
            CompactRefNamespace::Event,
            "aaaaaaaa",
            &mut |namespace, prefix| ambiguous.probe(namespace, prefix),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&first.to_string()), "{message}");
        assert!(message.contains(&second.to_string()), "{message}");
        assert!(message.contains("is ambiguous between full IDs"));

        let mut missing = ProbeWorld::default();
        let error = resolve_with_probe(
            CompactRefNamespace::Session,
            "bbbbbbbb",
            &mut |namespace, prefix| missing.probe(namespace, prefix),
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CompactRefResolveError>(),
            Some(CompactRefResolveError::NotFound {
                namespace: CompactRefNamespace::Session,
                reference,
            }) if reference == "bbbbbbbb"
        ));
    }

    #[test]
    fn batch_presentation_uses_shortest_no_dash_refs_in_independent_namespaces() {
        let first = id("aaaaaaaa-0000-8000-8000-000000000001");
        let second = id("aaaaaaaa-1000-8000-8000-000000000002");
        let unrelated = id("bbbbbbbb-0000-8000-8000-000000000003");
        let mut world = ProbeWorld {
            current_events: vec![first, second, unrelated],
            current_sessions: vec![first],
            ..ProbeWorld::default()
        };

        let rendered =
            compact_refs_with_probe([second, first, first], [first], &mut |namespace, prefix| {
                world.probe(namespace, prefix)
            })
            .unwrap();
        assert_eq!(rendered.event(first).unwrap(), "aaaaaaaa0");
        assert_eq!(rendered.event(second).unwrap(), "aaaaaaaa1");
        assert_eq!(rendered.session(first).unwrap(), "aaaaaaaa");
        assert!(matches!(
            rendered
                .get(CompactRefNamespace::Event, unrelated)
                .unwrap_err()
                .downcast_ref::<CompactRefResolveError>(),
            Some(CompactRefResolveError::ReferenceNotPrepared {
                namespace: CompactRefNamespace::Event,
                id,
            }) if *id == unrelated
        ));
        assert!(world
            .calls
            .iter()
            .all(|(_, prefix)| !prefix.starts_with("bbbbbbbb")));
        assert_eq!(
            world
                .calls
                .iter()
                .filter(|(namespace, prefix)| {
                    *namespace == CompactRefNamespace::Event && prefix == "aaaaaaaa"
                })
                .count(),
            1,
            "the batch must cache a shared collision probe"
        );

        for (namespace, expected, reference) in [
            (
                CompactRefNamespace::Event,
                first,
                rendered.event(first).unwrap(),
            ),
            (
                CompactRefNamespace::Event,
                second,
                rendered.event(second).unwrap(),
            ),
            (
                CompactRefNamespace::Session,
                first,
                rendered.session(first).unwrap(),
            ),
        ] {
            assert_eq!(
                resolve_with_probe(namespace, reference, &mut |namespace, prefix| {
                    world.probe(namespace, prefix)
                })
                .unwrap(),
                expected
            );
            assert!(reference.chars().all(|character| {
                character.is_ascii_hexdigit() && !character.is_ascii_uppercase()
            }));
            assert!(!reference.contains('-'));
        }
    }

    #[test]
    fn retained_generation_collision_lengthens_new_refs_and_stale_refs_fail_closed() {
        let retained = id("deadbeef-0000-8000-8000-000000000001");
        let current = id("deadbeef-1000-8000-8000-000000000002");

        let mut before = ProbeWorld {
            current_events: vec![retained],
            ..ProbeWorld::default()
        };
        let before_refs = compact_refs_with_probe([retained], [], &mut |namespace, prefix| {
            before.probe(namespace, prefix)
        })
        .unwrap();
        assert_eq!(before_refs.event(retained).unwrap(), "deadbeef");

        let mut transition = ProbeWorld {
            current_events: vec![current],
            retained_events: vec![retained],
            ..ProbeWorld::default()
        };
        let transition_refs = compact_refs_with_probe([current], [], &mut |namespace, prefix| {
            transition.probe(namespace, prefix)
        })
        .unwrap();
        let current_ref = transition_refs.event(current).unwrap();
        assert_eq!(current_ref, "deadbeef1");
        assert_eq!(
            resolve_with_probe(
                CompactRefNamespace::Event,
                current_ref,
                &mut |namespace, prefix| transition.probe(namespace, prefix),
            )
            .unwrap(),
            current
        );

        let stale_error = resolve_with_probe(
            CompactRefNamespace::Event,
            "deadbeef",
            &mut |namespace, prefix| transition.probe(namespace, prefix),
        )
        .unwrap_err();
        assert!(matches!(
            stale_error.downcast_ref::<CompactRefResolveError>(),
            Some(CompactRefResolveError::Ambiguous { first, second, .. })
                if [*first, *second].contains(&retained)
                    && [*first, *second].contains(&current)
        ));
    }

    #[test]
    fn duplicate_identity_across_generation_pair_stays_at_the_minimum() {
        let shared = id("cafebabe-0000-8000-8000-000000000001");
        let mut world = ProbeWorld {
            current_sessions: vec![shared],
            retained_sessions: vec![shared],
            ..ProbeWorld::default()
        };
        let rendered = compact_refs_with_probe([], [shared], &mut |namespace, prefix| {
            world.probe(namespace, prefix)
        })
        .unwrap();
        assert_eq!(rendered.session(shared).unwrap(), "cafebabe");
    }

    #[test]
    fn compact_reference_length_is_bounded_at_thirty_two_hex_characters() {
        let first = id("12345678-1234-1234-1234-123456789ab0");
        let second = id("12345678-1234-1234-1234-123456789ab1");
        let mut world = ProbeWorld {
            current_events: vec![first, second],
            ..ProbeWorld::default()
        };
        let rendered = compact_refs_with_probe([first, second], [], &mut |namespace, prefix| {
            world.probe(namespace, prefix)
        })
        .unwrap();
        assert_eq!(
            rendered.event(first).unwrap().len(),
            MAX_COMPACT_REF_HEX_LEN
        );
        assert_eq!(
            rendered.event(second).unwrap().len(),
            MAX_COMPACT_REF_HEX_LEN
        );
        assert_eq!(rendered.event(first).unwrap(), first.simple().to_string());
        assert_eq!(rendered.event(second).unwrap(), second.simple().to_string());
    }

    #[test]
    fn probes_returning_more_than_two_matches_are_rejected() {
        let ids = [
            id("aaaaaaaa-0000-8000-8000-000000000001"),
            id("aaaaaaaa-1000-8000-8000-000000000002"),
            id("aaaaaaaa-2000-8000-8000-000000000003"),
        ];
        let error = resolve_with_probe(CompactRefNamespace::Event, "aaaaaaaa", &mut |_, _| {
            Ok(ids.to_vec())
        })
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CompactRefResolveError>(),
            Some(CompactRefResolveError::ProbeLimitExceeded { actual: 3, .. })
        ));
    }
}
