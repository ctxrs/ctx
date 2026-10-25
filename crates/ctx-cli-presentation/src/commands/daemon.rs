use anyhow::Result;
use ctx_terminal::{
    fields, outcome, print_json, Document, Field, JsonOutputFormat, Outcome, OutcomeState,
    RenderContext, Token, Ui,
};
use serde_json::{json, Value};

pub fn run_registered_status(
    daemons: &[Value],
    format: JsonOutputFormat,
    ui: &mut Ui,
) -> Result<()> {
    if format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemons": daemons,
        }))?;
    } else {
        ui.write_stdout(&render(ui.stdout_context(), daemons))?;
    }
    Ok(())
}

fn render(context: &RenderContext, daemons: &[Value]) -> Document {
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
    for row in daemons {
        let daemon = &row["daemon"];
        let state = if daemon["running"] == true {
            "Running"
        } else if daemon["status"] == "unverified" || row["error"].is_string() {
            "Unverified"
        } else {
            "Stopped"
        };
        document.push_blank();
        document.append(fields(
            context,
            &[
                Field::new("Data root", row["data_root"].as_str().unwrap_or("Unknown"))
                    .with_value_token(Token::Reference),
                Field::new("Daemon", state),
            ],
        ));
        if let Some(error) = row["error"].as_str() {
            document.append(fields(
                context,
                &[
                    Field::new(
                        "Registration",
                        row["registration_path"].as_str().unwrap_or("Unknown"),
                    )
                    .with_value_token(Token::Reference),
                    Field::new("Error", error),
                ],
            ));
        }
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
                    json!({"data_root": "/scratch/two  spaces\u{1b}", "daemon": {"running": false, "status": "unverified"}}),
                    json!({"data_root": "/scratch/active", "daemon": {"running": true, "status": "running"}}),
                    json!({"data_root": "old-relative", "daemon": null, "registration_path": "/registry/invalid.json", "error": "invalid registration"}),
                    json!({"data_root": null, "daemon": null, "registration_path": "/registry/unreadable.json", "error": "unreadable registration"}),
                ],
            )
            .render_plain();
            assert!(rendered.contains("/scratch/two  spaces\\x1b"), "{rendered}");
            assert!(rendered.contains("Unverified"), "{rendered}");
            assert!(rendered.contains("Running"), "{rendered}");
            assert!(rendered.contains("old-relative"), "{rendered}");
            assert!(rendered.contains("Unknown"), "{rendered}");
            assert!(rendered.contains("/registry/unreadable.json"), "{rendered}");
            assert!(
                rendered
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("invalid registration"),
                "{rendered}"
            );
            assert!(!rendered.contains('\u{1b}'));
            assert!(render(&context, &[])
                .render_plain()
                .contains("No registered daemons"));
        }
    }
}
