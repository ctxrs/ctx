#![allow(dead_code, unused_imports)]

pub(crate) use ctx_history_capture::OutputOutcome;

#[path = "../src/repository_attribution/shell.rs"]
mod shell;

pub(crate) use shell::{
    bounded_outcome_plan, bounded_pull_request_association_query, lexical_absolute,
    BoundedCommitProducer, BoundedOutcomeOperation, BoundedOutcomePlan,
    BoundedOutcomePlanDisposition,
};

#[path = "../src/repository_attribution/association.rs"]
mod association;
pub(crate) use association::{
    exact_pull_request_association, UnscopedPullRequestAssociationObservation,
};

#[path = "../src/repository_attribution/outcome.rs"]
mod outcome;
