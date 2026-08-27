use std::io::{self, IsTerminal, Write};

use anyhow::{anyhow, Context, Result};
use ctx_agent_application::skill::{
    complete_picker_selection, plan_install_selection, status_selection, SkillInstallSelectionPlan,
    SkillPickerPrompt, SkillSelectionRequest,
};
use ctx_agent_integrations::skill::{parse_picker_selection, SkillAgentSelection};

use super::{
    agents::{picker_agents, SkillAgentArg},
    paths::PathContext,
    SkillInstallArgs, SkillStatusArgs,
};
use crate::ui::Ui;

pub(super) fn install_agent_selection(
    args: &SkillInstallArgs,
    context: &PathContext,
    ui: &mut Ui,
) -> Result<SkillAgentSelection> {
    match plan_install_selection(
        SkillSelectionRequest {
            agents: &args.agent,
            all_agents: args.all_agents,
            allow_picker: !args.format.is_json() && can_prompt(),
            project: args.project,
        },
        context,
    )? {
        SkillInstallSelectionPlan::Selected(selection) => Ok(selection),
        SkillInstallSelectionPlan::Prompt(prompt) => {
            let mut input = io::stdin().lock();
            Ok(complete_picker_selection(prompt_for_agents(
                &prompt, &mut input, ui,
            )?))
        }
    }
}

pub(super) fn status_agent_selection(
    args: &SkillStatusArgs,
    context: &PathContext,
) -> Result<SkillAgentSelection> {
    status_selection(&args.agent, args.all_agents, args.project, context)
}

fn can_prompt() -> bool {
    can_prompt_for(io::stdin().is_terminal(), io::stderr().is_terminal())
}

fn can_prompt_for(stdin_is_terminal: bool, stderr_is_terminal: bool) -> bool {
    stdin_is_terminal && stderr_is_terminal
}

fn prompt_for_agents(
    prompt: &SkillPickerPrompt,
    input: &mut impl io::BufRead,
    ui: &mut Ui,
) -> Result<Vec<SkillAgentArg>> {
    prompt_for_agents_with_io(prompt, input, ui.stderr_writer())
}

fn prompt_for_agents_with_io(
    prompt: &SkillPickerPrompt,
    input: &mut impl io::BufRead,
    stderr: &mut (impl Write + ?Sized),
) -> Result<Vec<SkillAgentArg>> {
    let options = picker_agents();
    let defaults = prompt
        .options
        .iter()
        .filter(|option| option.selected_by_default)
        .map(|option| option.agent)
        .collect::<Vec<_>>();
    for line in picker_prompt_lines(prompt) {
        writeln!(stderr, "{line}")?;
    }
    loop {
        write!(stderr, "Install target(s): ")?;
        stderr.flush()?;
        let mut line = String::new();
        input
            .read_line(&mut line)
            .context("read skill install selection")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(defaults);
        }
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "q" | "quit" | "cancel"
        ) {
            return Err(anyhow!("skill install canceled"));
        }
        match parse_picker_selection(trimmed, options) {
            Ok(agents) => return Ok(agents),
            Err(err) => {
                writeln!(stderr, "{err}")?;
            }
        }
    }
}

fn picker_prompt_lines(prompt: &SkillPickerPrompt) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Select where to install {}. Detected agents are preselected.",
            prompt.skill_name
        ),
        "Press Enter for the marked defaults, or enter numbers like 1,2.".to_owned(),
    ];
    for (index, option) in prompt.options.iter().enumerate() {
        let marker = if option.selected_by_default { "*" } else { " " };
        let detected_hint = if option.detected { " detected" } else { "" };
        lines.push(format!(
            "  {}. [{}] {} -> {}{}",
            index + 1,
            marker,
            option.agent.display_name(),
            option.target.skill_dir.display(),
            detected_hint
        ));
    }
    lines
}

#[cfg(test)]
mod prompt_tests {
    use super::*;
    use std::{
        io,
        path::Path,
        sync::{Arc, Mutex},
    };

    use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }
    }

    impl io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_ui() -> (Ui, SharedWriter, SharedWriter) {
        let stdout = SharedWriter::default();
        let stderr = SharedWriter::default();
        let stdout_copy = stdout.clone();
        let stderr_copy = stderr.clone();
        let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
        let stderr_context =
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Never));
        (
            Ui::with_writers(stdout, stdout_context, stderr, stderr_context),
            stdout_copy,
            stderr_copy,
        )
    }

    fn picker_prompt_for_test() -> SkillPickerPrompt {
        let temp = tempfile::tempdir().unwrap();
        picker_prompt_for_root(temp.path())
    }

    fn picker_prompt_for_root(root: &Path) -> SkillPickerPrompt {
        let context = PathContext::for_tests(root.join("home"), root.join("repo"))
            .with_env_override("CODEX_HOME", root.join("missing-codex"));
        let SkillInstallSelectionPlan::Prompt(prompt) = plan_install_selection(
            SkillSelectionRequest {
                agents: &[],
                all_agents: false,
                allow_picker: true,
                project: false,
            },
            &context,
        )
        .unwrap() else {
            panic!("interactive selection should request a prompt");
        };
        prompt
    }

    #[test]
    fn interactive_picker_prompt_is_explicit_and_actionable() {
        let prompt = picker_prompt_for_test();
        let lines = picker_prompt_lines(&prompt);
        let rendered = lines.join("\n");

        assert!(rendered.contains("Select where to install"));
        assert!(rendered.contains("Press Enter for the marked defaults"));
        assert!(rendered.contains("[*] Universal"));
        assert!(rendered.contains(".agents/skills/ctx"));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn prompt_for_agents_with_io_retries_on_stderr_and_returns_the_selected_agents() {
        let prompt = picker_prompt_for_test();
        let mut input = io::Cursor::new(b"99\n1\n".to_vec());
        let mut stderr = Vec::new();

        let selected = prompt_for_agents_with_io(&prompt, &mut input, &mut stderr).unwrap();

        assert_eq!(selected, vec![SkillAgentArg::Universal]);
        let rendered = String::from_utf8(stderr).unwrap();
        assert!(rendered.contains("Install target(s): "));
        assert!(rendered.contains("invalid selection 99: choose 1-"));
    }

    #[test]
    fn can_prompt_rejects_asymmetric_tty_streams() {
        assert!(can_prompt_for(true, true));
        assert!(!can_prompt_for(true, false));
        assert!(!can_prompt_for(false, true));
        assert!(!can_prompt_for(false, false));
    }

    #[test]
    fn prompt_for_agents_writes_exact_selected_stderr_protocol() {
        let temp = tempfile::tempdir().unwrap();
        let prompt = picker_prompt_for_root(temp.path());
        let mut input = io::Cursor::new(b"\n".to_vec());
        let (mut ui, stdout, stderr) = test_ui();
        assert_eq!(
            prompt_for_agents(&prompt, &mut input, &mut ui).unwrap(),
            vec![SkillAgentArg::Universal]
        );
        assert_eq!(
            stderr.bytes(),
            format!(
                "{}\nInstall target(s): ",
                picker_prompt_lines(&prompt).join("\n")
            )
            .into_bytes()
        );
        assert!(stdout.bytes().is_empty());
    }
}
