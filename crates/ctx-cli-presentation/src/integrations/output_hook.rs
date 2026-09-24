use anyhow::{Result, bail};
use clap::Args;
use ctx_agent_integrations::{
    output_hook::{self, Agent, Context, State},
    skill::{SkillAgentArg, detected_agents},
};
use serde_json::json;

use crate::{
    analytics::{IntegrationScope, IntegrationTelemetry, TargetSelection, count_bucket},
    output::JsonOutputFormat,
    ui::Ui,
};

#[derive(Debug, Args)]
pub(crate) struct OutputHookArgs {
    #[arg(long = "agent", value_parser = ctx_agent_integrations::skill::parse_skill_agent,
        conflicts_with = "all_agents")]
    agent: Vec<SkillAgentArg>,
    #[arg(long, conflicts_with = "agent")]
    all_agents: bool,
    #[arg(long, help = "Use project hook config instead of global host config")]
    project: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    format: JsonOutputFormat,
}

impl OutputHookArgs {
    pub(crate) fn json_output(&self) -> bool {
        self.format.is_json()
    }

    pub(crate) fn add_initial_analytics(&self, telemetry: &mut IntegrationTelemetry) {
        telemetry.scope = Some(if self.project {
            IntegrationScope::Project
        } else {
            IntegrationScope::Global
        });
        telemetry.selection = Some(if self.all_agents {
            TargetSelection::All
        } else if self.agent.is_empty() {
            TargetSelection::Detected
        } else {
            TargetSelection::Explicit
        });
        telemetry.target_agents = Some(count_bucket(if self.all_agents {
            SkillAgentArg::ALL.len() as u64
        } else {
            self.agent.len() as u64
        }));
    }

    fn select(&self, context: &Context) -> Result<(Vec<Agent>, Vec<SkillAgentArg>)> {
        self.select_supported(context, Agent::supported)
    }

    fn select_supported(
        &self,
        context: &Context,
        supported: impl Fn(Agent) -> bool,
    ) -> Result<(Vec<Agent>, Vec<SkillAgentArg>)> {
        let skills = if self.all_agents {
            SkillAgentArg::ALL.to_vec()
        } else if self.agent.is_empty() {
            detected_agents(&context.paths)
        } else {
            self.agent.clone()
        };
        let mut selected = Vec::new();
        let mut skipped = Vec::new();
        for skill in skills {
            if let Some(agent) = Agent::from_skill(skill) {
                if !supported(agent) {
                    if !self.agent.is_empty() {
                        bail!(
                            "{} Sift hook is unsupported here: {}",
                            agent.name(),
                            agent.limitation()
                        );
                    }
                    if !skipped.contains(&skill) {
                        skipped.push(skill);
                    }
                } else if !selected.contains(&agent) {
                    selected.push(agent);
                }
            } else if !skipped.contains(&skill) {
                skipped.push(skill);
            }
        }
        if !self.agent.is_empty() && !skipped.is_empty() {
            bail!(
                "no native Sift hook for {}; supported: claude-code, github-copilot, codex",
                skipped
                    .iter()
                    .map(|a| a.id())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok((selected, skipped))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Install,
    Status,
    Remove,
}

pub(crate) fn run(
    args: OutputHookArgs,
    operation: Operation,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    args.add_initial_analytics(telemetry);
    let context = Context::from_env()?;
    let (selected, skipped) = args.select(&context)?;
    let mut rows = Vec::new();
    let mut failed = false;
    for skill in skipped {
        let detail =
            Agent::from_skill(skill).map_or("no supported native Sift hook", Agent::limitation);
        rows.push(json!({"agent":skill.id(), "status":"skipped", "detail":detail}));
    }
    for agent in selected {
        let result = match operation {
            Operation::Install => output_hook::install(agent, args.project, &context),
            Operation::Status => output_hook::status(agent, args.project, &context),
            Operation::Remove => output_hook::remove(agent, args.project, &context),
        };
        let (status, detail) = match result {
            Ok(State::Missing) => ("absent", String::new()),
            Ok(State::Current) => ("installed", agent.limitation().to_owned()),
            Ok(State::Legacy) => ("outdated", "legacy ctx output hook; reinstall with ctx integrations install sift".to_owned()),
            Ok(State::SiftConflict) => ("conflict", "standalone Sift hook detected; remove it explicitly before installing the ctx Sift hook".to_owned()),
            Ok(State::Conflict) => ("conflict", "ctx Sift hook differs; inspect manually".to_owned()),
            Ok(State::Unsupported) => ("unsupported", agent.limitation().to_owned()),
            Err(error) => { failed = true; ("error", format!("{error:#}")) },
        };
        rows.push(json!({"agent":agent.name(), "status":status, "detail":detail}));
    }
    if args.json_output() {
        let mut body = serde_json::to_string_pretty(
            &json!({"scope":if args.project {"project"} else {"global"}, "results":rows}),
        )?;
        body.push('\n');
        ui.write_stdout_bytes(body.as_bytes())?;
    } else {
        if rows.is_empty() {
            ui.write_stdout_bytes(b"No detected hosts support an automatic Sift hook. Use --agent HOST to select one explicitly.\n")?;
        }
        for row in rows {
            let agent = row["agent"].as_str().unwrap_or("host");
            let status = row["status"].as_str().unwrap_or("unknown");
            let detail = row["detail"].as_str().unwrap_or("");
            ui.write_stdout_bytes(
                format!(
                    "{agent}: {status}{}\n",
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(" ({detail})")
                    }
                )
                .as_bytes(),
            )?;
        }
    }
    if failed {
        bail!("one or more Sift-hook operations failed")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_agent_integrations::skill::PathContext;

    #[test]
    fn default_uses_existing_skill_detection_without_universal_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".agents")).unwrap();
        let context = Context {
            paths: PathContext::for_tests(home, tmp.path().join("project"))
                .with_env_override("CODEX_HOME", tmp.path().join("absent-codex")),
            executable: tmp.path().join("ctx"),
        };
        let args = OutputHookArgs {
            agent: vec![],
            all_agents: false,
            project: false,
            format: JsonOutputFormat::Text,
        };
        let (selected, skipped) = args.select(&context).unwrap();
        assert_eq!(selected, vec![Agent::Claude]);
        assert!(skipped.contains(&SkillAgentArg::Universal));
        assert!(!context.paths.cwd.join(".claude").exists());
    }

