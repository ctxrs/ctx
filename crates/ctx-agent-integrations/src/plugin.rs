//! Native plugin-manager adapters for the ctx Agent Plugin.
//!
//! This module treats Codex and Claude Code as the sole authorities for plugin
//! state. It never reads or writes their config, caches, skills, or settings.

use std::{
    env,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::Value;

#[path = "plugin/model.rs"]
mod model;
#[path = "plugin/parse.rs"]
mod parse;
#[path = "plugin/process.rs"]
mod process;

pub use model::*;

use parse::{parse_marketplaces, parse_plugins, PluginInventory};
use process::{manager_environment, run_bounded, CommandOutput};

pub fn execute_install(request: PluginRequest, context: &PluginContext) -> PluginReceipt {
    execute(PluginOperation::Install, request, context)
}

pub fn execute_status(request: PluginRequest, context: &PluginContext) -> PluginReceipt {
    execute(PluginOperation::Status, request, context)
}

pub fn execute_remove(request: PluginRequest, context: &PluginContext) -> PluginReceipt {
    execute(PluginOperation::Remove, request, context)
}

fn execute(
    operation: PluginOperation,
    request: PluginRequest,
    context: &PluginContext,
) -> PluginReceipt {
    let scope = if request.project {
        PluginScope::Project
    } else {
        PluginScope::Global
    };
    let (selection, agents) = selected_agents(operation, &request, context);
    let results = agents
        .into_iter()
        .map(|agent| execute_for_agent(operation, agent, scope, context))
        .collect::<Vec<_>>();
    PluginReceipt {
        operation,
        scope,
        selection,
        failed: results.iter().filter(|result| !result.success).count(),
        operational_failures: results
            .iter()
            .filter(|result| result.is_operational_failure())
            .count(),
        modified: results.iter().filter(|result| result.modified).count(),
        results,
    }
}

fn selected_agents(
    operation: PluginOperation,
    request: &PluginRequest,
    context: &PluginContext,
) -> (PluginSelection, Vec<PluginAgent>) {
    if request.all_agents {
        return (PluginSelection::All, PluginAgent::ALL.to_vec());
    }
    if !request.agents.is_empty() {
        return (
            PluginSelection::Explicit,
            dedupe_agents(request.agents.iter().copied()),
        );
    }
    let mut agents = [PluginAgent::Codex, PluginAgent::ClaudeCode]
        .into_iter()
        .filter(|agent| context.detected(*agent))
        .collect::<Vec<_>>();
    if operation == PluginOperation::Status && context.detected(PluginAgent::Cursor) {
        agents.push(PluginAgent::Cursor);
    }
    (PluginSelection::Detected, agents)
}

fn dedupe_agents(agents: impl IntoIterator<Item = PluginAgent>) -> Vec<PluginAgent> {
    let mut deduped = Vec::new();
    for agent in agents {
        if !deduped.contains(&agent) {
            deduped.push(agent);
        }
    }
    deduped
}

fn execute_for_agent(
    operation: PluginOperation,
    agent: PluginAgent,
    scope: PluginScope,
    context: &PluginContext,
) -> PluginResult {
    if agent == PluginAgent::Cursor {
        return cursor_result(operation, scope, context.detected(agent));
    }
    if agent == PluginAgent::Codex && scope == PluginScope::Project {
        return unsupported_codex_project(operation, context.detected(agent));
    }
    let Some(program) = context.command(agent) else {
        return missing_cli_result(agent, scope);
    };
    let manager = Manager {
        agent,
        scope,
        program,
        cwd: context.cwd(),
        command_timeout: context.command_timeout(),
        output_limit_bytes: context.output_limit_bytes(),
    };
    match operation {
        PluginOperation::Install => automatic_install(manager),
        PluginOperation::Status => automatic_status(manager),
        PluginOperation::Remove => automatic_remove(manager),
    }
}

fn automatic_install(manager: Manager<'_>) -> PluginResult {
    let mut result = automatic_result(manager.agent, manager.scope);
    let marketplace = match manager.marketplaces() {
        Ok(status) => status,
        Err(diagnostic) => return diagnostic_failure(result, diagnostic),
    };
    if marketplace == PluginMarketplaceStatus::Conflict {
        return marketplace_conflict(result);
    }
    result.marketplace_status = marketplace;

    let mut inventory = match manager.plugins() {
        Ok(inventory) => inventory,
        Err(diagnostic) => return diagnostic_failure(result, diagnostic),
    };
    result.previous_status = inventory.status();
    observe_inventory(&mut result, &inventory);

    let mut marketplace_added = false;
    if marketplace == PluginMarketplaceStatus::Missing {
        let mutation_diagnostic = manager.add_marketplace().err();
        let observed = match manager.marketplaces() {
            Ok(observed) => observed,
            Err(reconciliation_diagnostic) => {
                return reconciliation_failure(
                    result,
                    mutation_diagnostic,
                    reconciliation_diagnostic,
                    PluginCommandStage::MarketplaceAdd,
                );
            }
        };
        result.modified |= observed != marketplace;
        marketplace_added = observed == PluginMarketplaceStatus::Present;
        result.marketplace_status = if marketplace_added {
            PluginMarketplaceStatus::Added
        } else {
            observed
        };
        if let Some(diagnostic) = mutation_diagnostic {
            return observed_mutation_failure(result, diagnostic);
        }
        match observed {
            PluginMarketplaceStatus::Present => {}
            PluginMarketplaceStatus::Conflict => {
                return observed_failure(
                    result,
                    "Marketplace ctx resolved to a different source after the add command."
                        .to_owned(),
                );
            }
            _ => {
                return observed_failure(
                    result,
                    format!(
                        "{} did not report the ctx marketplace after adding it.",
                        manager.agent.display_name()
                    ),
                );
            }
        }
    }

    let installed = if inventory.current.is_none() {
        let before = inventory.clone();
        let mutation_diagnostic = manager.install_plugin().err();
        inventory = match manager.plugins() {
            Ok(inventory) => inventory,
            Err(reconciliation_diagnostic) => {
                return reconciliation_failure(
                    result,
                    mutation_diagnostic,
                    reconciliation_diagnostic,
                    PluginCommandStage::PluginInstall,
                );
            }
        };
        result.modified |= inventory != before;
        observe_inventory(&mut result, &inventory);
        if let Some(diagnostic) = mutation_diagnostic {
            return observed_mutation_failure(result, diagnostic);
        }
        if inventory.current.is_none() {
            return observed_failure(
                result,
                format!(
                    "{} did not report {PLUGIN_ID} after the install command.",
                    manager.agent.display_name()
                ),
            );
        }
        true
    } else {
        false
    };

    let mut legacy_removed = false;
    if inventory.legacy.is_some() {
        let before = inventory.clone();
        let mutation_diagnostic = manager.remove_plugin(LEGACY_PLUGIN_ID).err();
        inventory = match manager.plugins() {
            Ok(inventory) => inventory,
            Err(reconciliation_diagnostic) => {
                return reconciliation_failure(
                    result,
                    mutation_diagnostic,
                    reconciliation_diagnostic,
                    PluginCommandStage::PluginRemove,
                );
            }
        };
        result.modified |= inventory != before;
        observe_inventory(&mut result, &inventory);
        if let Some(diagnostic) = mutation_diagnostic {
            return observed_mutation_failure(result, diagnostic);
        }
        if inventory.current.is_none() || inventory.legacy.is_some() {
            return observed_failure(
                result,
                format!(
                    "{} did not preserve {PLUGIN_ID} while removing the exact legacy plugin.",
                    manager.agent.display_name()
                ),
            );
        }
        legacy_removed = true;
    }

    observe_inventory(&mut result, &inventory);
    if !result.status.is_current() {
        return observed_failure(
            result,
            format!(
                "{} did not report {PLUGIN_ID} as installed.",
                manager.agent.display_name()
            ),
        );
    }
    result.success = true;
    result.action = if installed {
        PluginResultAction::Installed
    } else if legacy_removed {
        PluginResultAction::LegacyRemoved
    } else if marketplace_added {
        PluginResultAction::MarketplaceAdded
    } else {
        PluginResultAction::AlreadyInstalled
    };
    result
}

fn automatic_status(manager: Manager<'_>) -> PluginResult {
    let mut result = automatic_result(manager.agent, manager.scope);
    result.marketplace_status = match manager.marketplaces() {
        Ok(PluginMarketplaceStatus::Conflict) => return marketplace_conflict(result),
        Ok(status) => status,
        Err(diagnostic) => return diagnostic_failure(result, diagnostic),
    };
    let inventory = match manager.plugins() {
        Ok(inventory) => inventory,
        Err(diagnostic) => return diagnostic_failure(result, diagnostic),
    };
    result.previous_status = inventory.status();
    result.status = result.previous_status;
    result.installed_version = current_version(&inventory);
    result.action = PluginResultAction::Inspected;
    result.success = true;
    result
}

fn automatic_remove(manager: Manager<'_>) -> PluginResult {
    let mut result = automatic_result(manager.agent, manager.scope);
    result.marketplace_status = match manager.marketplaces() {
        Ok(PluginMarketplaceStatus::Conflict) => return marketplace_conflict(result),
        Ok(status) => status,
        Err(diagnostic) => return diagnostic_failure(result, diagnostic),
    };
    let mut inventory = match manager.plugins() {
        Ok(inventory) => inventory,
        Err(diagnostic) => return diagnostic_failure(result, diagnostic),
    };
    result.previous_status = inventory.status();
    observe_inventory(&mut result, &inventory);

    let mut removed = false;
    for plugin_id in [PLUGIN_ID, LEGACY_PLUGIN_ID] {
        let present = if plugin_id == PLUGIN_ID {
            inventory.current.is_some()
        } else {
            inventory.legacy.is_some()
        };
        if !present {
            continue;
        }
        let before = inventory.clone();
        let mutation_diagnostic = manager.remove_plugin(plugin_id).err();
        inventory = match manager.plugins() {
            Ok(inventory) => inventory,
            Err(reconciliation_diagnostic) => {
                return reconciliation_failure(
                    result,
                    mutation_diagnostic,
                    reconciliation_diagnostic,
                    PluginCommandStage::PluginRemove,
                );
            }
        };
        result.modified |= inventory != before;
        observe_inventory(&mut result, &inventory);
        if let Some(diagnostic) = mutation_diagnostic {
            return observed_mutation_failure(result, diagnostic);
        }
        let still_present = if plugin_id == PLUGIN_ID {
            inventory.current.is_some()
        } else {
            inventory.legacy.is_some()
        };
        if still_present {
            return observed_failure(
                result,
                format!(
                    "{} still reports {plugin_id} after removal.",
                    manager.agent.display_name()
                ),
            );
        }
        removed = true;
    }

    observe_inventory(&mut result, &inventory);
    if result.status != PluginInstallStatus::Missing {
        return observed_failure(
            result,
            format!(
                "{} still reports a ctx plugin after removal.",
                manager.agent.display_name()
            ),
        );
    }
    result.success = true;
    result.action = if removed {
        PluginResultAction::Removed
    } else {
        PluginResultAction::AlreadyAbsent
    };
    result
}

fn current_version(inventory: &PluginInventory) -> Option<String> {
    inventory
        .current
        .as_ref()
        .and_then(|plugin| plugin.version.clone())
}

fn observe_inventory(result: &mut PluginResult, inventory: &PluginInventory) {
    result.status = inventory.status();
    result.installed_version = current_version(inventory);
}

fn automatic_result(agent: PluginAgent, scope: PluginScope) -> PluginResult {
    PluginResult {
        agent,
        scope,
        capability: PluginCapability::Automatic,
        detected: true,
        supported: true,
        marketplace_status: PluginMarketplaceStatus::Error,
        previous_status: PluginInstallStatus::Error,
        status: PluginInstallStatus::Error,
        action: PluginResultAction::Failed,
        installed_version: None,
        success: false,
        modified: false,
        instructions: None,
        error: None,
        diagnostic: None,
        reconciliation_diagnostic: None,
    }
}

fn missing_cli_result(agent: PluginAgent, scope: PluginScope) -> PluginResult {
    PluginResult {
        agent,
        scope,
        capability: PluginCapability::Automatic,
        detected: false,
        supported: true,
        marketplace_status: PluginMarketplaceStatus::Error,
        previous_status: PluginInstallStatus::CliMissing,
        status: PluginInstallStatus::CliMissing,
        action: PluginResultAction::Failed,
        installed_version: None,
        success: false,
        modified: false,
        instructions: None,
        error: Some(format!(
            "{} CLI was not found on PATH.",
            agent.display_name()
        )),
        diagnostic: None,
        reconciliation_diagnostic: None,
    }
}

fn unsupported_codex_project(_operation: PluginOperation, detected: bool) -> PluginResult {
    PluginResult {
        agent: PluginAgent::Codex,
        scope: PluginScope::Project,
        capability: PluginCapability::UnsupportedScope,
        detected,
        supported: false,
        marketplace_status: PluginMarketplaceStatus::NotApplicable,
        previous_status: PluginInstallStatus::UnsupportedScope,
        status: PluginInstallStatus::UnsupportedScope,
        action: PluginResultAction::UnsupportedScope,
        installed_version: None,
        success: false,
        modified: false,
        instructions: None,
        error: Some("Codex plugins support global scope only; omit --project.".to_owned()),
        diagnostic: None,
        reconciliation_diagnostic: None,
    }
}

fn cursor_result(operation: PluginOperation, scope: PluginScope, detected: bool) -> PluginResult {
    let project_prefix = if scope == PluginScope::Project {
        "Open this project in Cursor. "
    } else {
        "Open Cursor. "
    };
    let instruction = match operation {
        PluginOperation::Install => format!(
            "{project_prefix}Choose Customize and open Marketplace. Find plugin ctx, then verify publisher ctx engineering inc and repository https://github.com/ctxrs/ctx before changing anything. Install ctx and verify that exact identity is installed before removing only the legacy ctx-agent-history-search plugin, if present."
        ),
        PluginOperation::Status => format!(
            "{project_prefix}Choose Customize and open Marketplace. Find plugin ctx, verify publisher ctx engineering inc and repository https://github.com/ctxrs/ctx, then review its status manually."
        ),
        PluginOperation::Remove => format!(
            "{project_prefix}Choose Customize and open Marketplace. Before removing plugin ctx, verify publisher ctx engineering inc and repository https://github.com/ctxrs/ctx. Remove only that verified plugin and the exact legacy ctx-agent-history-search plugin, if present."
        ),
    };
    PluginResult {
        agent: PluginAgent::Cursor,
        scope,
        capability: PluginCapability::ManualRequired,
        detected,
        supported: false,
        marketplace_status: PluginMarketplaceStatus::NotApplicable,
        previous_status: PluginInstallStatus::ManualRequired,
        status: PluginInstallStatus::ManualRequired,
        action: PluginResultAction::ManualRequired,
        installed_version: None,
        success: false,
        modified: false,
        instructions: Some(instruction),
        error: None,
        diagnostic: None,
        reconciliation_diagnostic: None,
    }
}

fn diagnostic_failure(
    mut result: PluginResult,
    diagnostic: PluginCommandDiagnostic,
) -> PluginResult {
    result.marketplace_status = if diagnostic.stage == PluginCommandStage::MarketplaceList {
        PluginMarketplaceStatus::Error
    } else {
        result.marketplace_status
    };
    result.status = PluginInstallStatus::Error;
    result.action = PluginResultAction::Failed;
    result.error = Some(diagnostic.concise_error(result.agent));
    result.diagnostic = Some(diagnostic);
    result
}

fn observed_mutation_failure(
    mut result: PluginResult,
    diagnostic: PluginCommandDiagnostic,
) -> PluginResult {
    result.action = PluginResultAction::Failed;
    result.error = Some(diagnostic.concise_error(result.agent));
    result.diagnostic = Some(diagnostic);
    result
}

fn reconciliation_failure(
    mut result: PluginResult,
    mutation_diagnostic: Option<PluginCommandDiagnostic>,
    reconciliation_diagnostic: PluginCommandDiagnostic,
    mutation_stage: PluginCommandStage,
) -> PluginResult {
    result.action = PluginResultAction::Failed;
    let reconciliation_fact = reconciliation_diagnostic.concise_error(result.agent);
    if let Some(mutation_diagnostic) = mutation_diagnostic {
        let mutation_fact = mutation_diagnostic.concise_error(result.agent);
        result.error = Some(format!(
            "{mutation_fact} State reconciliation also failed: {reconciliation_fact}"
        ));
        result.diagnostic = Some(mutation_diagnostic);
        result.reconciliation_diagnostic = Some(reconciliation_diagnostic);
    } else {
        result.error = Some(format!(
            "{} {} command completed, but state reconciliation failed: {reconciliation_fact}",
            result.agent.display_name(),
            mutation_stage.as_str()
        ));
        result.diagnostic = Some(reconciliation_diagnostic);
    }
    result
}

fn observed_failure(mut result: PluginResult, message: String) -> PluginResult {
    result.action = PluginResultAction::Failed;
    result.error = Some(message);
    result
}

fn marketplace_conflict(mut result: PluginResult) -> PluginResult {
    result.marketplace_status = PluginMarketplaceStatus::Conflict;
    result.status = PluginInstallStatus::Error;
    result.action = PluginResultAction::Failed;
    result.error = Some(
        "Marketplace ctx is configured from a different source; no plugin changes were made."
            .to_owned(),
    );
    result
}

struct Manager<'a> {
    agent: PluginAgent,
    scope: PluginScope,
    program: &'a Path,
    cwd: &'a Path,
    command_timeout: std::time::Duration,
    output_limit_bytes: usize,
}

