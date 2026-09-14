//! Shared human-terminal rendering foundation.
//!
//! Machine-readable output uses Ui's explicit byte-delivery methods whenever a
//! command needs selected-stream delivery. Those bytes remain opaque to the
//! document renderer while retaining the same injected-stream contract.

mod bootstrap;
mod components;
mod context;
mod document;
mod glyph;
mod style;
mod writer;

pub use bootstrap::{bootstrap_color_choice, scan_color_mode, scan_machine_output_hint};
pub use components::{
    callout, diagnostic, display_width, empty_state, evidence_list, fields, hint, is_copyable_atom,
    outcome, progress, refresh_progress, section, table, Action, Callout, CalloutPresentation,
    CalloutRow, CalloutStatus, Diagnostic, DiagnosticLevel, EmptyState, Evidence, Field, Hint,
    Outcome, OutcomeState, Progress, RefreshCurrentSourceProgress,
    RefreshCurrentSourceProgressStage, RefreshLogicalPhase, RefreshLogicalStatus, RefreshProgress,
    RefreshProgressSnapshot, RefreshRequestState, RefreshStatusKind, RefreshStructuredOutcome,
    RefreshTerminalPresentation, RefreshWholeRunStage, Table,
};
pub use context::{ColorMode, RenderContext, StreamKind, TestContext};
pub use document::{sanitize_untrusted_history_body_for_terminal, Document, Line, Span};
pub use style::{trim_terminal_line_ends, Token};
pub use writer::{LiveOutput, Ui};

/// Estimates one logical human result in a fixed, unbounded plain context.
///
/// This is useful for deterministic component tests and local size decisions.
/// Dispatch measures the actual wrapped, styled stdout and stderr bytes used
/// for runtime delivery accounting.
pub fn canonical_human_output_bytes(render: impl FnOnce(&RenderContext) -> Document) -> usize {
    render(&RenderContext::canonical_human_measurement())
        .render_plain()
        .len()
}

#[cfg(test)]
mod tests;