    #[test]
    fn explicit_and_all_selection_report_unsupported_skill_hosts() {
        let tmp = tempfile::tempdir().unwrap();
        let context = Context {
            paths: PathContext::for_tests(tmp.path().join("home"), tmp.path().join("project")),
            executable: tmp.path().join("ctx"),
        };
        let mut args = OutputHookArgs {
            agent: vec![SkillAgentArg::Cursor],
            all_agents: false,
            project: false,
            format: JsonOutputFormat::Text,
        };
        assert!(args.select(&context).is_err());
        args.agent.clear();
        args.all_agents = true;
        let (selected, skipped) = args.select(&context).unwrap();
        assert_eq!(
            selected.len(),
            Agent::ALL.iter().filter(|agent| agent.supported()).count()
        );
        for agent in Agent::ALL.into_iter().filter(|agent| agent.supported()) {
            assert!(selected.contains(&agent));
        }
        if !Agent::Codex.supported() {
            assert!(skipped.contains(&SkillAgentArg::Codex));
        }
        assert!(skipped.contains(&SkillAgentArg::Cursor));
    }

    #[test]
    fn windows_host_set_skips_codex_before_mutation_but_rejects_explicit_codex() {
        let tmp = tempfile::tempdir().unwrap();
        let context = Context {
            paths: PathContext::for_tests(tmp.path().join("home"), tmp.path().join("project")),
            executable: tmp.path().join("ctx"),
        };
        let mut args = OutputHookArgs {
            agent: vec![SkillAgentArg::ClaudeCode, SkillAgentArg::Codex],
            all_agents: false,
            project: true,
            format: JsonOutputFormat::Text,
        };
        assert!(
            args.select_supported(&context, |agent| agent != Agent::Codex)
                .is_err()
        );
        assert!(!context.paths.cwd.join(".claude/settings.json").exists());
        args.agent.clear();
        args.all_agents = true;
        let (selected, skipped) = args
            .select_supported(&context, |agent| agent != Agent::Codex)
            .unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&Agent::Claude));
        assert!(selected.contains(&Agent::Copilot));
        assert!(skipped.contains(&SkillAgentArg::Codex));
        for agent in selected {
            assert_eq!(
                output_hook::install(agent, true, &context).unwrap(),
                State::Current
            );
        }
        assert!(!context.paths.cwd.join(".codex/hooks.json").exists());
    }

    #[test]
    fn copilot_home_override_uses_shared_detected_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom-copilot");
        let context = Context {
            paths: PathContext::for_tests(tmp.path().join("home"), tmp.path().join("project"))
                .with_env_override("COPILOT_HOME", custom.clone())
                .with_env_override("CODEX_HOME", tmp.path().join("absent-codex")),
            executable: tmp.path().join("ctx"),
        };
        let args = OutputHookArgs {
            agent: vec![],
            all_agents: false,
            project: false,
            format: JsonOutputFormat::Text,
        };
        let (selected, _) = args.select(&context).unwrap();
        assert_eq!(selected, vec![Agent::Copilot]);
        assert_eq!(
            output_hook::install(Agent::Copilot, false, &context).unwrap(),
            State::Current
        );
        assert!(custom.join("hooks/ctx.json").exists());
        assert!(!context.paths.home.join(".copilot/hooks/ctx.json").exists());
    }
}
