use super::*;

/// The caller's requested Core operation, independent of its process or wire transport.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshOperation {
    Refresh,
    Import,
}

impl RefreshOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Import => "import",
        }
    }

    pub fn from_str(value: &str) -> Result<Self> {
        match value {
            "refresh" => Ok(Self::Refresh),
            "import" => Ok(Self::Import),
            operation => Err(anyhow!("invalid source refresh operation `{operation}`")),
        }
    }

    pub(crate) fn from_request_json(request: &Value) -> Result<Self> {
        request
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("daemon source refresh request operation is missing"))
            .and_then(Self::from_str)
    }
}

pub(crate) type SourceBackedRefreshOperation = RefreshOperation;

/// Process-neutral facts required to submit one logical refresh request.
#[derive(Debug, Clone)]
pub struct RefreshSubmission {
    pub(crate) request_id: String,
    pub(crate) operation: RefreshOperation,
    pub(crate) explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(crate) refresh_scope: SourceBackedRefreshScope,
    pub(crate) fresh_after_admitted_snapshot: bool,
    pub(crate) maintenance_wake: bool,
}

impl RefreshSubmission {
    pub fn new(
        request_id: String,
        operation: RefreshOperation,
        explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
        refresh_scope: SourceBackedRefreshScope,
        fresh_after_admitted_snapshot: bool,
        maintenance_wake: bool,
    ) -> Self {
        Self {
            request_id,
            operation,
            explicit_source_catalog,
            refresh_scope,
            fresh_after_admitted_snapshot,
            maintenance_wake,
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }
}

/// Typed engine status. Schema-v1 rendering remains owned by the process
/// adapter so transport code never reaches into coordinator state.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshStatus {
    schema_v1_fields: Value,
}

impl RefreshStatus {
    pub(crate) fn from_schema_v1_fields(fields: Value) -> Self {
        Self {
            schema_v1_fields: fields,
        }
    }

    #[doc(hidden)]
    pub fn schema_v1_fields(&self) -> &Value {
        &self.schema_v1_fields
    }

    pub fn request_id(&self) -> Option<&str> {
        self.schema_v1_fields
            .get("request_id")
            .and_then(Value::as_str)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl std::ops::Index<&str> for RefreshStatus {
    type Output = Value;

    fn index(&self, field: &str) -> &Self::Output {
        &self.schema_v1_fields[field]
    }
}

/// Admission hold released only after the hosting process attempts to write
/// the typed acknowledgement to its transport.
#[derive(Debug)]
pub struct AdmissionResponseBarrier {
    request_id: Option<String>,
}

impl AdmissionResponseBarrier {
    pub(crate) fn new(request_id: String) -> Self {
        Self {
            request_id: Some(request_id),
        }
    }

    pub fn release(mut self, engine: &crate::RefreshEngine) {
        if let Some(request_id) = self.request_id.take() {
            engine.release_admission_response(&request_id);
        }
    }
}

/// Result of one durable submission reservation.
#[derive(Debug)]
pub struct RefreshAdmission {
    status: RefreshStatus,
    response_barrier: Option<AdmissionResponseBarrier>,
}

impl RefreshAdmission {
    pub(crate) fn new(
        status: RefreshStatus,
        response_barrier: Option<AdmissionResponseBarrier>,
    ) -> Self {
        Self {
            status,
            response_barrier,
        }
    }

    pub fn status(&self) -> &RefreshStatus {
        &self.status
    }

    pub fn into_parts(self) -> (RefreshStatus, Option<AdmissionResponseBarrier>) {
        (self.status, self.response_barrier)
    }
}
