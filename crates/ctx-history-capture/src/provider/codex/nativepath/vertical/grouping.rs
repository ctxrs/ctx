use super::*;

impl CodexNativeProducerStep {
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Window { chunk, .. } => chunk.serialized_bytes,
            Self::Noop(_) => 0,
        }
    }

    pub(super) fn source_done(&self) -> bool {
        matches!(
            self,
            Self::Noop(_)
                | Self::Window {
                    source_done: true,
                    ..
                }
        )
    }

    pub(super) fn try_merge_window(
        self,
        later: Self,
    ) -> VerticalResult<std::result::Result<Self, (Self, Self)>> {
        let (
            Self::Window {
                chunk,
                source_done: false,
                delta,
                report: None,
            },
            Self::Window {
                chunk: later_chunk,
                source_done,
                delta: later_delta,
                report,
            },
        ) = (self, later)
        else {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "Codex producer attempted to merge incompatible prepared steps",
            ));
        };
        match chunk.try_merge_same_source(later_chunk)? {
            Ok(chunk) => Ok(Ok(Self::Window {
                chunk,
                source_done,
                delta: CodexNativeCommittedDelta {
                    imported_sessions: delta
                        .imported_sessions
                        .saturating_add(later_delta.imported_sessions),
                    imported_events: delta
                        .imported_events
                        .saturating_add(later_delta.imported_events),
                    imported_edges: delta
                        .imported_edges
                        .saturating_add(later_delta.imported_edges),
                },
                report,
            })),
            Err((chunk, later_chunk)) => Ok(Err((
                Self::Window {
                    chunk,
                    source_done: false,
                    delta,
                    report: None,
                },
                Self::Window {
                    chunk: later_chunk,
                    source_done,
                    delta: later_delta,
                    report,
                },
            ))),
        }
    }
}

impl CodexNativeRootChunk {
    fn try_merge_same_source(
        self,
        later: Self,
    ) -> VerticalResult<std::result::Result<Self, (Self, Self)>> {
        let compatible = !self.terminal
            && self.context.canonical_source_key == later.context.canonical_source_key
            && self.context.generation == later.context.generation
            && self.context.source_revision == later.context.source_revision
            && self.next_frontier == later.expected_frontier
            && later
                .expected_store_cursor
                .as_ref()
                .is_some_and(|expected| expected == &self.next_store_cursor);
        if !compatible {
            return Err(CodexNativeVerticalError::CorruptFrontier(
                "adjacent Codex windows do not form one exact source chain",
            ));
        }
        let pages = self.pages.len().saturating_add(later.pages.len());
        let mutation_units = self
            .mutation_units
            .saturating_add(later.mutation_units)
            .saturating_sub(STORE_MUTATION_OVERHEAD_UNITS);
        let serialized_bytes = self.serialized_bytes.saturating_add(later.serialized_bytes);
        if pages > NATIVE_PATH_MAX_GROUP_PAGES
            || mutation_units > NATIVE_PATH_MAX_MUTATION_UNITS
            || serialized_bytes > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
        {
            return Ok(Err((self, later)));
        }

        let Self {
            pages: mut left_pages,
            expected_store_cursor,
            expected_frontier,
            ..
        } = self;
        let Self {
            context,
            pages: right_pages,
            next_store_cursor,
            next_frontier,
            terminal,
            ..
        } = later;
        left_pages.extend(right_pages);
        Self::new(
            context,
            left_pages,
            expected_store_cursor,
            next_store_cursor,
            expected_frontier,
            next_frontier,
            terminal,
        )
        .map(Ok)
    }
}
