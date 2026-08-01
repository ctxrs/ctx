use std::collections::BTreeSet;

use ctx_history_core::SourceKey;
use ctx_history_index::{CoreSourceEventPage, SourceEventCursor, VerifiedIndex};
use ctx_history_relational::{
    RelationalProjectionError, RelationalProjectionRecord, RelationalSourceMetadata,
};
use uuid::Uuid;

use super::{relational_source_metadata, SourceBackedRelationalCatchUpError};

#[derive(Clone, Copy)]
pub(super) enum RelationalSourceSelection<'a> {
    All,
    Changed(&'a BTreeSet<Uuid>),
}

impl RelationalSourceSelection<'_> {
    fn includes(self, source_id: Uuid) -> bool {
        match self {
            Self::All => true,
            Self::Changed(sources) => sources.contains(&source_id),
        }
    }
}

pub(super) struct RelationalRecordStream<'a> {
    index: &'a VerifiedIndex,
    selection: RelationalSourceSelection<'a>,
    certificate_index: usize,
    current: Option<SourceRecordStream>,
    page_size: usize,
    failed: bool,
    #[cfg(test)]
    pub(super) pages_loaded: usize,
    #[cfg(test)]
    pub(super) page_items_loaded: usize,
    #[cfg(test)]
    pub(super) max_page_items: usize,
}

impl<'a> RelationalRecordStream<'a> {
    pub(super) fn new(
        index: &'a VerifiedIndex,
        selection: RelationalSourceSelection<'a>,
        page_size: usize,
    ) -> Self {
        Self {
            index,
            selection,
            certificate_index: 0,
            current: None,
            page_size,
            failed: false,
            #[cfg(test)]
            pages_loaded: 0,
            #[cfg(test)]
            page_items_loaded: 0,
            #[cfg(test)]
            max_page_items: 0,
        }
    }

    fn prepare_next_source(
        &mut self,
    ) -> std::result::Result<bool, SourceBackedRelationalCatchUpError> {
        while let Some(certificate) = self.index.manifest().sources.get(self.certificate_index) {
            self.certificate_index += 1;
            let source = certificate.observation().source();
            if !self.selection.includes(source.identity().as_uuid()) {
                continue;
            }
            let page = load_source_page(self.index, source, None, self.page_size)?;
            self.observe_page(&page);
            self.current = Some(SourceRecordStream::new(
                relational_source_metadata(certificate)?,
                page,
            )?);
            return Ok(true);
        }
        Ok(false)
    }

    #[cfg(test)]
    fn observe_page(&mut self, page: &CoreSourceEventPage) {
        self.pages_loaded += 1;
        self.page_items_loaded += page.items.len();
        self.max_page_items = self.max_page_items.max(page.items.len());
    }

    #[cfg(not(test))]
    fn observe_page(&mut self, _page: &CoreSourceEventPage) {}

    #[cfg(test)]
    fn observe_page_items(&mut self, page_items: usize) {
        self.pages_loaded += 1;
        self.page_items_loaded += page_items;
        self.max_page_items = self.max_page_items.max(page_items);
    }

    #[cfg(not(test))]
    fn observe_page_items(&mut self, _page_items: usize) {}
}

impl Iterator for RelationalRecordStream<'_> {
    type Item = std::result::Result<RelationalProjectionRecord, RelationalProjectionError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if self.current.is_none() {
                match self.prepare_next_source() {
                    Ok(true) => {}
                    Ok(false) => return None,
                    Err(error) => return self.fail(error),
                }
            }
            let Some(current) = self.current.as_mut() else {
                return self.fail(SourceBackedRelationalCatchUpError::InvalidMetadata(
                    "Core source stream was not initialized".to_owned(),
                ));
            };
            match current.next_record(self.index, self.page_size) {
                Ok(Some((record, page_items))) => {
                    if let Some(page_items) = page_items {
                        self.observe_page_items(page_items);
                    }
                    return Some(Ok(record));
                }
                Ok(None) => self.current = None,
                Err(error) => return self.fail(error),
            }
        }
    }
}

impl RelationalRecordStream<'_> {
    fn fail(
        &mut self,
        error: SourceBackedRelationalCatchUpError,
    ) -> Option<std::result::Result<RelationalProjectionRecord, RelationalProjectionError>> {
        self.failed = true;
        Some(Err(RelationalProjectionError::InvalidRecord(
            error.to_string(),
        )))
    }
}

