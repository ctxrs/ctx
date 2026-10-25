use anyhow::Result;
use ctx_terminal::{
    fields, outcome, print_json, Document, Field, JsonOutputFormat, Outcome, OutcomeState,
    RenderContext, Token, Ui,
};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn run(format: JsonOutputFormat, ui: &mut Ui) -> Result<()> {
    let daemons = collect()?;
    if format.is_json() {
        print_json(json!({
            "schema_version": 1,
            "daemons": daemons.iter().map(|(root, daemon)| json!({
                "data_root": root,
                "daemon": daemon,
            })).collect::<Vec<_>>(),
        }))?;
    } else {
        ui.write_stdout(&render(ui.stdout_context(), &daemons))?;
    }
    Ok(())
}

fn collect() -> Result<Vec<(PathBuf, Value)>> {
    Ok(
        crate::daemon_autostart::registered_installation_daemon_roots()?
            .into_iter()
            .map(|root| {
                let report = crate::paths_status::daemon_report_read_only(&root);
                (root, report)
            })
            .collect(),
    )
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
#[path = "registered_tests.rs"]
mod tests;
