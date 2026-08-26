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
            kind,
            replace,
        } => crate::config::add_provider_root_with_kind(
            &environment.data_root,
            &name,
            provider.capture_provider(),
            &root,
            source_group.as_deref(),
            kind,
            replace,
        )?,
        SourcesCommand::Remove { name } => {
            crate::config::remove_provider_root(&environment.data_root, &name)?
        }
    };
    let value = provider_root_mutation_json(operation, &mutation);
    if args.format.is_json() {
        ctx_terminal::print_json(value)?;
    } else {
        ui.write_stdout(&provider_root_mutation_human(operation, &mutation))?;
    }
    Ok(())
}

fn provider_root_mutation_json(
    operation: &str,
    mutation: &crate::config::ProviderRootMutation,
) -> serde_json::Value {
    let mut root = serde_json::json!({
        "name": mutation.root.id.clone(),
        "provider": mutation.root.provider.as_str(),
        "path": mutation.root.path.clone(),
        "group": mutation.root.group.clone(),
    });
    if mutation.root.provider == ctx_history_core::CaptureProvider::OpenHands {
        root["kind"] = serde_json::json!(mutation.root.kind.map(|kind| kind.as_str()));
    }
    serde_json::json!({
        "schema_version": 1,
        "operation": operation,
        "changed": mutation.changed,
        "root": root,
    })
}

fn provider_root_mutation_human(
    operation: &str,
    mutation: &crate::config::ProviderRootMutation,
) -> crate::ui::Document {
    crate::ui::Document::from_line(crate::ui::Line::text(format!(
        "{} {} history root '{}' ({})",
        match (operation, mutation.changed, mutation.replaced) {
            ("add", true, true) => "Replaced",
            ("add", true, false) => "Added",
            ("remove", true, _) => "Removed",
            (_, false, _) => "Kept",
            _ => "Updated",
        },
        mutation.root.provider.as_str(),
        mutation.root.id,
        mutation.root.path.display()
    )))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ctx_history_capture::ProviderRootDefinition;
    use ctx_history_core::CaptureProvider;
    use serde_json::json;

    use super::*;

    fn mutation(changed: bool, replaced: bool) -> crate::config::ProviderRootMutation {
        crate::config::ProviderRootMutation {
            root: ProviderRootDefinition {
                id: "work".to_owned(),
                provider: CaptureProvider::Claude,
                path: PathBuf::from("/history/work"),
                group: Some("team".to_owned()),
                kind: None,
            },
            changed,
            replaced,
        }
    }

    #[test]
    fn add_replacement_keeps_the_schema_v1_add_operation() {
        let value = provider_root_mutation_json("add", &mutation(true, true));
        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "operation": "add",
                "changed": true,
                "root": {
                    "name": "work",
                    "provider": "claude",
                    "path": PathBuf::from("/history/work"),
                    "group": "team",
                }
            })
        );
        assert!(value.get("replaced").is_none());

        let mut cleared = mutation(true, true);
        cleared.root.group = None;
        let cleared_value = provider_root_mutation_json("add", &cleared);
        assert!(cleared_value["root"]["group"].is_null());
        assert_eq!(cleared_value.as_object().unwrap().len(), 4);

        let unchanged_value = provider_root_mutation_json("add", &mutation(false, false));
        assert_eq!(unchanged_value["operation"], "add");
        assert_eq!(unchanged_value["changed"], false);

        let removed_value = provider_root_mutation_json("remove", &mutation(true, false));
        assert_eq!(removed_value["operation"], "remove");
        assert_eq!(removed_value["changed"], true);
        assert_eq!(removed_value.as_object().unwrap().len(), 4);
    }

    #[test]
    fn add_remove_and_noop_human_results_name_the_specific_provider_history_root() {
        let cases = [
            ("add", mutation(true, false), "Added"),
            ("add", mutation(true, true), "Replaced"),
            ("add", mutation(false, false), "Kept"),
            ("remove", mutation(true, false), "Removed"),
        ];
        for (operation, mutation, verb) in cases {
            let rendered = provider_root_mutation_human(operation, &mutation).render_plain();
            assert!(
                rendered.contains(&format!("{verb} claude history root 'work'")),
                "{rendered}"
            );
            assert!(!rendered.contains("provider home"), "{rendered}");
        }
    }

    #[test]
    fn openhands_json_adds_kind_without_changing_old_provider_shape() {
        let mut openhands = mutation(true, false);
        openhands.root.provider = CaptureProvider::OpenHands;
        openhands.root.kind =
            Some(ctx_history_capture::ProviderRootKind::OpenHandsCurrentConversations);
        let value = provider_root_mutation_json("add", &openhands);
        assert_eq!(value["root"]["kind"], "current-conversations");
        assert!(
            provider_root_mutation_json("add", &mutation(true, false))["root"]
                .get("kind")
                .is_none()
        );
    }
}