struct SourceRecordStream {
    source: RelationalSourceMetadata,
    stage: SourceRecordStage,
    page: CorePageStream,
}

impl SourceRecordStream {
    fn new(
        source: RelationalSourceMetadata,
        page: CoreSourceEventPage,
    ) -> std::result::Result<Self, SourceBackedRelationalCatchUpError> {
        Ok(Self {
            source,
            stage: SourceRecordStage::Begin,
            page: CorePageStream::from_page(page)?,
        })
    }

    fn next_record(
        &mut self,
        index: &VerifiedIndex,
        page_size: usize,
    ) -> std::result::Result<
        Option<(RelationalProjectionRecord, Option<usize>)>,
        SourceBackedRelationalCatchUpError,
    > {
        loop {
            match self.stage {
                SourceRecordStage::Begin => {
                    self.stage = SourceRecordStage::Records;
                    return Ok(Some((
                        RelationalProjectionRecord::BeginSource(Box::new(self.source.clone())),
                        None,
                    )));
                }
                SourceRecordStage::Records => {
                    if let Some(record) = self.page.items.next() {
                        return Ok(Some((
                            RelationalProjectionRecord::CoreRecord(Box::new(record.core_record)),
                            None,
                        )));
                    }
                    if self.page.terminal {
                        self.stage = SourceRecordStage::End;
                        continue;
                    }
                    let page = load_source_page(
                        index,
                        &self.source.source,
                        self.page.cursor.as_ref(),
                        page_size,
                    )?;
                    let page_items = page.items.len();
                    self.page.replace_page(page)?;
                    if let Some(record) = self.page.items.next() {
                        return Ok(Some((
                            RelationalProjectionRecord::CoreRecord(Box::new(record.core_record)),
                            Some(page_items),
                        )));
                    }
                    self.stage = SourceRecordStage::End;
                    return self
                        .next_record(index, page_size)
                        .map(|record| record.map(|(record, _)| (record, Some(page_items))));
                }
                SourceRecordStage::End => {
                    self.stage = SourceRecordStage::Done;
                    return Ok(Some((
                        RelationalProjectionRecord::EndSource {
                            source_id: self.source.source.identity().as_uuid(),
                        },
                        None,
                    )));
                }
                SourceRecordStage::Done => return Ok(None),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SourceRecordStage {
    Begin,
    Records,
    End,
    Done,
}

struct CorePageStream {
    cursor: Option<SourceEventCursor>,
    items: std::vec::IntoIter<ctx_history_index::CoreEventRecord>,
    terminal: bool,
}

impl CorePageStream {
    fn from_page(
        page: CoreSourceEventPage,
    ) -> std::result::Result<Self, SourceBackedRelationalCatchUpError> {
        let mut stream = Self {
            cursor: None,
            items: Vec::new().into_iter(),
            terminal: false,
        };
        stream.replace_page(page)?;
        Ok(stream)
    }

    fn replace_page(
        &mut self,
        page: CoreSourceEventPage,
    ) -> std::result::Result<(), SourceBackedRelationalCatchUpError> {
        self.terminal = page.terminal;
        self.cursor = if page.terminal {
            None
        } else {
            Some(next_page_cursor(&page)?)
        };
        self.items = page.items.into_iter();
        if self.items.len() == 0 && !self.terminal {
            return Err(SourceBackedRelationalCatchUpError::InvalidMetadata(
                "non-terminal Core page is empty".to_owned(),
            ));
        }
        Ok(())
    }
}

fn load_source_page(
    index: &VerifiedIndex,
    source: &SourceKey,
    cursor: Option<&SourceEventCursor>,
    page_size: usize,
) -> std::result::Result<CoreSourceEventPage, SourceBackedRelationalCatchUpError> {
    let page = index
        .core_source_event_page(source, cursor, page_size)
        .map_err(|error| {
            SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                "enumerate Core source {}: {error}",
                source.identity()
            ))
        })?;
    if page.generation_id != index.generation_id() || !page.source.exact_descriptor_eq(source) {
        return Err(SourceBackedRelationalCatchUpError::GenerationMismatch {
            expected: index.generation_id().to_owned(),
            actual: page.generation_id,
        });
    }
    Ok(page)
}

fn next_page_cursor(
    page: &CoreSourceEventPage,
) -> std::result::Result<SourceEventCursor, SourceBackedRelationalCatchUpError> {
    page.next_cursor.clone().ok_or_else(|| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(
            "non-terminal Core page has no cursor".to_owned(),
        )
    })
}
