#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Mutex, MutexGuard, OnceLock},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use super::*;

struct FakeManager {
    _temp: TempDir,
    executable: PathBuf,
    state: PathBuf,
    log: PathBuf,
}

impl FakeManager {
    fn codex() -> Self {
        Self::new("codex", CODEX_SCRIPT)
    }

    fn claude() -> Self {
        Self::new("claude", CLAUDE_SCRIPT)
    }

    fn new(name: &str, script: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join(name);
        fs::write(&executable, script).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let state = PathBuf::from(format!("{}.state", executable.display()));
        let log = PathBuf::from(format!("{}.log", executable.display()));
        fs::create_dir(&state).unwrap();
        Self {
            _temp: temp,
            executable,
            state,
            log,
        }
    }

    fn mark(&self, name: &str) {
        fs::write(self.state.join(name), b"").unwrap();
    }

    fn marked(&self, name: &str) -> bool {
        self.state.join(name).exists()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn context(&self, agent: PluginAgent) -> PluginContext {
        let cwd = self._temp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        match agent {
            PluginAgent::Codex => {
                PluginContext::for_tests(cwd, Some(self.executable.clone()), None, None)
            }
            PluginAgent::ClaudeCode => {
                PluginContext::for_tests(cwd, None, Some(self.executable.clone()), None)
            }
            PluginAgent::Cursor => unreachable!(),
        }
    }

    fn context_with_limits(
        &self,
        agent: PluginAgent,
        timeout: Duration,
        output_limit_bytes: usize,
    ) -> PluginContext {
        self.context(agent)
            .with_command_limits_for_tests(timeout, output_limit_bytes)
    }
}

fn request(agent: PluginAgent, project: bool) -> PluginRequest {
    PluginRequest {
        agents: vec![agent],
        all_agents: false,
        project,
    }
}

fn fake_process_guard() -> MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[test]
fn codex_status_is_strictly_read_only() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    fake.mark("current");

    let receipt = execute_status(
        request(PluginAgent::Codex, false),
        &fake.context(PluginAgent::Codex),
    );

    assert_eq!(receipt.modified, 0);
    assert_eq!(receipt.results[0].status, PluginInstallStatus::Installed);
    assert_eq!(
        fake.log_lines(),
        ["plugin marketplace list --json", "plugin list --json"]
    );
}

#[test]
fn codex_install_is_idempotent_and_verifies_every_mutation() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    let context = fake.context(PluginAgent::Codex);

    let first = execute_install(request(PluginAgent::Codex, false), &context);
    let second = execute_install(request(PluginAgent::Codex, false), &context);

    assert!(first.results[0].success);
    assert!(first.results[0].modified);
    assert_eq!(first.results[0].action, PluginResultAction::Installed);
    assert!(second.results[0].success);
    assert!(!second.results[0].modified);
    assert_eq!(
        second.results[0].action,
        PluginResultAction::AlreadyInstalled
    );
    assert_eq!(
        fake.log_lines(),
        [
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin marketplace add ctxrs/ctx --json",
            "plugin marketplace list --json",
            "plugin add ctx@ctx --json",
            "plugin list --json",
            "plugin marketplace list --json",
            "plugin list --json",
        ]
    );
}

#[test]
fn claude_project_install_trusts_the_manager_selected_version() {
    let _guard = fake_process_guard();
    let fake = FakeManager::claude();
    fake.mark("marketplace");
    fake.mark("current-project");
    fake.mark("alternate-current-version-project");

    let receipt = execute_install(
        request(PluginAgent::ClaudeCode, true),
        &fake.context(PluginAgent::ClaudeCode),
    );

    assert!(receipt.results[0].success);
    assert!(!receipt.results[0].modified);
    assert_eq!(
        receipt.results[0].action,
        PluginResultAction::AlreadyInstalled
    );
    assert_eq!(receipt.results[0].status, PluginInstallStatus::Installed);
    assert_eq!(
        receipt.results[0].installed_version.as_deref(),
        Some("0.9.0")
    );
    assert_eq!(
        fake.log_lines(),
        ["plugin marketplace list --json", "plugin list --json",]
    );
}

