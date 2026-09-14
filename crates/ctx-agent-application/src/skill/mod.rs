//! Bundled Agent Skill lifecycle workflows.

mod install;
mod remove;
mod selection;

pub use install::{
    force_install_command, install, status, SkillInstallOutcome, SkillStatusOutcome,
};
pub use remove::{force_remove_command, remove, SkillRemoveOutcome};
pub use selection::{
    complete_picker_selection, plan_install_selection, status_selection, SkillInstallSelectionPlan,
    SkillPickerOption, SkillPickerPrompt, SkillSelectionRequest,
};

#[cfg(test)]
mod tests;
