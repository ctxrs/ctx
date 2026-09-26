use super::*;

/// Bounded prefix results collected from the active generation before its
/// searcher is released. This lets compact presentation open the retained
/// peer without keeping two large Tantivy readers alive at once.
#[derive(Debug, Default)]
pub(crate) struct PreparedCompactRefCurrent {
    events: BTreeMap<String, Vec<Uuid>>,
    sessions: BTreeMap<String, Vec<Uuid>>,
}

impl PreparedCompactRefCurrent {
    pub(crate) fn from_ids<EventIds, SessionIds>(
        current: &VerifiedIndex,
        event_ids: EventIds,
        session_ids: SessionIds,
    ) -> Result<Self>
    where
        EventIds: IntoIterator<Item = Uuid>,
        SessionIds: IntoIterator<Item = Uuid>,
    {
        let mut prepared = Self::default();
        prepare_current_prefixes(&mut prepared.events, event_ids, |prefix| {
            Ok(current.event_ids_by_id_prefix(prefix)?)
        })?;
        prepare_current_prefixes(&mut prepared.sessions, session_ids, |prefix| {
            Ok(current.session_ids_by_id_prefix(prefix)?)
        })?;
        Ok(prepared)
    }
}

fn prepare_current_prefixes<Ids, Probe>(
    prepared: &mut BTreeMap<String, Vec<Uuid>>,
    ids: Ids,
    mut probe: Probe,
) -> Result<()>
where
    Ids: IntoIterator<Item = Uuid>,
    Probe: FnMut(&str) -> Result<Vec<Uuid>>,
{
    for id in ids.into_iter().collect::<BTreeSet<_>>() {
        let full = id.simple().to_string();
        for length in MIN_COMPACT_REF_HEX_LEN..=MAX_COMPACT_REF_HEX_LEN {
            let prefix = full[..length].to_owned();
            let matches = if let Some(matches) = prepared.get(&prefix) {
                matches.clone()
            } else {
                let matches = probe(&prefix)?;
                prepared.insert(prefix, matches.clone());
                matches
            };
            if matches.is_empty() || matches.as_slice() == [id] {
                for later_length in length.saturating_add(1)..=MAX_COMPACT_REF_HEX_LEN {
                    prepared.insert(full[..later_length].to_owned(), matches.clone());
                }
                break;
            }
        }
    }
    Ok(())
}

/// Compact-reference resolver backed by bounded active-generation probes and
/// an optionally opened retained peer.
pub(crate) struct PreparedCompactRefResolver<'index> {
    current: PreparedCompactRefCurrent,
    retained_peer: Option<&'index VerifiedIndex>,
}

impl<'index> PreparedCompactRefResolver<'index> {
    pub(crate) const fn new(
        current: PreparedCompactRefCurrent,
        retained_peer: Option<&'index VerifiedIndex>,
    ) -> Self {
        Self {
            current,
            retained_peer,
        }
    }

    pub(crate) fn contains_exact(&self, namespace: CompactRefNamespace, id: Uuid) -> Result<bool> {
        Ok(self
            .matches_for_prefix(namespace, &id.simple().to_string())?
            .contains(&id))
    }

    pub(crate) fn compact_refs<EventIds, SessionIds>(
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
        let mut matches = match namespace {
            CompactRefNamespace::Event => self.current.events.get(prefix).cloned(),
            CompactRefNamespace::Session => self.current.sessions.get(prefix).cloned(),
        }
        .unwrap_or_default();
        if matches.len() < 2 {
            if let Some(peer) = self.retained_peer {
                let peer_matches = match namespace {
                    CompactRefNamespace::Event => peer.event_ids_by_id_prefix(prefix)?,
                    CompactRefNamespace::Session => peer.session_ids_by_id_prefix(prefix)?,
                };
                for candidate in peer_matches {
                    push_distinct_match(&mut matches, candidate);
                    if matches.len() == 2 {
                        break;
                    }
                }
            }
        }
        matches.sort_unstable();
        Ok(matches)
    }
}