#[test]
fn claude_remove_is_idempotent_and_preserves_the_marketplace() {
    let _guard = fake_process_guard();
    let fake = FakeManager::claude();
    fake.mark("marketplace");
    fake.mark("current-user");
    fake.mark("legacy-user");
    let context = fake.context(PluginAgent::ClaudeCode);

    let first = execute_remove(request(PluginAgent::ClaudeCode, false), &context);
    let second = execute_remove(request(PluginAgent::ClaudeCode, false), &context);

    assert!(first.results[0].success);
    assert_eq!(first.results[0].action, PluginResultAction::Removed);
    assert!(second.results[0].success);
    assert_eq!(second.results[0].action, PluginResultAction::AlreadyAbsent);
    assert!(fake.marked("marketplace"));
    assert_eq!(
        fake.log_lines(),
        [
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin uninstall ctx@ctx --scope user --yes",
            "plugin list --json",
            "plugin uninstall ctx-agent-history-search@ctx --scope user --yes",
            "plugin list --json",
            "plugin marketplace list --json",
            "plugin list --json",
        ]
    );
}

#[test]
fn marketplace_name_conflict_fails_closed_before_plugin_list() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace-conflict");

    let receipt = execute_install(
        request(PluginAgent::Codex, false),
        &fake.context(PluginAgent::Codex),
    );

    assert!(!receipt.results[0].success);
    assert_eq!(
        receipt.results[0].marketplace_status,
        PluginMarketplaceStatus::Conflict
    );
    assert_eq!(fake.log_lines(), ["plugin marketplace list --json"]);
}

#[test]
fn malformed_and_nonzero_manager_output_are_sanitized_but_retained() {
    let _guard = fake_process_guard();
    let malformed = FakeManager::codex();
    malformed.mark("marketplace");
    malformed.mark("malformed-plugin-list");
    let malformed_receipt = execute_status(
        request(PluginAgent::Codex, false),
        &malformed.context(PluginAgent::Codex),
    );
    let malformed_result = &malformed_receipt.results[0];
    let malformed_diagnostic = malformed_result.diagnostic.as_ref().unwrap();
    assert_eq!(
        malformed_diagnostic.kind,
        PluginCommandFailureKind::MalformedJson,
        "diagnostic: {malformed_diagnostic:?}"
    );
    assert!(malformed_result
        .error
        .as_deref()
        .unwrap()
        .contains("malformed JSON"));

    let nonzero = FakeManager::claude();
    nonzero.mark("marketplace");
    nonzero.mark("nonzero-plugin-list");
    let nonzero_receipt = execute_status(
        request(PluginAgent::ClaudeCode, false),
        &nonzero.context(PluginAgent::ClaudeCode),
    );
    let nonzero_result = &nonzero_receipt.results[0];
    let diagnostic = nonzero_result.diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.kind, PluginCommandFailureKind::NonZero);
    assert!(
        String::from_utf8_lossy(diagnostic.captured_stderr_bytes()).contains("private host detail")
    );
    assert!(!nonzero_result
        .error
        .as_deref()
        .unwrap()
        .contains("private host detail"));
}

#[test]
fn missing_cli_codex_project_and_cursor_are_receipts_without_processes() {
    let _guard = fake_process_guard();
    let context = PluginContext::for_tests(PathBuf::from("/project"), None, None, None);

    let missing = execute_status(request(PluginAgent::ClaudeCode, false), &context);
    assert_eq!(missing.results[0].status, PluginInstallStatus::CliMissing);
    assert!(!missing.results[0].detected);

    let unsupported = execute_install(request(PluginAgent::Codex, true), &context);
    assert_eq!(
        unsupported.results[0].status,
        PluginInstallStatus::UnsupportedScope
    );

    let cursor = execute_install(request(PluginAgent::Cursor, true), &context);
    assert_eq!(
        cursor.results[0].status,
        PluginInstallStatus::ManualRequired
    );
    assert_eq!(cursor.failed, 1);
    assert_eq!(cursor.operational_failures, 0);
    assert!(!cursor.results[0].success);
    let instructions = cursor.results[0].instructions.as_deref().unwrap();
    assert!(instructions.contains("this project"));
    assert!(instructions.contains("Customize"));
    assert!(instructions.contains("Marketplace"));
    assert!(instructions.contains("plugin ctx"));
    assert!(instructions.contains("publisher ctx engineering inc"));
    assert!(instructions.contains("repository https://github.com/ctxrs/ctx"));
    assert!(instructions.find("verify").unwrap() < instructions.find("Install ctx").unwrap());
    assert!(instructions.find("Install ctx").unwrap() < instructions.find("legacy").unwrap());
}

