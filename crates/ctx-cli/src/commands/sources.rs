use anyhow::Result;
pub(crate) use ctx_cli_presentation::commands::sources::{
    SourcesArgs, SourcesCommand, SourcesEnvironment,
};

pub(crate) fn run_sources(
    mut args: SourcesArgs,
    environment: SourcesEnvironment,
    telemetry: &mut crate::analytics::SourcesTelemetry,
    local_usage: &mut crate::local_usage::CliUsage,
    ui: &mut ctx_terminal::Ui,
) -> Result<()> {
    let Some(command) = args.command.take() else {
        return ctx_cli_presentation::commands::sources::run_sources(
            args,
            environment,
            telemetry,
            local_usage,
            ui,
        );
    };
    if args.provider.is_some() || args.all || args.show_missing {
        anyhow::bail!("source listing filters cannot be combined with add or remove");
    }
    let operation = match &command {
        SourcesCommand::Add { .. } => "add",
        SourcesCommand::Remove { .. } => "remove",
    };
    let mutation = match command {
        SourcesCommand::Add {
            name,
            provider,
            root,
            source_group,
        } => crate::config::add_provider_root(
            &environment.data_root,
            &name,
            provider.capture_provider(),
            &root,
            source_group.as_deref(),
        )?,
        SourcesCommand::Remove { name } => {
            crate::config::remove_provider_root(&environment.data_root, &name)?
        }
    };
    let value = serde_json::json!({
        "schema_version": 1,
        "operation": operation,
        "changed": mutation.changed,
        "root": {
            "name": mutation.root.id.clone(),
            "provider": mutation.root.provider.as_str(),
            "path": mutation.root.path.clone(),
            "group": mutation.root.group.clone(),
        }
    });
    if args.format.is_json() {
        ctx_terminal::print_json(value)?;
    } else {
        let document = crate::ui::Document::from_line(crate::ui::Line::text(format!(
            "{} provider root '{}' ({})",
            match (operation, mutation.changed) {
                ("add", true) => "Added",
                ("remove", true) => "Removed",
                (_, false) => "Kept",
                _ => "Updated",
            },
            mutation.root.id,
            mutation.root.path.display()
        )));
        ui.write_stdout(&document)?;
    }
    Ok(())
}
