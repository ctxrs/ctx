use anyhow::Result;
use ctx_pro_host_protocol::BlameResult;
use serde_json::Value;

use crate::ui::{canonical_human_output_bytes, Document, RenderContext, Ui};

mod commit;
mod evidence;
mod file;
mod human;
mod layout;
mod pull_request;
mod relationships;
mod target;

#[must_use]
pub(crate) fn blame_result_json(result: &BlameResult) -> Value {
    serde_json::to_value(result).unwrap_or(Value::Null)
}

/// Emits one blame result and returns its canonical, color-independent byte
/// count. Machine output intentionally bypasses the terminal UI.
pub(crate) fn print_blame_result(
    result: &BlameResult,
    json_output: bool,
    ui: &mut Ui,
) -> Result<usize> {
    if json_output {
        let mut rendered = serde_json::to_vec_pretty(result)?;
        rendered.push(b'\n');
        ui.stdout_writer().write_all(&rendered)?;
        return Ok(rendered.len());
    }

    let document = render_blame_document(result, ui.stdout_context());
    let plain_bytes =
        canonical_human_output_bytes(|context| render_blame_document(result, context));
    ui.write_stdout(&document)?;
    Ok(plain_bytes)
}

fn render_blame_document(result: &BlameResult, context: &RenderContext) -> Document {
    human::render(result, context)
}

#[cfg(test)]
mod tests;
