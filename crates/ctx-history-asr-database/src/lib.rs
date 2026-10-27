//! Native database ASR converters: independent read transactions and capture snapshots.
pub mod batch8;
pub mod relational;
pub mod sqlite;
use ctx_history_asr_core::{
    content, privacy, records, source, spool, Error, Input, Options, Provider, Result, Summary,
};
