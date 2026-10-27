//! Native SQLite snapshots. SQL row order is serialization, never cross-store causality.
mod common;
mod context;
pub use context::manifest;
mod crush;
mod goose;
mod header;
mod lingma;
mod store;
mod warp;
use crate::{records::Writer, spool::Spool, Result};
pub use header::{capture_header, header};
use serde_json::{json, Value};
pub use store::{check_source, ingest, namespace, visit};

pub fn convert(spool: &mut Spool, writer: &mut Writer, pos: u64, row: &Value) -> Result<()> {
    writer.snapshot_position(
        pos,
        row["_key"].as_str().unwrap_or("unknown"),
        row["_capture"] == true,
    );
    if row["_gap"].is_string() {
        let mut gap = json!({"type":"gap","reason":row["_gap"],"count":1});
        common::row_meta(spool, &mut gap, writer.provider, row)?;
        writer.emit(gap, &Value::Null)?;
        return Ok(());
    }
    match writer.provider {
        crate::Provider::Crush => crush::convert(spool, writer, row),
        crate::Provider::Goose => goose::convert(spool, writer, row),
        crate::Provider::Lingma => lingma::convert(spool, writer, row),
        crate::Provider::Warp => warp::convert(spool, writer, row),
        _ => unreachable!("SQLite converter dispatch"),
    }
}
