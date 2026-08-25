use std::collections::{HashMap, VecDeque};

use ctx_history_core::StableEntityId;
use ctx_history_index_query::{EventSearchCandidate, SearchFamilyKey};

use super::{SearchEventMetadata, SearchHit, SearchResultWindow};

pub(super) struct SessionChampion<'candidate> {
    pub(super) candidate: &'candidate EventSearchCandidate,
    pub(super) match_count: usize,
}

pub(super) struct FamilyShapingOutcome {
    pub(super) result_window: SearchResultWindow,
    pub(super) distinct_families: usize,
    pub(super) changed_final_top_n: bool,
}

pub fn shape_search_result_window<'a>(
    candidates: impl IntoIterator<Item = &'a EventSearchCandidate>,
    limit: usize,
    event_results: bool,
) -> SearchResultWindow {
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    let mut hits = if event_results {
        dense_hits(candidates.iter().copied())
    } else {
        session_champions(candidates.iter().copied())
            .into_iter()
            .map(|session| session_champion_hit(&session))
            .collect()
    };
    let more_available = hits.len() > limit;
    hits.truncate(limit);
    SearchResultWindow {
        limit,
        hits,
        more_available,
    }
}

pub(super) fn dense_result_window(
    candidates: &[EventSearchCandidate],
    limit: usize,
) -> SearchResultWindow {
    let mut hits = dense_hits(candidates.iter());
    let more_available = hits.len() > limit;
    hits.truncate(limit);
    SearchResultWindow {
        limit,
        hits,
        more_available,
    }
}

fn dense_hits<'candidate>(
    candidates: impl IntoIterator<Item = &'candidate EventSearchCandidate>,
) -> Vec<SearchHit> {
    candidates
        .into_iter()
        .map(|candidate| SearchHit {
            event: SearchEventMetadata::from(&candidate.event),
            score: candidate.score,
            more_matches_in_session: 0,
        })
        .collect()
}

pub(super) fn session_champions<'candidate>(
    candidates: impl IntoIterator<Item = &'candidate EventSearchCandidate>,
) -> Vec<SessionChampion<'candidate>> {
    let mut session_positions = HashMap::<StableEntityId, usize>::new();
    let mut champions = Vec::<SessionChampion<'_>>::new();
    for candidate in candidates {
        if let Some(position) = session_positions.get(&candidate.event.session_id).copied() {
            champions[position].match_count = champions[position].match_count.saturating_add(1);
            continue;
        }
        session_positions.insert(candidate.event.session_id, champions.len());
        champions.push(SessionChampion {
            candidate,
            match_count: 1,
        });
    }
    champions
}

pub(super) fn shape_family_result_window(
    champions: &[SessionChampion<'_>],
    families: &[SearchFamilyKey],
    limit: usize,
) -> FamilyShapingOutcome {
    debug_assert_eq!(champions.len(), families.len());
    let mut family_positions = HashMap::<StableEntityId, usize>::new();
    let mut positions_by_family = Vec::<Vec<usize>>::new();
    for (position, family) in families.iter().copied().enumerate() {
        let family_position = match family_positions.get(&family.session_id).copied() {
            Some(family_position) => family_position,
            None => {
                let family_position = positions_by_family.len();
                family_positions.insert(family.session_id, family_position);
                positions_by_family.push(Vec::new());
                family_position
            }
        };
        positions_by_family[family_position].push(position);
    }

    let mut shaped_positions = Vec::with_capacity(champions.len());
    let mut next_by_family = vec![0_usize; positions_by_family.len()];
    let mut active_families = (0..positions_by_family.len()).collect::<VecDeque<_>>();
    while let Some(family_position) = active_families.pop_front() {
        let next = next_by_family[family_position];
        let positions = &positions_by_family[family_position];
        if let Some(position) = positions.get(next).copied() {
            shaped_positions.push(position);
            next_by_family[family_position] = next.saturating_add(1);
            if next_by_family[family_position] < positions.len() {
                active_families.push_back(family_position);
            }
        }
    }

    let changed_final_top_n = champions
        .iter()
        .take(limit)
        .map(|champion| champion.candidate.event.event_id)
        .ne(shaped_positions
            .iter()
            .take(limit)
            .map(|position| champions[*position].candidate.event.event_id));
    let more_available = shaped_positions.len() > limit;
    let hits = shaped_positions
        .into_iter()
        .take(limit)
        .map(|position| session_champion_hit(&champions[position]))
        .collect();
    FamilyShapingOutcome {
        result_window: SearchResultWindow {
            limit,
            hits,
            more_available,
        },
        distinct_families: positions_by_family.len(),
        changed_final_top_n,
    }
}

fn session_champion_hit(session: &SessionChampion<'_>) -> SearchHit {
    SearchHit {
        event: SearchEventMetadata::from(&session.candidate.event),
        score: session.candidate.score,
        more_matches_in_session: session.match_count.saturating_sub(1),
    }
}
