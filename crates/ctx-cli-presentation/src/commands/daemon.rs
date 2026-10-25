use anyhow::Result;
use ctx_terminal::{
    fields, outcome, print_json, Document, Field, JsonOutputFormat, Outcome, OutcomeState,
    RenderContext, Token, Ui,
};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn run_registered_status(
    daemons: &[(PathBuf, Value)],
    format: JsonOutputFormat,
    ui: &mut Ui,
) -> Result<()> {
    if format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemons": daemons.iter().map(|(root, daemon)| json!({
                "data_root": root,
                "daemon": daemon,
            })).collect::<Vec<_>>(),
        }))?;
    } else {
        ui.write_stdout(&render(ui.stdout_context(), daemons))?;
    }
    Ok(())
}

fn render(context: &RenderContext, daemons: &[(PathBuf, Value)]) -> Document {
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Neutral,
            title: if daemons.is_empty() {
                "No registered daemons"
            } else {
                "Registered daemons"
            },
            detail: Some("For this installation and managed data root."),
        },
    );
    for (root, daemon) in daemons {
        let state = if daemon["running"] == true {
            "Running"
        } else if daemon["status"] == "unverified" {
            "Unverified"
        } else {
            "Stopped"
        };
        document.push_blank();
        document.append(fields(
            context,
            &[
                Field::new("Data root", &root.display().to_string())
                    .with_value_token(Token::Reference),
                Field::new("Daemon", state),
            ],
        ));
    }
    document
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registered_root_rendering_preserves_paths_and_unverified_state() {
        use ctx_terminal::{ColorMode, StreamKind, TestContext};
        for width in [32, 48, 80, 120] {
            let context = RenderContext::for_test(
                TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never),
            );
            let rendered = render(
                &context,
                &[
                    (
                        PathBuf::from("/scratch/two  spaces\u{1b}"),
                        json!({"running": false, "status": "unverified"}),
                    ),
                    (
                        PathBuf::from("/scratch/active"),
                        json!({"running": true, "status": "running"}),
                    ),
                ],
            )
            .render_plain();
            assert!(rendered.contains("/scratch/two  spaces\\x1b"), "{rendered}");
            assert!(rendered.contains("Unverified"), "{rendered}");
            assert!(rendered.contains("Running"), "{rendered}");
            assert!(!rendered.contains('\u{1b}'));
            assert!(render(&context, &[])
                .render_plain()
                .contains("No registered daemons"));
        }
    }
}
