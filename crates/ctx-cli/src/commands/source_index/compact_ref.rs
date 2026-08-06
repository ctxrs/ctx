use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use anyhow::{anyhow, Result};
use ctx_history_index::VerifiedIndex;
use uuid::Uuid;

use crate::transcript::normalize_uuid_prefix;

pub(super) const MIN_COMPACT_REF_HEX_LEN: usize = 8;
pub(super) const MAX_COMPACT_REF_HEX_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum CompactRefNamespace {
    Event,
    Session,
}

impl CompactRefNamespace {
    const fn ctx_id_name(self) -> &'static str {
        match self {
            Self::Event => "ctx_event_id",
            Self::Session => "ctx_session_id",
        }
    }
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
pub(super) enum CompactRefResolveError {
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
        "{namespace} id prefix {reference:?} is ambiguous; conflicting full IDs are {first} and {second}; use a longer {ctx_id_name} or a full UUID",
        ctx_id_name = namespace.ctx_id_name()
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
pub(super) struct CompactRefMap {
    events: BTreeMap<Uuid, String>,
    sessions: BTreeMap<Uuid, String>,
}

impl CompactRefMap {
    pub(super) fn event(&self, id: Uuid) -> Result<&str> {
        self.get(CompactRefNamespace::Event, id)
    }

    pub(super) fn session(&self, id: Uuid) -> Result<&str> {
        self.get(CompactRefNamespace::Session, id)
    }

    pub(super) fn get(&self, namespace: CompactRefNamespace, id: Uuid) -> Result<&str> {
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
pub(super) struct CompactRefResolver<'index> {
    current: &'index VerifiedIndex,
    retained_peer: Option<&'index VerifiedIndex>,
}

impl<'index> CompactRefResolver<'index> {
    pub(super) const fn new(
        current: &'index VerifiedIndex,
        retained_peer: Option<&'index VerifiedIndex>,
    ) -> Self {
        Self {
            current,
            retained_peer,
        }
    }

    pub(super) const fn current_index(&self) -> &'index VerifiedIndex {
        self.current
    }

    pub(super) fn resolve_id(
        &self,
        namespace: CompactRefNamespace,
        reference: &str,
    ) -> Result<Uuid> {
        resolve_with_probe(namespace, reference, &mut |namespace, prefix| {
            self.matches_for_prefix(namespace, prefix)
        })
    }

    /// Builds compact aliases only for the IDs the caller will render.
    /// Event and session IDs are abbreviated in independent namespaces.
    pub(super) fn compact_refs<EventIds, SessionIds>(
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
                    for event in index.events_by_id_prefix(prefix)? {
                        push_distinct_match(&mut matches, event.event_id.as_uuid());
                        if matches.len() == 2 {
                            matches.sort_unstable();
                            return Ok(matches);
                        }
                    }
                }
                CompactRefNamespace::Session => {
                    for session in index.sessions_by_id_prefix(prefix)? {
                        push_distinct_match(&mut matches, session.session_id.as_uuid());
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

fn push_distinct_match(matches: &mut Vec<Uuid>, candidate: Uuid) {
    if !matches.contains(&candidate) {
        matches.push(candidate);
    }
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
        assert!(message.contains("longer ctx_event_id or a full UUID"));

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