#[test]
fn only_the_exact_proven_legacy_id_is_removed() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    let context = fake.context(PluginAgent::Codex);

    let absent = execute_remove(request(PluginAgent::Codex, false), &context);
    assert_eq!(absent.results[0].action, PluginResultAction::AlreadyAbsent);

    fake.mark("legacy");
    let migrated = execute_install(request(PluginAgent::Codex, false), &context);
    assert!(migrated.results[0].success);
    assert_eq!(migrated.results[0].action, PluginResultAction::Installed);
    assert!(!fake.marked("legacy"));
    assert!(fake.marked("current"));
    assert!(fake
        .log_lines()
        .contains(&"plugin remove ctx-agent-history-search@ctx --json".to_owned()));
    let log = fake.log_lines();
    let install = log
        .iter()
        .position(|line| line == "plugin add ctx@ctx --json")
        .unwrap();
    let remove = log
        .iter()
        .position(|line| line == "plugin remove ctx-agent-history-search@ctx --json")
        .unwrap();
    assert!(install < remove);
    assert!(
        !fake
            .log_lines()
            .iter()
            .any(|line| line.contains("ctx-agent-history-search-extra@ctx")
                && line.contains("remove"))
    );
}

#[test]
fn failed_current_install_preserves_the_exact_legacy_plugin() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    fake.mark("legacy");
    fake.mark("fail-install");

    let receipt = execute_install(
        request(PluginAgent::Codex, false),
        &fake.context(PluginAgent::Codex),
    );

    assert!(!receipt.results[0].success);
    assert!(fake.marked("legacy"));
    assert!(!fake.marked("current"));
    assert_eq!(
        fake.log_lines(),
        [
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin add ctx@ctx --json",
            "plugin list --json",
        ]
    );
}

#[test]
fn mutate_then_fail_install_is_reconciled_without_removing_legacy() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    fake.mark("legacy");
    fake.mark("mutate-then-fail-install");

    let receipt = execute_install(
        request(PluginAgent::Codex, false),
        &fake.context(PluginAgent::Codex),
    );
    let result = &receipt.results[0];

    assert!(!result.success);
    assert!(result.modified);
    assert_eq!(result.status, PluginInstallStatus::Installed);
    assert_eq!(result.installed_version.as_deref(), Some("1.2.3"));
    assert!(fake.marked("current"));
    assert!(fake.marked("legacy"));
    assert_eq!(
        result.diagnostic.as_ref().unwrap().kind,
        PluginCommandFailureKind::NonZero
    );
    assert!(
        String::from_utf8_lossy(result.diagnostic.as_ref().unwrap().captured_stderr_bytes())
            .contains("private mutated install detail")
    );
    assert!(!result
        .error
        .as_deref()
        .unwrap()
        .contains("private mutated install detail"));
    assert_eq!(
        fake.log_lines(),
        [
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin add ctx@ctx --json",
            "plugin list --json",
        ]
    );
}

#[test]
fn mutate_then_fail_marketplace_add_is_reconciled_before_stopping() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("mutate-then-fail-marketplace-add");

    let receipt = execute_install(
        request(PluginAgent::Codex, false),
        &fake.context(PluginAgent::Codex),
    );
    let result = &receipt.results[0];

    assert!(!result.success);
    assert!(result.modified);
    assert_eq!(result.marketplace_status, PluginMarketplaceStatus::Added);
    assert_eq!(result.status, PluginInstallStatus::Missing);
    assert!(fake.marked("marketplace"));
    assert!(!fake.marked("current"));
    assert_eq!(
        result.diagnostic.as_ref().unwrap().kind,
        PluginCommandFailureKind::NonZero
    );
    assert_eq!(
        fake.log_lines(),
        [
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin marketplace add ctxrs/ctx --json",
            "plugin marketplace list --json",
        ]
    );
}

