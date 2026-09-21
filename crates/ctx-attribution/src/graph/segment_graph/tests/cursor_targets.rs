use super::*;
use crate::protocol::{ResolvedBlameTarget, ResourceRef as PublicResource};
use crate::query::BlamePosition;

#[test]
fn fact_cursor_anchor_must_belong_to_the_resolved_targets_eligible_answers() {
    for (kind, family, fact_type, first, other) in [
        (
            ResourceKind::Commit,
            BlameFactFamily::Commit,
            GIT_COMMIT_REFERENCED,
            COMMIT,
            REFERENCED_COMMIT,
        ),
        (
            ResourceKind::PullRequest,
            BlameFactFamily::PullRequest,
            FORGE_CREATE,
            PULL_REQUEST,
            "github.com/ctxrs/ctx/pull_request/8",
        ),
    ] {
        let records = records(3);
        let subject = |display| ResourceRef::in_repository(kind, display, REPOSITORY);
        let facts = records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                vec![fact(
                    record,
                    fact_type,
                    subject(if index == 2 { other } else { first }),
                    "referenced_by",
                    Some(ResourceRef::new(
                        ResourceKind::Session,
                        record.session_id.to_string(),
                    )),
                    Some(1_700_000_000_000 + index as i64),
                    EnvelopeConfidence::Verified,
                    EnvelopeFactState::Asserted,
                    BTreeMap::new(),
                )]
            })
            .collect();
        let projected = project_core_batch(&batch(&records, facts)).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let graph = open_graph(directory.path(), synthetic_receipt(), projected.records);
        let resource = |display: &str| PublicResource {
            id: ServingResource::from_core(
                kind,
                display,
                Some(REPOSITORY.to_owned()),
                None,
                None,
                &records[0].source,
            )
            .unwrap()
            .graph_id()
            .unwrap(),
            kind,
            display: display.to_owned(),
        };
        let repository = PublicResource {
            id: ctx_attribution_index::logical_repository_graph_id(REPOSITORY).unwrap(),
            kind: ResourceKind::Repository,
            display: REPOSITORY.to_owned(),
        };
        let resolved = match family {
            BlameFactFamily::Commit => ResolvedBlameTarget::Commit {
                commit: resource(first),
                repository,
            },
            BlameFactFamily::PullRequest => ResolvedBlameTarget::PullRequest {
                selector: first.to_owned(),
                pull_request: resource(first),
                repository,
            },
        };
        let target = ResourceId(resource(first).id);
        let first_page = graph.facts_page(&target, family, None, 1).unwrap();
        assert_eq!(first_page.items.len(), 1);
        let anchor = first_page.next_cursor.unwrap();
        let wrap = |position| match family {
            BlameFactFamily::Commit => BlamePosition::Commit(position),
            BlameFactFamily::PullRequest => BlamePosition::PullRequest(position),
        };
        let cursor = graph
            .encode_blame_cursor(&resolved, graph.graph_generation(), &wrap(anchor.clone()))
            .unwrap();
        assert_eq!(
            graph
                .decode_blame_cursor(&cursor, &resolved, graph.graph_generation())
                .unwrap(),
            wrap(anchor.clone())
        );
        let continuation = graph.facts_page(&target, family, Some(&anchor), 1).unwrap();
        assert_eq!(continuation.items.len(), 1);
        assert_ne!(first_page.items[0].0.id, continuation.items[0].0.id);

        let foreign = graph
            .facts_page(&ResourceId(resource(other).id), family, None, 1)
            .unwrap()
            .items
            .remove(0)
            .1;
        // Valid encoding, same manifest generation, same target fingerprint,
        // eligible same-family fact; only the fact's subject is wrong.
        let forged = graph
            .encode_blame_cursor(&resolved, graph.graph_generation(), &wrap(foreign))
            .unwrap();
        assert!(matches!(
            graph.decode_blame_cursor(&forged, &resolved, graph.graph_generation()),
            Err(SegmentGraphError::InvalidCursor)
        ));
        assert!(matches!(
            graph.decode_blame_cursor(
                &"x".repeat(crate::protocol::MAX_BLAME_CURSOR_BYTES + 1),
                &resolved,
                graph.graph_generation()
            ),
            Err(SegmentGraphError::InvalidCursor)
        ));
    }
}
