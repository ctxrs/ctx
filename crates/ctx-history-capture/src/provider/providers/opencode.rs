pub(crate) mod native_path;
mod normalization;
mod schema;

pub(crate) use schema::{
    OpenCodeSqliteDialect, KILO_SQLITE_DIALECT, MIMOCODE_SQLITE_DIALECT, OPENCODE_SQLITE_DIALECT,
};