#[test]
fn mutate_then_fail_remove_reports_observed_absence() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    fake.mark("current");
    fake.mark("mutate-then-fail-remove-current");

    let receipt = execute_remove(
        request(PluginAgent::Codex, false),
        &fake.context(PluginAgent::Codex),
    );
    let result = &receipt.results[0];

    assert!(!result.success);
    assert!(result.modified);
    assert_eq!(result.status, PluginInstallStatus::Missing);
    assert_eq!(result.installed_version, None);
    assert!(!fake.marked("current"));
    assert_eq!(
        result.diagnostic.as_ref().unwrap().kind,
        PluginCommandFailureKind::NonZero
    );
    assert_eq!(
        fake.log_lines(),
        [
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin remove ctx@ctx --json",
            "plugin list --json",
        ]
    );
}

#[test]
fn mutation_and_reconciliation_failures_are_both_sanitized_and_retained() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    fake.mark("mutate-then-fail-install-and-reconcile");

    let receipt = execute_install(
        request(PluginAgent::Codex, false),
        &fake.context(PluginAgent::Codex),
    );
    let result = &receipt.results[0];
    let mutation = result.diagnostic.as_ref().unwrap();
    let reconciliation = result.reconciliation_diagnostic.as_ref().unwrap();

    assert_eq!(mutation.kind, PluginCommandFailureKind::NonZero);
    assert_eq!(reconciliation.kind, PluginCommandFailureKind::MalformedJson);
    assert!(String::from_utf8_lossy(mutation.captured_stderr_bytes())
        .contains("private original failure"));
    assert!(
        String::from_utf8_lossy(reconciliation.captured_stdout_bytes())
            .contains("private reconciliation path")
    );
    let error = result.error.as_deref().unwrap();
    assert!(error.contains("failed with exit code 31"));
    assert!(error.contains("State reconciliation also failed"));
    assert!(error.contains("malformed JSON"));
    assert!(!error.contains("private original failure"));
    assert!(!error.contains("private reconciliation path"));
}

#[test]
fn timed_out_mutation_is_reconciled_and_manager_tree_is_terminated() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    fake.mark("legacy");
    fake.mark("hang-after-mutating-install");
    let context = fake.context_with_limits(PluginAgent::Codex, Duration::from_millis(150), 4096);
    let started = Instant::now();

    let receipt = execute_install(request(PluginAgent::Codex, false), &context);
    let result = &receipt.results[0];

    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(result.modified);
    assert_eq!(result.status, PluginInstallStatus::Installed);
    assert!(fake.marked("current"));
    assert!(fake.marked("legacy"));
    assert_eq!(
        result.diagnostic.as_ref().unwrap().kind,
        PluginCommandFailureKind::Timeout
    );
    let descendant = fs::read_to_string(fake.state.join("hanging-descendant-pid"))
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let gone = (0..50).any(|_| {
        // SAFETY: Signal zero only checks whether the fake descendant exists.
        let result = unsafe { libc::kill(descendant, 0) };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            true
        } else {
            thread::sleep(Duration::from_millis(10));
            false
        }
    });
    assert!(gone, "timed-out manager descendant {descendant} survived");
    assert_eq!(
        fake.log_lines(),
        [
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin add ctx@ctx --json",
            "plugin list --json",
        ]
    );
}

#[test]
fn oversized_manager_output_is_bounded_and_rejected() {
    let _guard = fake_process_guard();
    let fake = FakeManager::codex();
    fake.mark("marketplace");
    fake.mark("oversized-plugin-list");
    let context = fake.context_with_limits(PluginAgent::Codex, Duration::from_secs(2), 1024);

    let receipt = execute_status(request(PluginAgent::Codex, false), &context);
    let result = &receipt.results[0];
    let diagnostic = result.diagnostic.as_ref().unwrap();

    assert_eq!(diagnostic.kind, PluginCommandFailureKind::OutputLimit);
    assert_eq!(diagnostic.captured_stdout_bytes().len(), 1024);
    assert!(result.error.as_deref().unwrap().contains("output limit"));
}

