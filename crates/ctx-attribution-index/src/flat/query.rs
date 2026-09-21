use super::super::model::{FactFamily, ServingRecord};
use super::{
    FlatQueryContinuation, FlatQueryPage, FlatSegmentError, MAX_QUERY_POSTINGS_SCANNED,
    validate_repository_scope,
};

pub struct QueryState {
    pub postings_scanned: usize,
    pub results: Vec<ServingRecord>,
    first_repository_id: Option<String>,
    repository_ambiguous: bool,
}

impl QueryState {
    pub fn resume(continuation: Option<&FlatQueryContinuation>) -> Self {
        continuation.map_or_else(
            || Self {
                postings_scanned: 0,
                results: Vec::new(),
                first_repository_id: None,
                repository_ambiguous: false,
            },
            |value| Self {
                postings_scanned: value.postings_scanned,
                results: Vec::new(),
                first_repository_id: value.first_repository_id.clone(),
                repository_ambiguous: value.repository_ambiguous,
            },
        )
    }

    pub fn validate_continuation(&self) -> Result<(), FlatSegmentError> {
        if self.postings_scanned > MAX_QUERY_POSTINGS_SCANNED {
            return Err(FlatSegmentError::Corrupt("query continuation scan count"));
        }
        let Some(first_repository_id) = self.first_repository_id.as_deref() else {
            if self.repository_ambiguous {
                return Err(FlatSegmentError::Corrupt("query continuation ambiguity"));
            }
            return Ok(());
        };
        validate_repository_scope(first_repository_id)?;
        Ok(())
    }

    pub fn consider(&mut self, record: ServingRecord) {
        if let Some(first_repository_id) = &self.first_repository_id {
            if first_repository_id != &record.repository_id {
                self.repository_ambiguous = true;
            }
        } else {
            self.first_repository_id = Some(record.repository_id.clone());
        }
        self.results.push(record);
    }

    pub fn into_page(self, next: Option<(usize, Option<Vec<u8>>, usize)>) -> FlatQueryPage {
        let continuation =
            next.map(
                |(shard_index, key, next_posting_index)| FlatQueryContinuation {
                    shard_index,
                    key,
                    next_posting_index,
                    postings_scanned: self.postings_scanned,
                    first_repository_id: self.first_repository_id.clone(),
                    repository_ambiguous: self.repository_ambiguous,
                },
            );
        FlatQueryPage {
            records: self.results,
            repository_ambiguous: self.repository_ambiguous,
            continuation,
        }
    }
}

pub enum QuerySelector<'a> {
    Exact {
        repository_id: &'a str,
        fact_family: &'a FactFamily,
        term: &'a str,
    },
    Prefix {
        repository_id: &'a str,
        fact_family: &'a FactFamily,
        term_prefix: &'a str,
    },
    ExactUnscoped {
        fact_family: &'a FactFamily,
        term: &'a str,
    },
    PrefixUnscoped {
        fact_family: &'a FactFamily,
        term_prefix: &'a str,
    },
}

impl QuerySelector<'_> {
    pub fn selects_indexed_term(
        &self,
        record: &ServingRecord,
        indexed_term: &str,
    ) -> Result<bool, FlatSegmentError> {
        let (scope_matches, expected_term) = match self {
            Self::Exact {
                repository_id,
                fact_family,
                term,
            } => (
                record.repository_id == *repository_id && record.fact_family == **fact_family,
                record
                    .index_terms
                    .iter()
                    .find(|candidate| candidate.as_str() == *term),
            ),
            Self::Prefix {
                repository_id,
                fact_family,
                term_prefix,
            } => (
                record.repository_id == *repository_id && record.fact_family == **fact_family,
                record
                    .index_terms
                    .iter()
                    .find(|candidate| candidate.starts_with(term_prefix)),
            ),
            Self::ExactUnscoped { fact_family, term } => (
                record.fact_family == **fact_family,
                record
                    .index_terms
                    .iter()
                    .find(|candidate| candidate.as_str() == *term),
            ),
            Self::PrefixUnscoped {
                fact_family,
                term_prefix,
            } => (
                record.fact_family == **fact_family,
                record
                    .index_terms
                    .iter()
                    .find(|candidate| candidate.starts_with(term_prefix)),
            ),
        };
        if !scope_matches || expected_term.is_none() {
            return Err(FlatSegmentError::Corrupt("posting selector scope"));
        }
        if !record
            .index_terms
            .iter()
            .any(|candidate| candidate == indexed_term)
        {
            return Err(FlatSegmentError::Corrupt("posting selector term"));
        }
        Ok(expected_term.is_some_and(|term| term == indexed_term))
    }
}
