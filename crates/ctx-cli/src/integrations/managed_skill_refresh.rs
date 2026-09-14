use ctx_agent_integrations::skill::{
    existing_managed_skill_refresh_required, refresh_existing_managed_skills, PathContext,
};
use ctx_cli_presentation::commands::SemanticCommand;
use ctx_upgrade_engine::{managed_install_marker_for_current_exe, ManagedInstallMarker};

use crate::{
    cli::{CommandRoot, DaemonCommand},
    commands::{search::CliRefreshArg, sources::SourcesCommand},
};

pub(crate) fn refresh_existing_managed_skills_on_startup(command: &CommandRoot) {
    if !command_is_refresh_eligible(command) {
        return;
    }
    let Ok(context) = PathContext::from_env_best_effort() else {
        return;
    };
    if !existing_managed_skill_refresh_required(&context) {
        return;
    }
    let Ok(ManagedInstallMarker::Valid(marker)) = managed_install_marker_for_current_exe() else {
        return;
    };
    if marker.version != env!("CARGO_PKG_VERSION") {
        return;
    }
    refresh_existing_managed_skills(&context, env!("CARGO_PKG_VERSION"));
}

fn command_is_refresh_eligible(command: &CommandRoot) -> bool {
    match command {
        CommandRoot::Pro | CommandRoot::Blame | CommandRoot::Setup(_) | CommandRoot::Import(_) => {
            true
        }
        CommandRoot::Semantic(args) => !matches!(&args.command, SemanticCommand::Status(_)),
        CommandRoot::Sources(args) => matches!(
            &args.command,
            Some(SourcesCommand::Add { .. } | SourcesCommand::Remove { .. })
        ),
        CommandRoot::Search(args) => args.refresh != CliRefreshArg::Off,
        CommandRoot::Daemon(args) => matches!(
            &args.command,
            DaemonCommand::Run(_) | DaemonCommand::Enable(_)
        ),
        CommandRoot::Referral
        | CommandRoot::Status(_)
        | CommandRoot::Stats(_)
        | CommandRoot::Index(_)
        | CommandRoot::Show(_)
        | CommandRoot::List(_)
        | CommandRoot::Locate(_)
        | CommandRoot::Docs(_)
        | CommandRoot::Integrations(_)
        | CommandRoot::Mcp(_)
        | CommandRoot::Upgrade(_)
        | CommandRoot::Doctor(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;
    use crate::Cli;

    fn eligible(arguments: &[&str]) -> bool {
        let cli = Cli::try_parse_from(arguments).expect("test command should parse");
        command_is_refresh_eligible(&cli.command)
    }

    #[test]
    fn ordinary_mutation_startups_are_refresh_eligible() {
        for arguments in [
            &["ctx", "setup"][..],
            &["ctx", "pro"][..],
            &["ctx", "blame"][..],
            &["ctx", "import", "--all"][..],
            &["ctx", "search", "needle"][..],
            &["ctx", "search", "needle", "--refresh", "wait"][..],
            &["ctx", "semantic", "enable"][..],
            &["ctx", "semantic", "disable"][..],
            &[
                "ctx",
                "sources",
                "add",
                "work",
                "--provider",
                "codex",
                "--root",
                "/tmp/codex",
            ][..],
            &["ctx", "sources", "remove", "work"][..],
            &["ctx", "daemon", "run"][..],
            &["ctx", "daemon", "enable"][..],
        ] {
            assert!(
                eligible(arguments),
                "expected eligibility for {arguments:?}"
            );
        }
    }

    #[test]
    fn observational_and_specialized_commands_do_not_refresh() {
        for arguments in [
            &["ctx", "status"][..],
            &["ctx", "status", "--usage", "enable"][..],
            &["ctx", "stats"][..],
            &["ctx", "index"][..],
            &["ctx", "sources"][..],
            &["ctx", "show", "event", "deadbeef"][..],
            &["ctx", "list", "events"][..],
            &["ctx", "locate", "event", "deadbeef"][..],
            &["ctx", "docs", "list"][..],
            &["ctx", "integrations", "status", "skill"][..],
            &["ctx", "mcp", "serve"][..],
            &["ctx", "daemon", "status"][..],
            &["ctx", "daemon", "disable"][..],
            &["ctx", "upgrade", "check"][..],
            &["ctx", "upgrade", "enable"][..],
            &["ctx", "upgrade", "disable"][..],
            &["ctx", "doctor"][..],
            &["ctx", "semantic", "status"][..],
        ] {
            assert!(
                !eligible(arguments),
                "unexpected eligibility for {arguments:?}"
            );
        }
    }

    #[test]
    fn search_refresh_off_is_observational() {
        assert!(!eligible(&["ctx", "search", "needle", "--refresh", "off",]));
    }
}
