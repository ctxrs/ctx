//! Nonwaiting analytics identity admission. Product callers retain the ordinary
//! durable identity APIs; an optional observer can simply miss a busy update.
use super::*;

#[cfg(test)]
mod tests;
