use std::collections::BTreeMap;

pub use ctx_attribution_index::model::*;

use crate::query::{
    Citation as QueryCitation, Confidence as QueryConfidence, Fact as QueryFact,
    FactState as QueryFactState, QueryError, Resource as QueryResource, ResourceId,
};

pub trait ServingCitationQueryExt {
    fn to_query_citation(&self) -> Result<QueryCitation, QueryError>;
}

impl ServingCitationQueryExt for ServingCitation {
    fn to_query_citation(&self) -> Result<QueryCitation, QueryError> {
        QueryCitation::new(
            self.exact_core_citation()
                .map_err(|_| QueryError::InvalidCitation)?,
        )
    }
}

pub trait ServingResourceQueryExt {
    fn to_query_resource(&self) -> Result<QueryResource, ServingModelError>;
}

impl ServingResourceQueryExt for ServingResource {
    fn to_query_resource(&self) -> Result<QueryResource, ServingModelError> {
        Ok(QueryResource {
            id: ResourceId(self.graph_id()?),
            kind: self.typed_kind()?,
            display: self.display()?,
            logical_repository: self.logical_repository_graph_id()?.map(ResourceId),
        })
    }
}

pub trait ServingRecordQueryExt {
    fn to_query_fact(&self) -> Result<QueryFact, QueryError>;
}

impl ServingRecordQueryExt for ServingRecord {
    fn to_query_fact(&self) -> Result<QueryFact, QueryError> {
        let predicate = self
            .attributes
            .get(PREDICATE_ATTRIBUTE)
            .and_then(|value| match value {
                AttributeValue::String(value) => Some(value.clone()),
                _ => None,
            })
            .ok_or_else(|| QueryError::Backend("serving fact lost its predicate".to_owned()))?;
        let values = self
            .attributes
            .iter()
            .filter(|(key, _)| key.as_str() != PREDICATE_ATTRIBUTE)
            .map(|(key, value)| original_attribute_string(value).map(|value| (key.clone(), value)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let value = (!values.is_empty())
            .then(|| serde_json::to_string(&values))
            .transpose()
            .map_err(|_| QueryError::Backend("serving fact attributes are invalid".to_owned()))?;
        Ok(QueryFact {
            id: self.record_id.clone(),
            fact_type: self.fact_family.as_str().to_owned(),
            subject: ResourceId(
                self.subject
                    .graph_id()
                    .map_err(|_| QueryError::Backend("invalid serving subject".to_owned()))?,
            ),
            predicate,
            object: self
                .object
                .as_ref()
                .map(ServingResource::graph_id)
                .transpose()
                .map_err(|_| QueryError::Backend("invalid serving object".to_owned()))?
                .map(ResourceId),
            value,
            occurred_at_ms: self.occurred_at_unix_ms,
            confidence: match self.confidence {
                ServingConfidence::Verified => QueryConfidence::Verified,
                ServingConfidence::High => QueryConfidence::High,
                ServingConfidence::Medium => QueryConfidence::Medium,
                ServingConfidence::Ambiguous => QueryConfidence::Ambiguous,
            },
            state: match self.state {
                ServingFactState::Asserted => QueryFactState::Asserted,
                ServingFactState::Ambiguous => QueryFactState::Ambiguous,
                ServingFactState::Contradicted => QueryFactState::Contradicted,
                ServingFactState::Superseded => QueryFactState::Superseded,
            },
            detector_version: format!("{}@{}", self.detector_id, self.detector_revision),
            root_run: self
                .scope
                .as_ref()
                .map(ServingResource::graph_id)
                .transpose()
                .map_err(|_| QueryError::Backend("invalid serving scope".to_owned()))?
                .map(ResourceId),
            direct_actor: self
                .direct_actor
                .as_ref()
                .map(ServingResource::graph_id)
                .transpose()
                .map_err(|_| QueryError::Backend("invalid serving actor".to_owned()))?
                .map(ResourceId),
            citations: self
                .citations
                .iter()
                .map(ServingCitationQueryExt::to_query_citation)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

fn original_attribute_string(value: &AttributeValue) -> Result<String, QueryError> {
    match value {
        AttributeValue::String(value) => Ok(value.clone()),
        AttributeValue::Integer(value) => Ok(value.to_string()),
        AttributeValue::Boolean(value) => Ok(value.to_string()),
        AttributeValue::Strings(_) => Err(QueryError::Backend(
            "serving fact contains a non-Core attribute list".to_owned(),
        )),
    }
}
