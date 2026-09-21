use super::*;
use crate::graph::segment::{GIT_REMOTE_ALIAS, ServingFactState};
use crate::protocol::{BlameTarget, ResolvedBlameTarget};
use crate::query::{BlameEntry, BlameService, QueryBounds};

const DENSE_REPOSITORY: &str = "forge:github.com/example/dense-lookup-fixture";
const DENSE_REMOTE: &str = "https://github.com/example/dense-lookup-fixture";
const DENSE_COMMIT: &str = "01170aca1fd0b1da431965dd3470084dac59ad62";
const DENSE_FACT_ID: &str = "fact_4ccff844417035087df26e2950d2737f";
const DENSE_LIVE_ACCESS_RECORDS: usize = 47_221;
const DENSE_ALIAS_RECORDS: usize = 18_586;
const DENSE_RECORDS: usize = 65_808;

fn open_dense_repository_graph(root: &Path) -> (SegmentGraph, ResourceId, ResourceId) {
    const RECORDS_PER_SEGMENT: usize = 258;

    let (_records, _batch, mut projected) = referenced_commit_projection(1, true);
    let mut target_template = projected.pop().expect("dense repository target template");
    target_template.repository_id = DENSE_REPOSITORY.to_owned();
    for resource in [
        Some(&mut target_template.subject),
        target_template.object.as_mut(),
        target_template.scope.as_mut(),
        target_template.direct_actor.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        if resource.typed_kind().ok() == Some(ResourceKind::Repository) {
            resource.id = DENSE_REPOSITORY.to_owned();
        } else if resource.typed_kind().ok() == Some(ResourceKind::Commit) {
            resource.id = DENSE_COMMIT.to_owned();
        }
        if resource.repository_id.is_some() {
            resource.repository_id = Some(DENSE_REPOSITORY.to_owned());
        }
    }
    let target = ResourceId(target_template.subject.graph_id().expect("commit graph ID"));
    let repository = ServingResource {
        kind: ResourceKind::Repository.wire_name().to_owned(),
        id: DENSE_REPOSITORY.to_owned(),
        repository_id: Some(DENSE_REPOSITORY.to_owned()),
        worktree_id: None,
    };
    let repository_id = ResourceId(repository.graph_id().expect("repository graph ID"));
    let remote = ServingResource {
        kind: ResourceKind::Remote.wire_name().to_owned(),
        id: DENSE_REMOTE.to_owned(),
        repository_id: Some(DENSE_REPOSITORY.to_owned()),
        worktree_id: None,
    };
    let mut segments = Vec::new();
    for (segment_index, start) in (0..DENSE_RECORDS).step_by(RECORDS_PER_SEGMENT).enumerate() {
        let end = (start + RECORDS_PER_SEGMENT).min(DENSE_RECORDS);
        let records = (start..end)
            .map(|index| {
                let mut record = target_template.clone();
                let fact_id = if index + 1 == DENSE_RECORDS {
                    DENSE_FACT_ID.to_owned()
                } else {
                    format!("fact_{index:032x}")
                };
                record.record_id = fact_id.clone();
                record.occurred_at_unix_ms = Some(
                    1_700_200_000_000 + i64::try_from(index).expect("dense fixture timestamp"),
                );
                record.scope = Some(repository.clone());
                if index < DENSE_LIVE_ACCESS_RECORDS {
                    record.fact_family =
                        FactFamily::new(REPOSITORY_LIVE_ACCESS).expect("lookup fact family");
                    record.subject = repository.clone();
                } else if index < DENSE_LIVE_ACCESS_RECORDS + DENSE_ALIAS_RECORDS {
                    record.fact_family =
                        FactFamily::new(GIT_REMOTE_ALIAS).expect("alias fact family");
                    record.subject = remote.clone();
                    record.object = Some(repository.clone());
                    record.state = ServingFactState::Ambiguous;
                }

                record.index_terms = vec![fact_id, repository_id.0.clone()];
                for resource in [
                    Some(&record.subject),
                    record.object.as_ref(),
                    record.scope.as_ref(),
                    record.direct_actor.as_ref(),
                ]
                .into_iter()
                .flatten()
                {
                    record
                        .index_terms
                        .push(resource.graph_id().expect("dense resource graph ID"));
                    record
                        .index_terms
                        .push(resource.display().expect("dense resource display"));
                }
                record.index_terms.sort();
                record.index_terms.dedup();
                record
            })
            .collect();
        segments.push(write_segment(
            root,
            u8::try_from(segment_index).expect("bounded dense fixture segments"),
            records,
            Vec::new(),
        ));
    }
    publish(root, synthetic_receipt(), segments, None);
    (
        SegmentGraph::open(root, None).expect("open dense repository graph"),
        target,
        repository_id,
    )
}

