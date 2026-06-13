#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryExportErrorKind {
    NotFound,
}

#[derive(Debug, Clone)]
pub struct TelemetryExportError {
    kind: TelemetryExportErrorKind,
}

impl TelemetryExportError {
    pub fn not_found() -> Self {
        Self {
            kind: TelemetryExportErrorKind::NotFound,
        }
    }

    pub fn kind(&self) -> TelemetryExportErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_export_not_found_error_preserves_status_kind() {
        let error = TelemetryExportError::not_found();

        assert_eq!(error.kind(), TelemetryExportErrorKind::NotFound);
    }
}
