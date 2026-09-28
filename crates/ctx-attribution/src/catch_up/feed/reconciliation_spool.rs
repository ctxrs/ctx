use crate::materializer::{SegmentMaterializerError, runtime_file::RuntimeFile};
use crate::protocol::{
    CoreSourceReconciliation, MAX_CORE_CONTROL_WIRE_BYTES, MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
};

/// A bounded acknowledgement page at a time, until source reconciliation is
/// terminal and the existing ordered event consumer may start. Cursor hashes
/// retained by the session authenticate each replayed source selection.
pub(super) struct ReconciliationSpool {
    file: RuntimeFile,
    bytes: u64,
    offset: u64,
    count: u32,
    consumed: u32,
}

impl ReconciliationSpool {
    pub(super) fn new(file: RuntimeFile) -> Self {
        Self {
            file,
            bytes: 0,
            offset: 0,
            count: 0,
            consumed: 0,
        }
    }

    pub(super) fn count(&self) -> u32 {
        self.count
    }

    pub(super) fn append(
        &mut self,
        items: &[CoreSourceReconciliation],
    ) -> Result<(), SegmentMaterializerError> {
        if items.is_empty() {
            return Ok(());
        }
        if items.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS {
            return Err(SegmentMaterializerError::Bounds);
        }
        let mut count = self.count;
        for item in items {
            if item.materialize_index != count {
                return Err(SegmentMaterializerError::Conflict);
            }
            count = count
                .checked_add(1)
                .ok_or(SegmentMaterializerError::Bounds)?;
        }
        let bytes = serde_json::to_vec(items).map_err(|_| SegmentMaterializerError::Encoding)?;
        if bytes.len() > MAX_CORE_CONTROL_WIRE_BYTES {
            return Err(SegmentMaterializerError::Bounds);
        }
        let length = u32::try_from(bytes.len()).map_err(|_| SegmentMaterializerError::Bounds)?;
        let end = self
            .bytes
            .checked_add(4)
            .and_then(|n| n.checked_add(u64::from(length)))
            .ok_or(SegmentMaterializerError::Bounds)?;
        if self.file.append(&length.to_le_bytes())? != self.bytes {
            return Err(SegmentMaterializerError::Corrupt(
                "reconciliation spill length changed",
            ));
        }
        self.file.append(&bytes)?;
        self.bytes = end;
        self.count = count;
        Ok(())
    }

    pub(super) fn next_batch(
        &mut self,
    ) -> Result<Option<Vec<CoreSourceReconciliation>>, SegmentMaterializerError> {
        if self.offset == self.bytes {
            if self.consumed != self.count {
                return Err(SegmentMaterializerError::Conflict);
            }
            return Ok(None);
        }
        let mut length = [0; 4];
        self.file.read_at(self.offset, &mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        let payload = self
            .offset
            .checked_add(4)
            .ok_or(SegmentMaterializerError::Bounds)?;
        let end = payload
            .checked_add(length as u64)
            .ok_or(SegmentMaterializerError::Bounds)?;
        if length > MAX_CORE_CONTROL_WIRE_BYTES || end > self.bytes || end > self.file.byte_len()? {
            return Err(SegmentMaterializerError::Corrupt(
                "reconciliation spill frame length",
            ));
        }
        let mut bytes = vec![0; length];
        self.file.read_at(payload, &mut bytes)?;
        let items: Vec<CoreSourceReconciliation> =
            serde_json::from_slice(&bytes).map_err(|_| SegmentMaterializerError::Encoding)?;
        if items.is_empty() || items.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS {
            return Err(SegmentMaterializerError::Corrupt(
                "reconciliation spill page count",
            ));
        }
        for item in &items {
            if item.materialize_index != self.consumed || self.consumed >= self.count {
                return Err(SegmentMaterializerError::Conflict);
            }
            self.consumed += 1;
        }
        self.offset = end;
        Ok(Some(items))
    }
}
