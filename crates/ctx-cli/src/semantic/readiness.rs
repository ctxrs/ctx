use serde::{Deserialize, Serialize};

use super::model_retry::{SemanticModelFailureClass, SemanticModelRetryStatus};

pub(crate) const SEMANTIC_READINESS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticReadinessState {
    Ready,
    Partial,
    Empty,
    RetryDeferred,
    Unavailable,
    Failed,
    Disabled,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticCoverageDiagnostics {
    pub(crate) indexed_items: usize,
    pub(crate) indexed_chunks: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) searchable_items: Option<usize>,
    pub(crate) dirty_items: usize,
    pub(crate) queued_items: usize,
}

impl SemanticCoverageDiagnostics {
    pub(crate) fn is_complete(&self) -> bool {
        self.searchable_items.is_some_and(|searchable| {
            self.indexed_items >= searchable && self.dirty_items == 0 && self.queued_items == 0
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticReadinessBlockerCode {
    SemanticDisabled,
    UnsupportedPlatform,
    SearchableItemsUnknown,
    NoSearchableItems,
    NoIndexedItems,
    CoverageIncomplete,
    DirtyItems,
    QueuedItems,
    ModelUnavailable,
    ModelRetryDeferred,
    ModelFailureTerminal,
    SidecarUnavailable,
    VectorBackendUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub(crate) enum SemanticReadinessBlocker {
    SemanticDisabled,
    UnsupportedPlatform,
    SearchableItemsUnknown,
    NoSearchableItems,
    NoIndexedItems,
    CoverageIncomplete {
        missing_items: usize,
    },
    DirtyItems {
        items: usize,
    },
    QueuedItems {
        items: usize,
    },
    ModelUnavailable,
    ModelRetryDeferred {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_class: Option<SemanticModelFailureClass>,
        attempt: u32,
        next_retry_at_ms: i64,
    },
    ModelFailureTerminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_class: Option<SemanticModelFailureClass>,
        attempt: u32,
    },
    SidecarUnavailable,
    VectorBackendUnavailable,
}

impl SemanticReadinessBlocker {
    pub(crate) fn code(&self) -> SemanticReadinessBlockerCode {
        match self {
            Self::SemanticDisabled => SemanticReadinessBlockerCode::SemanticDisabled,
            Self::UnsupportedPlatform => SemanticReadinessBlockerCode::UnsupportedPlatform,
            Self::SearchableItemsUnknown => SemanticReadinessBlockerCode::SearchableItemsUnknown,
            Self::NoSearchableItems => SemanticReadinessBlockerCode::NoSearchableItems,
            Self::NoIndexedItems => SemanticReadinessBlockerCode::NoIndexedItems,
            Self::CoverageIncomplete { .. } => SemanticReadinessBlockerCode::CoverageIncomplete,
            Self::DirtyItems { .. } => SemanticReadinessBlockerCode::DirtyItems,
            Self::QueuedItems { .. } => SemanticReadinessBlockerCode::QueuedItems,
            Self::ModelUnavailable => SemanticReadinessBlockerCode::ModelUnavailable,
            Self::ModelRetryDeferred { .. } => SemanticReadinessBlockerCode::ModelRetryDeferred,
            Self::ModelFailureTerminal { .. } => SemanticReadinessBlockerCode::ModelFailureTerminal,
            Self::SidecarUnavailable => SemanticReadinessBlockerCode::SidecarUnavailable,
            Self::VectorBackendUnavailable => {
                SemanticReadinessBlockerCode::VectorBackendUnavailable
            }
        }
    }

    pub(crate) fn blocks_retrieval(&self) -> bool {
        !matches!(
            self,
            Self::CoverageIncomplete { .. } | Self::DirtyItems { .. } | Self::QueuedItems { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticReadinessInputs {
    pub(crate) enabled: bool,
    pub(crate) supported: bool,
    pub(crate) model_available: bool,
    pub(crate) sidecar_available: bool,
    pub(crate) vector_backend_available: bool,
    pub(crate) coverage: SemanticCoverageDiagnostics,
    pub(crate) model_retry: SemanticModelRetryStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticReadinessDiagnostics {
    pub(crate) schema_version: u32,
    pub(crate) state: SemanticReadinessState,
    pub(crate) retrieval_available: bool,
    pub(crate) coverage: SemanticCoverageDiagnostics,
    pub(crate) blockers: Vec<SemanticReadinessBlocker>,
    pub(crate) model_retry: SemanticModelRetryStatus,
}

impl SemanticReadinessDiagnostics {
    pub(crate) fn evaluate(inputs: SemanticReadinessInputs) -> Self {
        let mut blockers = Vec::new();
        if !inputs.enabled {
            blockers.push(SemanticReadinessBlocker::SemanticDisabled);
        }
        if !inputs.supported {
            blockers.push(SemanticReadinessBlocker::UnsupportedPlatform);
        }
        match inputs.coverage.searchable_items {
            None => blockers.push(SemanticReadinessBlocker::SearchableItemsUnknown),
            Some(0) => blockers.push(SemanticReadinessBlocker::NoSearchableItems),
            Some(searchable) if inputs.coverage.indexed_items < searchable => {
                blockers.push(SemanticReadinessBlocker::CoverageIncomplete {
                    missing_items: searchable.saturating_sub(inputs.coverage.indexed_items),
                });
            }
            Some(_) => {}
        }
        if inputs.coverage.searchable_items.unwrap_or(0) > 0 && inputs.coverage.indexed_items == 0 {
            blockers.push(SemanticReadinessBlocker::NoIndexedItems);
        }
        if inputs.coverage.dirty_items > 0 {
            blockers.push(SemanticReadinessBlocker::DirtyItems {
                items: inputs.coverage.dirty_items,
            });
        }
        if inputs.coverage.queued_items > 0 {
            blockers.push(SemanticReadinessBlocker::QueuedItems {
                items: inputs.coverage.queued_items,
            });
        }
        if !inputs.model_available {
            if inputs.model_retry.terminal {
                blockers.push(SemanticReadinessBlocker::ModelFailureTerminal {
                    failure_class: inputs.model_retry.failure_class,
                    attempt: inputs.model_retry.attempt,
                });
            } else if let Some(next_retry_at_ms) = inputs.model_retry.next_retry_at_ms {
                blockers.push(SemanticReadinessBlocker::ModelRetryDeferred {
                    failure_class: inputs.model_retry.failure_class,
                    attempt: inputs.model_retry.attempt,
                    next_retry_at_ms,
                });
            }
            blockers.push(SemanticReadinessBlocker::ModelUnavailable);
        }
        if !inputs.sidecar_available {
            blockers.push(SemanticReadinessBlocker::SidecarUnavailable);
        }
        if !inputs.vector_backend_available {
            blockers.push(SemanticReadinessBlocker::VectorBackendUnavailable);
        }

        let retrieval_available = !blockers
            .iter()
            .any(SemanticReadinessBlocker::blocks_retrieval);
        let state = if !inputs.enabled {
            SemanticReadinessState::Disabled
        } else if !inputs.supported {
            SemanticReadinessState::Unsupported
        } else if inputs.coverage.searchable_items == Some(0) {
            SemanticReadinessState::Empty
        } else if !inputs.model_available && inputs.model_retry.terminal {
            SemanticReadinessState::Failed
        } else if !inputs.model_available && inputs.model_retry.next_retry_at_ms.is_some() {
            SemanticReadinessState::RetryDeferred
        } else if retrieval_available && inputs.coverage.is_complete() {
            SemanticReadinessState::Ready
        } else if retrieval_available {
            SemanticReadinessState::Partial
        } else {
            SemanticReadinessState::Unavailable
        };

        Self {
            schema_version: SEMANTIC_READINESS_SCHEMA_VERSION,
            state,
            retrieval_available,
            coverage: inputs.coverage,
            blockers,
            model_retry: inputs.model_retry,
        }
    }

    pub(crate) fn primary_blocker(&self) -> Option<&SemanticReadinessBlocker> {
        self.blockers
            .iter()
            .find(|blocker| blocker.blocks_retrieval())
            .or_else(|| self.blockers.first())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticRetrievalRequestMode {
    AutomaticRerank,
    ExplicitSemantic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticEffectiveBackend {
    Lexical,
    Hybrid,
    Semantic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticRetrievalDiagnostics {
    pub(crate) attempted: bool,
    pub(crate) request_mode: SemanticRetrievalRequestMode,
    pub(crate) readiness: SemanticReadinessState,
    pub(crate) effective_backend: SemanticEffectiveBackend,
    pub(crate) lexical_eligibility_preserved: bool,
    pub(crate) lexical_order_preserved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blocker: Option<SemanticReadinessBlockerCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) candidate_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) candidate_limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vector_bytes_estimate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vector_byte_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vector_bytes_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vector_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) query_embed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vector_scan_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) chunks_scanned: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) events_scored: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hits_returned: Option<usize>,
}

impl SemanticRetrievalDiagnostics {
    pub(crate) fn automatic_fallback(
        readiness: &SemanticReadinessDiagnostics,
        attempted: bool,
    ) -> Self {
        Self {
            attempted,
            request_mode: SemanticRetrievalRequestMode::AutomaticRerank,
            readiness: readiness.state,
            effective_backend: SemanticEffectiveBackend::Lexical,
            lexical_eligibility_preserved: true,
            lexical_order_preserved: true,
            blocker: readiness
                .primary_blocker()
                .map(SemanticReadinessBlocker::code),
            candidate_items: None,
            candidate_limit: None,
            vector_bytes_estimate: None,
            vector_byte_limit: None,
            vector_bytes_read: None,
            vector_backend: None,
            query_embed_ms: None,
            vector_scan_ms: None,
            chunks_scanned: None,
            events_scored: None,
            hits_returned: None,
        }
    }

    pub(crate) fn automatic_success(readiness: SemanticReadinessState) -> Self {
        Self::success(
            SemanticRetrievalRequestMode::AutomaticRerank,
            SemanticEffectiveBackend::Hybrid,
            readiness,
        )
    }

    pub(crate) fn explicit_success(readiness: SemanticReadinessState) -> Self {
        Self::success(
            SemanticRetrievalRequestMode::ExplicitSemantic,
            SemanticEffectiveBackend::Semantic,
            readiness,
        )
    }

    fn success(
        request_mode: SemanticRetrievalRequestMode,
        effective_backend: SemanticEffectiveBackend,
        readiness: SemanticReadinessState,
    ) -> Self {
        Self {
            attempted: true,
            request_mode,
            readiness,
            effective_backend,
            lexical_eligibility_preserved: request_mode
                == SemanticRetrievalRequestMode::AutomaticRerank,
            lexical_order_preserved: false,
            blocker: None,
            candidate_items: None,
            candidate_limit: None,
            vector_bytes_estimate: None,
            vector_byte_limit: None,
            vector_bytes_read: None,
            vector_backend: None,
            query_embed_ms: None,
            vector_scan_ms: None,
            chunks_scanned: None,
            events_scored: None,
            hits_returned: None,
        }
    }

    pub(crate) fn with_candidate_state(
        mut self,
        candidate_items: usize,
        candidate_limit: usize,
    ) -> Self {
        self.candidate_items = Some(candidate_items);
        self.candidate_limit = Some(candidate_limit);
        self
    }

    pub(crate) fn with_vector_byte_state(
        mut self,
        estimate: u64,
        limit: u64,
        bytes_read: Option<u64>,
    ) -> Self {
        self.vector_bytes_estimate = Some(estimate);
        self.vector_byte_limit = Some(limit);
        self.vector_bytes_read = bytes_read;
        self
    }
}
