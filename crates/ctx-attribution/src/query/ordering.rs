//! Frozen semantic ordering for query projection.

use std::cmp::Ordering;

use super::{Confidence, Fact, FactState, ProductionAttribution};

/// Storage-independent semantic priority for evidence about one typed blame target.
///
/// The priority changes presentation only. Callers must apply the existing ambiguity
/// policy before sorting; the comparator never upgrades confidence or changes fact state.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ProducerFirstPriority {
    DirectProduction = 0,
    DirectModification = 1,
    PossibleProduction = 2,
    DirectOperation = 3,
    Copied = 4,
    Inspected = 5,
    Referenced = 6,
    Mentioned = 7,
    Other = u8::MAX,
}

impl ProducerFirstPriority {
    pub const fn rank(self) -> u8 {
        self as u8
    }

    pub const fn from_rank(rank: u8) -> Self {
        match rank {
            0 => Self::DirectProduction,
            1 => Self::DirectModification,
            2 => Self::PossibleProduction,
            3 => Self::DirectOperation,
            4 => Self::Copied,
            5 => Self::Inspected,
            6 => Self::Referenced,
            7 => Self::Mentioned,
            _ => Self::Other,
        }
    }
}

/// Complete deterministic order key after the caller has restricted evidence to one
/// typed blame target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProducerFirstOrderKey<'a> {
    priority: ProducerFirstPriority,
    occurred_at_ms: Option<i64>,
    related_resource_id: &'a str,
    fact_id: &'a str,
}

impl<'a> ProducerFirstOrderKey<'a> {
    pub const fn new(
        priority: ProducerFirstPriority,
        occurred_at_ms: Option<i64>,
        related_resource_id: &'a str,
        fact_id: &'a str,
    ) -> Self {
        Self {
            priority,
            occurred_at_ms,
            related_resource_id,
            fact_id,
        }
    }

    pub fn for_fact(fact: &'a Fact) -> Self {
        Self::new(
            producer_first_priority(fact),
            fact.occurred_at_ms,
            fact.object.as_ref().map_or("", |resource| &resource.0),
            &fact.id,
        )
    }

    fn for_attribution(attribution: &'a ProductionAttribution) -> Self {
        let priority = match attribution.relationship {
            crate::protocol::ProductionRelationship::ProducedBy
                if attribution.state == FactState::Asserted =>
            {
                ProducerFirstPriority::DirectProduction
            }
            crate::protocol::ProductionRelationship::PossiblyProducedBy => {
                ProducerFirstPriority::PossibleProduction
            }
            crate::protocol::ProductionRelationship::ProducedBy => ProducerFirstPriority::Other,
        };
        Self::new(
            priority,
            None,
            &attribution.producing_session.0,
            &attribution.fact_id,
        )
    }
}

pub fn compare_producer_evidence(
    left: &ProducerFirstOrderKey<'_>,
    right: &ProducerFirstOrderKey<'_>,
) -> Ordering {
    left.priority
        .rank()
        .cmp(&right.priority.rank())
        .then_with(|| optional_time_desc(left.occurred_at_ms, right.occurred_at_ms))
        .then_with(|| left.related_resource_id.cmp(right.related_resource_id))
        .then_with(|| left.fact_id.cmp(right.fact_id))
}

pub fn compare_production_attributions(
    left: &ProductionAttribution,
    right: &ProductionAttribution,
) -> Ordering {
    compare_producer_evidence(
        &ProducerFirstOrderKey::for_attribution(left),
        &ProducerFirstOrderKey::for_attribution(right),
    )
}

pub fn compare_facts(left: &Fact, right: &Fact) -> Ordering {
    compare_producer_evidence(
        &ProducerFirstOrderKey::for_fact(left),
        &ProducerFirstOrderKey::for_fact(right),
    )
}

pub fn producer_first_priority(fact: &Fact) -> ProducerFirstPriority {
    let direct = is_asserted_verified(fact.state, fact.confidence);
    match fact.predicate.as_str() {
        "produced_by" if direct => ProducerFirstPriority::DirectProduction,
        "modified_by" | "amended_by" | "cherry_picked_from" | "reverts" if direct => {
            ProducerFirstPriority::DirectModification
        }
        "possibly_produced_by" => ProducerFirstPriority::PossibleProduction,
        "pushed_by" if direct => ProducerFirstPriority::DirectOperation,
        "copied_by" | "copied_from" => ProducerFirstPriority::Copied,
        "inspected_by" => ProducerFirstPriority::Inspected,
        "referenced_by" => ProducerFirstPriority::Referenced,
        "mentioned_by" => ProducerFirstPriority::Mentioned,
        _ => ProducerFirstPriority::Other,
    }
}

const fn is_asserted_verified(state: FactState, confidence: Confidence) -> bool {
    matches!(state, FactState::Asserted) && matches!(confidence, Confidence::Verified)
}

fn optional_time_desc(left: Option<i64>, right: Option<i64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
#[path = "ordering_tests.rs"]
mod tests;