#[test]
fn marketplace_parsers_require_the_official_github_discriminators() {
    use serde_json::json;

    assert_eq!(
        parse_marketplaces(
            PluginAgent::Codex,
            &json!({"marketplaces":[{
                "name":"ctx",
                "marketplaceSource":{"type":"github","value":"ctxrs/ctx"}
            }]})
        ),
        Ok(PluginMarketplaceStatus::Present)
    );
    assert_eq!(
        parse_marketplaces(
            PluginAgent::ClaudeCode,
            &json!([{"name":"ctx","source":"github","repo":"ctxrs/ctx"}])
        ),
        Ok(PluginMarketplaceStatus::Present)
    );

    for hostile in [
        json!({"marketplaces":[{"name":"ctx","marketplaceSource":{"type":"local","value":"ctxrs/ctx"}}]}),
        json!({"marketplaces":[{"name":"ctx","marketplaceSource":{"type":"path","path":"/tmp/ctx"}}]}),
        json!({"marketplaces":[{"name":"ctx","marketplaceSource":{"type":"unknown","value":"ctxrs/ctx"}}]}),
        json!({"marketplaces":[{"name":"ctx","marketplaceSource":{"type":"github","value":"https://github.com/ctxrs/ctx"}}]}),
    ] {
        assert_ne!(
            parse_marketplaces(PluginAgent::Codex, &hostile),
            Ok(PluginMarketplaceStatus::Present),
            "hostile Codex marketplace was accepted: {hostile}"
        );
    }
    for hostile in [
        json!([{"name":"ctx","source":"local","repo":"ctxrs/ctx"}]),
        json!([{"name":"ctx","source":"path","path":"/tmp/ctx"}]),
        json!([{"name":"ctx","source":"unknown","repo":"ctxrs/ctx"}]),
        json!([{"name":"ctx","source":"github","repo":"http://github.com/ctxrs/ctx"}]),
    ] {
        assert_ne!(
            parse_marketplaces(PluginAgent::ClaudeCode, &hostile),
            Ok(PluginMarketplaceStatus::Present),
            "hostile Claude marketplace was accepted: {hostile}"
        );
    }
}

#[test]
fn plugin_parsers_reject_undocumented_shapes_and_synthesized_ids() {
    use serde_json::json;

    for hostile in [
        json!({"plugins":[]}),
        json!({"installed":[{"id":"ctx@ctx","installed":true}]}),
        json!({"installed":[{"name":"ctx","marketplaceName":"ctx","installed":true}]}),
        json!({"installed":[{"pluginId":"ctx@ctx","installed":"yes"}]}),
        json!({"installed":[{"pluginId":"ctx@ctx","installed":true,"version":7}]}),
        json!({"installed":[{"pluginId":"ctx@ctx","installed":true,"name":"ctx"}]}),
        json!({"installed":[{"pluginId":"ctx@ctx","installed":true,"name":"wrong","marketplaceName":"ctx"}]}),
        json!({"installed":[{"pluginId":"ctx@ctx","installed":true,"name":"ctx","marketplaceName":"wrong"}]}),
        json!({"installed":[{"pluginId":"other@ctx","installed":true,"name":"ctx","marketplaceName":"ctx"}]}),
        json!({"installed":[{"pluginId":"ctx@ctx","installed":true,"name":7,"marketplaceName":"ctx"}]}),
        json!({"installed":[{"pluginId":"ctx@ctx","installed":true,"name":"ctx","marketplaceName":false}]}),
        json!({"installed":[
            {"pluginId":"ctx@ctx","installed":true,"name":"ctx","marketplaceName":"ctx"},
            {"pluginId":"ctx@ctx","installed":false,"name":"ctx","marketplaceName":"ctx"}
        ]}),
        json!({"installed":[
            {"pluginId":"ctx-agent-history-search@ctx","installed":true,"name":"ctx-agent-history-search","marketplaceName":"ctx"},
            {"pluginId":"ctx-agent-history-search@ctx","installed":true,"name":"ctx-agent-history-search","marketplaceName":"ctx"}
        ]}),
    ] {
        assert!(
            parse_plugins(PluginAgent::Codex, PluginScope::Global, &hostile).is_err(),
            "hostile Codex plugin list was accepted: {hostile}"
        );
    }
    for hostile in [
        json!({"installed":[]}),
        json!([{"pluginId":"ctx@ctx","scope":"user"}]),
        json!([{"name":"ctx","marketplace":"ctx","scope":"user"}]),
        json!([{"id":"ctx@ctx"}]),
        json!([{"id":"ctx@ctx","scope":"unknown"}]),
        json!([{"id":"ctx@ctx","scope":"user","version":7}]),
    ] {
        assert!(
            parse_plugins(PluginAgent::ClaudeCode, PluginScope::Global, &hostile).is_err(),
            "hostile Claude plugin list was accepted: {hostile}"
        );
    }
}

