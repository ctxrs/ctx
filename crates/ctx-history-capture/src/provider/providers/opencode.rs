pub(crate) mod native_path;
mod normalization;
mod schema;

#[allow(unused_imports)] // Consumed by the pending shared provider registration seam.
pub(crate) use native_path::source_backed::{
    kilo_source_backed_registration, mimocode_source_backed_registration,
    opencode_family_source_backed_registrations, opencode_source_backed_registration,
    OpenCodeSourceBackedError, OpenCodeSourceBackedRegistration, OpenCodeSourceBackedResult,
};
pub(crate) use schema::{
    OpenCodeSqliteDialect, KILO_SQLITE_DIALECT, MIMOCODE_SQLITE_DIALECT, OPENCODE_SQLITE_DIALECT,
};
