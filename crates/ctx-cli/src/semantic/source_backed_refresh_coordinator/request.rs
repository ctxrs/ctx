use super::*;

/// The caller's requested Core operation, independent of how it observes the
/// daemon attempt and whether it supplied an explicit catalog snapshot.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshOperation {
    Refresh,
    Import,
}

impl SourceBackedRefreshOperation {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Import => "import",
        }
    }

    pub(super) fn from_request_json(request: &Value) -> Result<Self> {
        match request.get("operation").and_then(Value::as_str) {
            Some("refresh") => Ok(Self::Refresh),
            Some("import") => Ok(Self::Import),
            Some(operation) => Err(anyhow!(
                "invalid daemon source refresh operation `{operation}`"
            )),
            None => Err(anyhow!(
                "daemon source refresh request operation is missing"
            )),
        }
    }
}

/// Typed source-refresh IPC request. Import requests resolve an omitted
/// catalog to one exact snapshot before serialization so intent and catalog
/// authority cannot be inferred differently by the client and daemon.
pub(super) struct SourceBackedRefreshRequest<'a> {
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
}

impl<'a> SourceBackedRefreshRequest<'a> {
    pub(super) const fn new(
        mode: SourceBackedRefreshMode,
        operation: SourceBackedRefreshOperation,
        explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    ) -> Self {
        Self {
            mode,
            operation,
            explicit_source_catalog,
        }
    }

    pub(super) fn to_json(&self, data_root: &Path) -> Result<Value> {
        let implicit_catalog = if self.operation == SourceBackedRefreshOperation::Import
            && self.explicit_source_catalog.is_none()
        {
            Some(
                load_explicit_source_catalog_authority(data_root)
                    .context("load implicit catalog authority for import refresh request")?,
            )
        } else {
            None
        };
        let catalog = self.explicit_source_catalog.or(implicit_catalog.as_ref());
        Ok(compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": self.mode.as_str(),
            "operation": self.operation.as_str(),
            "explicit_source_catalog": catalog
                .map(ExplicitSourceCatalogAuthority::to_json),
        })))
    }
}