impl Manager<'_> {
    fn marketplaces(&self) -> Result<PluginMarketplaceStatus, PluginCommandDiagnostic> {
        let output = self.run_json(
            PluginCommandStage::MarketplaceList,
            &["plugin", "marketplace", "list", "--json"],
        )?;
        parse_marketplaces(self.agent, &output.value)
            .map_err(|()| output.unexpected_json(PluginCommandStage::MarketplaceList))
    }

    fn plugins(&self) -> Result<PluginInventory, PluginCommandDiagnostic> {
        let output = self.run_json(
            PluginCommandStage::PluginList,
            &["plugin", "list", "--json"],
        )?;
        parse_plugins(self.agent, self.scope, &output.value)
            .map_err(|()| output.unexpected_json(PluginCommandStage::PluginList))
    }

    fn add_marketplace(&self) -> Result<(), PluginCommandDiagnostic> {
        match self.agent {
            PluginAgent::Codex => self.run_mutation(
                PluginCommandStage::MarketplaceAdd,
                &["plugin", "marketplace", "add", MARKETPLACE_SOURCE, "--json"],
            ),
            PluginAgent::ClaudeCode => self.run_mutation(
                PluginCommandStage::MarketplaceAdd,
                &[
                    "plugin",
                    "marketplace",
                    "add",
                    MARKETPLACE_SOURCE,
                    "--scope",
                    self.scope.claude_scope(),
                ],
            ),
            PluginAgent::Cursor => {
                Err(self.unsupported_manager(PluginCommandStage::MarketplaceAdd))
            }
        }
    }

    fn install_plugin(&self) -> Result<(), PluginCommandDiagnostic> {
        match self.agent {
            PluginAgent::Codex => self.run_mutation(
                PluginCommandStage::PluginInstall,
                &["plugin", "add", PLUGIN_ID, "--json"],
            ),
            PluginAgent::ClaudeCode => self.run_mutation(
                PluginCommandStage::PluginInstall,
                &[
                    "plugin",
                    "install",
                    PLUGIN_ID,
                    "--scope",
                    self.scope.claude_scope(),
                    "--yes",
                ],
            ),
            PluginAgent::Cursor => Err(self.unsupported_manager(PluginCommandStage::PluginInstall)),
        }
    }

    fn remove_plugin(&self, plugin_id: &'static str) -> Result<(), PluginCommandDiagnostic> {
        match self.agent {
            PluginAgent::Codex => self.run_mutation(
                PluginCommandStage::PluginRemove,
                &["plugin", "remove", plugin_id, "--json"],
            ),
            PluginAgent::ClaudeCode => self.run_mutation(
                PluginCommandStage::PluginRemove,
                &[
                    "plugin",
                    "uninstall",
                    plugin_id,
                    "--scope",
                    self.scope.claude_scope(),
                    "--yes",
                ],
            ),
            PluginAgent::Cursor => Err(self.unsupported_manager(PluginCommandStage::PluginRemove)),
        }
    }

    fn run_json(
        &self,
        stage: PluginCommandStage,
        args: &[&str],
    ) -> Result<JsonCommandOutput, PluginCommandDiagnostic> {
        let output = self.output(stage, args)?;
        match serde_json::from_slice(&output.stdout) {
            Ok(value) => Ok(JsonCommandOutput {
                value,
                stdout: output.stdout,
                stderr: output.stderr,
            }),
            Err(_) => Err(PluginCommandDiagnostic::new(
                stage,
                PluginCommandFailureKind::MalformedJson,
                output.exit_code,
                output.stdout,
                output.stderr,
            )),
        }
    }

    fn run_mutation(
        &self,
        stage: PluginCommandStage,
        args: &[&str],
    ) -> Result<(), PluginCommandDiagnostic> {
        self.output(stage, args).map(|_| ())
    }

    fn output(
        &self,
        stage: PluginCommandStage,
        args: &[&str],
    ) -> Result<CommandOutput, PluginCommandDiagnostic> {
        let mut command = Command::new(self.program);
        command
            .args(args)
            .current_dir(self.cwd)
            .stdin(Stdio::null());
        command.env_clear();
        command.envs(manager_environment(env::vars_os()));
        run_bounded(
            &mut command,
            stage,
            self.command_timeout,
            self.output_limit_bytes,
        )
    }

    fn unsupported_manager(&self, stage: PluginCommandStage) -> PluginCommandDiagnostic {
        PluginCommandDiagnostic::new(
            stage,
            PluginCommandFailureKind::Spawn,
            None,
            Vec::new(),
            Vec::new(),
        )
    }
}

struct JsonCommandOutput {
    value: Value,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl JsonCommandOutput {
    fn unexpected_json(self, stage: PluginCommandStage) -> PluginCommandDiagnostic {
        PluginCommandDiagnostic::new(
            stage,
            PluginCommandFailureKind::UnexpectedJson,
            Some(0),
            self.stdout,
            self.stderr,
        )
    }
}

#[cfg(test)]
#[path = "plugin/tests.rs"]
mod tests;
