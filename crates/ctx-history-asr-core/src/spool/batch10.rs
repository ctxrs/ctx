use super::*;
impl Spool {
    pub fn insert_document_item(
        &mut self,
        kind: &str,
        value: Value,
        native: Option<String>,
        snapshot: bool,
    ) -> Result<()> {
        self.source_lines += 1;
        let pos = self.source_lines;
        let row = json!({"kind":kind,"value":value,"nativeId":native,"snapshot":snapshot});
        crate::privacy::questions(self, &row)?;
        let off = self.bytes.stream_position()?;
        let raw = serde_json::to_vec(&row)?;
        self.bytes.write_all(&raw)?;
        self.db.execute(
            "INSERT INTO rows(pos,off,len,kind,native_id,observed) VALUES(?1,?2,?3,?4,?5,?1)",
            params![pos, off, raw.len(), kind, native],
        )?;
        Ok(())
    }
}
