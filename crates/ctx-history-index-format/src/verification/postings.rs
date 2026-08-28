use tantivy::{
    postings::TermInfo, schema::IndexRecordOption, DocAddress, DocSet, InvertedIndexReader,
    SegmentReader, TERMINATED,
};
use uuid::Uuid;

use crate::{IndexError, Result};

pub(super) fn for_each_live_posting(
    inverted: &InvertedIndexReader,
    term_info: &TermInfo,
    segment_ord: usize,
    segment: &SegmentReader,
    mut visit: impl FnMut(DocAddress) -> Result<()>,
) -> Result<()> {
    let mut postings = inverted.read_postings_from_terminfo(term_info, IndexRecordOption::Basic)?;
    let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
    let mut doc_id = postings.doc();
    while doc_id != TERMINATED {
        if !segment.is_deleted(doc_id) {
            visit(DocAddress::new(segment_ord, doc_id))?;
        }
        doc_id = postings.advance();
    }
    Ok(())
}

pub(super) fn canonical_uuid_term(term: &[u8], field: &'static str) -> Result<Uuid> {
    let term =
        std::str::from_utf8(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    let uuid = Uuid::parse_str(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    if uuid.to_string() != term {
        return Err(IndexError::InvalidStoredDocumentField(field));
    }
    Ok(uuid)
}