#[test]
fn plugin_parsers_preserve_optional_versions_without_overriding_manager_state() {
    use serde_json::json;

    let codex = parse_plugins(
        PluginAgent::Codex,
        PluginScope::Global,
        &json!({"installed":[{
            "pluginId":"ctx@ctx",
            "name":"ctx",
            "marketplaceName":"ctx",
            "installed":true,
            "version":"99.0.0"
        }]}),
    )
    .unwrap();
    assert_eq!(codex.status(), PluginInstallStatus::Installed);
    assert_eq!(codex.current.unwrap().version.as_deref(), Some("99.0.0"));

    let unrelated_omitted_fields = parse_plugins(
        PluginAgent::Codex,
        PluginScope::Global,
        &json!({"installed":[{"pluginId":"unrelated@other","installed":true}]}),
    )
    .unwrap();
    assert_eq!(
        unrelated_omitted_fields.status(),
        PluginInstallStatus::Missing
    );

    let wrong_scope = parse_plugins(
        PluginAgent::ClaudeCode,
        PluginScope::Project,
        &json!([{"id":"ctx@ctx","scope":"user","version":"99.0.0"}]),
    )
    .unwrap();
    assert_eq!(wrong_scope.status(), PluginInstallStatus::Missing);
}

const CODEX_SCRIPT: &str = r#"#!/bin/sh
set -eu
state="$0.state"
log="$0.log"
printf '%s\n' "$*" >> "$log"

if [ "$*" = "plugin marketplace list --json" ]; then
  if [ -f "$state/marketplace-conflict" ]; then
    printf '%s\n' '{"marketplaces":[{"name":"ctx","root":"/opaque","marketplaceSource":{"type":"github","value":"someone/else"}}]}'
  elif [ -f "$state/marketplace" ]; then
    printf '%s\n' '{"marketplaces":[{"name":"ctx","root":"/opaque","marketplaceSource":{"type":"github","value":"ctxrs/ctx"}}]}'
  else
    printf '%s\n' '{"marketplaces":[]}'
  fi
  exit 0
fi
if [ "$*" = "plugin marketplace add ctxrs/ctx --json" ]; then
  : > "$state/marketplace"
  if [ -f "$state/mutate-then-fail-marketplace-add" ]; then
    printf '%s\n' 'private mutated marketplace detail' >&2
    exit 21
  fi
  printf '%s\n' '{}'
  exit 0
fi
if [ "$*" = "plugin list --json" ]; then
  if [ -f "$state/malformed-next-plugin-list" ]; then
    rm -f "$state/malformed-next-plugin-list"
    printf '%s\n' '{"private reconciliation path":"/secret/reconcile"'
    exit 0
  fi
  if [ -f "$state/oversized-plugin-list" ]; then
    rm -f "$state/oversized-plugin-list"
    count=0
    while [ "$count" -lt 4096 ]; do
      printf '%s' '0123456789abcdef0123456789abcdef'
      count=$((count + 1))
    done
    exit 0
  fi
  if [ -f "$state/nonzero-plugin-list" ]; then
    printf '%s\n' 'private host detail: /secret/path' >&2
    exit 17
  fi
  if [ -f "$state/malformed-plugin-list" ]; then
    printf '%s\n' '{'
    exit 0
  fi
  printf '%s' '{"installed":[{"pluginId":"ctx-agent-history-search-extra@ctx","version":"9.9.9","installed":true}'
  if [ -f "$state/current" ]; then
    version="1.2.3"
    if [ -f "$state/alternate-current-version" ]; then version="0.9.0"; fi
    printf ',{"pluginId":"ctx@ctx","name":"ctx","marketplaceName":"ctx","version":"%s","installed":true}' "$version"
  fi
  if [ -f "$state/legacy" ]; then
    printf '%s' ',{"pluginId":"ctx-agent-history-search@ctx","name":"ctx-agent-history-search","marketplaceName":"ctx","version":"0.8.0","installed":true}'
  fi
  printf '%s\n' ']}'
  exit 0
