use super::*;

#[derive(Default)]
struct EncodedLengthWriter {
    bytes: usize,
}

impl Write for EncodedLengthWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("encoded length overflowed"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn canonical_encoded_len(value: &impl Serialize) -> Result<usize, ProtocolError> {
    let mut writer = EncodedLengthWriter::default();
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "Core prepared event delta page encoding failed",
        )
    })?;
    Ok(writer.bytes)
}