#[test]
fn logical_pagination_exhausts_physical_prefix_and_exact_pages_before_ordering() {
    let (_records, _batch, mut projected) = referenced_commit_projection(501, true);
    assert_eq!(projected.len(), 501);
    let target = ResourceId(projected[0].subject.graph_id().expect("commit graph ID"));
    for (index, record) in projected.iter_mut().enumerate() {
        record.index_terms.push(if index + 1 == 501 {
            "page-z-late".to_owned()
        } else {
            format!("page-a-{index:04}")
        });
        record.index_terms.sort();
        record.index_terms.dedup();
    }
    let directory = tempfile::tempdir().expect("segment directory");

    let graph = open_graph(directory.path(), synthetic_receipt(), projected);

    let prefix_records = graph
        .merged_records(
            GIT_COMMIT_REFERENCED,
            super::merge::Lookup::Prefix {
                repository: None,
                term: "page-",
            },
        )
        .expect("exhaust physical prefix pages");
    assert_eq!(prefix_records.len(), 501);

    let mut first_work = super::merge::QueryWork::new();
    let first = graph
        .facts_page_with_work(&target, BlameFactFamily::Commit, None, 500, &mut first_work)
        .expect("first logical page");
    assert_eq!(first.items.len(), 500);
    assert_eq!(first.items[0].0.occurred_at_ms, Some(1_700_100_000_500));
    assert_eq!(first.items[499].0.occurred_at_ms, Some(1_700_100_000_001));
    assert_eq!(first_work.candidate_count(), 501);
    assert_eq!(first_work.retained_high_water(), 501);
    let cursor = first.next_cursor.expect("logical lookahead cursor");
    let mut second_work = super::merge::QueryWork::new();
    let second = graph
        .facts_page_with_work(
            &target,
            BlameFactFamily::Commit,
            Some(&cursor),
            500,
            &mut second_work,
        )
        .expect("second logical page");
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].0.occurred_at_ms, Some(1_700_100_000_000));
    assert!(second.next_cursor.is_none());
    assert_eq!(second_work.candidate_count(), 501);
    assert_eq!(second_work.retained_high_water(), 1);

    let graph = std::sync::Arc::new(graph);
    let workers = (0..4)
        .map(|_| {
            let graph = std::sync::Arc::clone(&graph);
            let target = target.clone();
            std::thread::spawn(move || {
                for _ in 0..3 {
                    let page = BlameGraph::blame_facts_page(
                        &&*graph,
                        &target,
                        BlameFactFamily::Commit,
                        None,
                        500,
                    )
                    .expect("concurrent logical page");
                    assert_eq!(page.items.len(), 500);
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().expect("query worker");
    }
}

#[test]
fn dense_65808_repository_records_support_scoped_and_unscoped_commit_queries() {
    assert_eq!(
        DENSE_LIVE_ACCESS_RECORDS + DENSE_ALIAS_RECORDS + 1,
        DENSE_RECORDS
    );
    assert_eq!(DENSE_LIVE_ACCESS_RECORDS + 1 + 1, 47_223);

    let directory = tempfile::tempdir().expect("dense repository directory");

    let (graph, target, repository_id) = open_dense_repository_graph(directory.path());

    let aliases = graph
        .merged_records(
            GIT_REMOTE_ALIAS,
            super::merge::Lookup::Exact {
                repository: None,
                term: DENSE_REMOTE,
            },
        )
        .expect("checked dense aliases");
    assert_eq!(aliases.len(), DENSE_ALIAS_RECORDS);
    assert!(
        aliases
            .iter()
            .all(|alias| alias.state == ServingFactState::Ambiguous)
    );

    let service = BlameService::new(&graph, QueryBounds::default()).expect("dense blame service");
    for repository in [Some(DENSE_REPOSITORY.to_owned()), None] {
        for _ in 0..3 {
            let page = service
                .execute(
                    &BlameTarget::Commit {
                        oid: DENSE_COMMIT.to_owned(),
                        repository: repository.clone(),
                    },
                    None,
                )
                .expect("dense commit query");
            let ResolvedBlameTarget::Commit { commit, repository } = &page.target else {
                panic!("dense commit target");
            };
            assert_eq!(commit.display, DENSE_COMMIT);
            assert_eq!(repository.display, DENSE_REPOSITORY);
            assert_eq!(page.entries.len(), 1);
            let BlameEntry::Commit(entry) = &page.entries[0] else {
                panic!("dense commit entry");
            };
            assert_eq!(entry.fact.id, DENSE_FACT_ID);
            assert_eq!(entry.fact.fact_type, GIT_COMMIT_REFERENCED);
            assert_eq!(entry.fact.subject, target);
            assert_eq!(entry.fact.citations.len(), 1);
            assert!(!page.has_more);
        }
    }

    let mut resource_work = super::merge::QueryWork::new();
    let repositories = graph
        .load_resources_with_work(std::slice::from_ref(&repository_id), 2, &mut resource_work)
        .expect("bounded repository resource lookup");
    assert_eq!(repositories.len(), 1);
    assert_eq!(repositories[0].id, repository_id);
    assert_eq!(repositories[0].kind, ResourceKind::Repository);
    assert!(resource_work.candidate_count() <= 258);
    assert_eq!(resource_work.retained_high_water(), 1);
}

#[test]
fn typed_resource_ambiguity_survives_bounded_selection() {
    let (_records, _batch, mut projected) = referenced_commit_projection(2, true);
    projected[1].subject.worktree_id = Some("alternate-worktree".to_owned());
    let directory = tempfile::tempdir().expect("ambiguous resource directory");

    let graph = open_graph(directory.path(), synthetic_receipt(), projected);

    let resources = BlameGraph::resolve(
        &&graph,
        &ResourceSelector {
            kind: ResourceKind::Commit,
            value: COMMIT.to_owned(),
            repository: Some(REPOSITORY.to_owned()),
        },
        2,
    )
    .expect("bounded ambiguous commit selection");
    assert_eq!(resources.len(), 2);
    assert!(
        resources.iter().all(|resource| {
            resource.kind == ResourceKind::Commit && resource.display == COMMIT
        })
    );
    assert_ne!(resources[0].id, resources[1].id);
}