fi
if [ "$*" = "plugin add ctx@ctx --json" ]; then
  if [ -f "$state/hang-after-mutating-install" ]; then
    : > "$state/current"
    sleep 30 &
    descendant=$!
    printf '%s\n' "$descendant" > "$state/hanging-descendant-pid"
    wait "$descendant"
  fi
  if [ -f "$state/mutate-then-fail-install-and-reconcile" ]; then
    : > "$state/current"
    : > "$state/malformed-next-plugin-list"
    printf '%s\n' 'private original failure: /secret/install' >&2
    exit 31
  fi
  if [ -f "$state/mutate-then-fail-install" ]; then
    : > "$state/current"
    printf '%s\n' 'private mutated install detail' >&2
    exit 23
  fi
  if [ -f "$state/fail-install" ]; then
    printf '%s\n' 'private install failure' >&2
    exit 23
  fi
  : > "$state/current"
  rm -f "$state/alternate-current-version"
  printf '%s\n' '{}'
  exit 0
fi
if [ "$*" = "plugin remove ctx@ctx --json" ]; then
  rm -f "$state/current" "$state/alternate-current-version"
  if [ -f "$state/mutate-then-fail-remove-current" ]; then
    printf '%s\n' 'private mutated remove detail' >&2
    exit 29
  fi
  printf '%s\n' '{}'
  exit 0
fi
if [ "$*" = "plugin remove ctx-agent-history-search@ctx --json" ]; then
  rm -f "$state/legacy"
  printf '%s\n' '{}'
  exit 0
fi
printf '%s\n' "unexpected argv: $*" >&2
exit 99
"#;

const CLAUDE_SCRIPT: &str = r#"#!/bin/sh
set -eu
state="$0.state"
log="$0.log"
printf '%s\n' "$*" >> "$log"

if [ "$*" = "plugin marketplace list --json" ]; then
  if [ -f "$state/marketplace-conflict" ]; then
    printf '%s\n' '[{"name":"ctx","source":"github","repo":"someone/else","installLocation":"/opaque"}]'
  elif [ -f "$state/marketplace" ]; then
    printf '%s\n' '[{"name":"ctx","source":"github","repo":"ctxrs/ctx","installLocation":"/opaque"}]'
  else
    printf '%s\n' '[]'
  fi
  exit 0
fi
case "$*" in
  "plugin marketplace add ctxrs/ctx --scope user"|"plugin marketplace add ctxrs/ctx --scope project")
    : > "$state/marketplace"
    exit 0
    ;;
esac
if [ "$*" = "plugin list --json" ]; then
  if [ -f "$state/nonzero-plugin-list" ]; then
    printf '%s\n' 'private host detail: /secret/path' >&2
    exit 19
  fi
  if [ -f "$state/malformed-plugin-list" ]; then
    printf '%s\n' '['
    exit 0
  fi
  printf '%s' '[{"id":"ctx-agent-history-search-extra@ctx","version":"9.9.9","scope":"user"}'
  for scope in user project; do
    if [ -f "$state/current-$scope" ]; then
      version="1.2.3"
      if [ -f "$state/alternate-current-version-$scope" ]; then version="0.9.0"; fi
      printf ',{"id":"ctx@ctx","version":"%s","scope":"%s"}' "$version" "$scope"
    fi
    if [ -f "$state/legacy-$scope" ]; then
      printf ',{"id":"ctx-agent-history-search@ctx","version":"0.8.0","scope":"%s"}' "$scope"
    fi
  done
  printf '%s\n' ']'
  exit 0
fi
case "$*" in
  "plugin install ctx@ctx --scope user --yes")
    : > "$state/current-user"
    rm -f "$state/alternate-current-version-user"
    exit 0
    ;;
  "plugin install ctx@ctx --scope project --yes")
    : > "$state/current-project"
    rm -f "$state/alternate-current-version-project"
    exit 0
    ;;
  "plugin uninstall ctx@ctx --scope user --yes")
    rm -f "$state/current-user" "$state/alternate-current-version-user"
    exit 0
    ;;
  "plugin uninstall ctx@ctx --scope project --yes")
    rm -f "$state/current-project" "$state/alternate-current-version-project"
    exit 0
    ;;
  "plugin uninstall ctx-agent-history-search@ctx --scope user --yes")
    rm -f "$state/legacy-user"
    exit 0
    ;;
  "plugin uninstall ctx-agent-history-search@ctx --scope project --yes")
    rm -f "$state/legacy-project"
    exit 0
    ;;
esac
printf '%s\n' "unexpected argv: $*" >&2
exit 99
"#;
