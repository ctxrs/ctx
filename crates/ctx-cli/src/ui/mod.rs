//! Shared human-terminal rendering foundation.
//!
//! Machine-readable output deliberately bypasses this module. Commands will
//! migrate to this internal surface separately; keeping it registered but
//! otherwise unused in this patch lets the foundation land without changing
//! existing output contracts.

mod bootstrap;
mod components;
mod context;
mod document;
mod glyph;
mod style;
mod writer;

pub(crate) use bootstrap::bootstrap_color_choice;
pub(crate) use components::{
    diagnostic, empty_state, evidence_list, fields, hint, outcome, progress, section, table,
    Action, Diagnostic, DiagnosticLevel, EmptyState, Evidence, Field, Hint, Outcome, OutcomeState,
    Progress, Table,
};
pub(crate) use context::{ColorMode, RenderContext, StreamKind, TestContext};
pub(crate) use document::{Document, Line, Span};
pub(crate) use style::{Token, CLAP_STYLES};
pub(crate) use writer::Ui;

#[cfg(test)]
mod tests;
