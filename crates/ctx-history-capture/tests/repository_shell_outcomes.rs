#![allow(dead_code, unused_imports)]

pub(crate) use ctx_history_capture::OutputOutcome;

#[path = "../src/repository_attribution/shell.rs"]
mod shell;

pub(crate) use shell::{
    bounded_outcome_plan, lexical_absolute, BoundedOutcomeOperation, BoundedOutcomePlan,
    BoundedOutcomePlanDisposition,
};

#[path = "../src/repository_attribution/outcome.rs"]
mod outcome;
