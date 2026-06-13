#[derive(Debug)]
pub struct TextRouteDownload {
    pub bytes: Vec<u8>,
    pub filename: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_route_download_preserves_payload_and_filename() {
        let download = TextRouteDownload {
            bytes: b"log".to_vec(),
            filename: "entry.log".to_string(),
        };

        assert_eq!(download.bytes, b"log");
        assert_eq!(download.filename, "entry.log");
    }
}
