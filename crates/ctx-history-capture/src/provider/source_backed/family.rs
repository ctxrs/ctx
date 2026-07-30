//! Shared physical source-family engines.
//!
//! Families own bounded source access and replay evidence. Provider adapters
//! retain every semantic decision, including parsing, identity, counters, and
//! projection.

pub(crate) mod document;
pub(crate) mod jsonl;

#[cfg(test)]
#[path = "family/tests.rs"]
mod tests;
