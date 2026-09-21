//! Interpretation of repository-neutral Core activity.

use std::collections::BTreeMap;

use ctx_history_capture_model::exact_json_value;
use ctx_history_core::{
    ActivityJsonCapture, ActivityTextCapture, CoreActivity, CoreRecord, LiteralFactKind,
};
use serde_json::Value;

mod file_invocation;

use file_invocation::{FileInvocationExtraction, exact_file_invocations};

use crate::{
    CommandLiteralDisposition, GitObjectFormat, GitObjectId, LinkedOutcomeInput,
    LiteralFileObservation, LiteralVcsObservation, NeutralRepositoryFacts, RepositoryEvaluation,
    RepositoryEvidenceResolver, RepositoryFileObservationKind, RepositoryVcsObservationKind,
    linked_outcome_evidence,
};

const MAX_PENDING_INVOCATIONS: usize = 256;
const MAX_CODEX_EXEC_COMMAND_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_CODEX_EXEC_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_CODEX_EXEC_COMMAND_WORKDIR_BYTES: usize = 16 * 1024;
const CODEX_CORE_ACTIVITY_REVISION: &str = "codex-nativepath-core-activity-v15-revert-lineage";
const CODEX_RELEASED_V14_REVISION: &str =
    "codex-nativepath-core-activity-v14-literal-patch-file-facts";
const CODEX_RELEASED_V11_REVISION: &str = "codex-nativepath-core-activity-v11-item-call-identity";

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexExecCommandExtraction {
    NotApplicable,
    Exact {
        command: String,
        workdir: Option<String>,
    },
    Rejected,
}

#[derive(Debug, Clone)]
struct PendingInvocation {
    call_id: String,
    event_sequence: u64,
    command: Option<String>,
    session_cwd: Option<String>,
    declared_workdir: Option<String>,
}

/// Stateful, source-scoped adapter from neutral Core activity into repository
/// conclusions.
///
/// Unresolved invocation/result state is deliberately bounded and reset at
/// every immutable Core source boundary. Duplicate native call IDs that
/// coexist in this unresolved window become ambiguous. A later record cannot
/// retroactively revoke a join already projected by this streaming adapter;
/// source-wide uniqueness, when required, must be established upstream.
#[derive(Debug, Default)]
pub struct CoreRepositoryEvidenceAdapter {
    resolver: RepositoryEvidenceResolver,
    pending: BTreeMap<String, Option<PendingInvocation>>,
    pending_results: BTreeMap<String, Option<PendingResult>>,
}

impl CoreRepositoryEvidenceAdapter {
    pub fn begin_source(&mut self) {
        self.pending.clear();
        self.pending_results.clear();
        self.resolver.begin_source();
    }

    #[must_use]
    pub fn evaluate_record(
        &mut self,
        record: &CoreRecord,
        record_sha256: &str,
    ) -> RepositoryEvaluation {
        let mut facts = neutral_facts(record);
        let Some(activity) = record.content.activity.as_ref() else {
            return self.resolver.evaluate(facts);
        };
        let Some(call_id) = activity
            .provider_call_id
            .as_ref()
            .and_then(|call_id| serde_json::to_string(call_id).ok())
        else {
            return self.resolver.evaluate(facts);
        };

        if let Some(invocation) = activity.invocation.as_ref() {
            reconcile_codex_exec_command(record, invocation, &mut facts);
            match exact_file_invocations(record, invocation) {
                FileInvocationExtraction::Exact(evidence) => {
                    facts.repository_file_invocation_evidence = evidence;
                }
                FileInvocationExtraction::Rejected => {
                    facts.provider_native_context_ambiguous = true;
                }
                FileInvocationExtraction::NotApplicable => {}
            }
            let pending = PendingInvocation {
                call_id: call_id.clone(),
                event_sequence: record.event_sequence,
                command: facts.command.clone(),
                session_cwd: facts.session_cwd.clone(),
                declared_workdir: facts.declared_tool_workdir.clone(),
            };
            match self.pending_results.remove(&call_id) {
                Some(Some(result)) => apply_linked_result(&mut facts, &pending, &result),
                Some(None) => {
                    facts.repository_file_invocation_evidence.clear();
                    facts.provider_native_context_ambiguous = true;
                }
                None => {
                    let duplicate = match self.pending.entry(call_id.clone()) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(Some(pending));
                            false
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            entry.insert(None);
                            true
                        }
                    };
                    if duplicate {
                        facts.repository_file_invocation_evidence.clear();
                        facts.provider_native_context_ambiguous = true;
                    }
                }
            }
            while self.pending.len() > MAX_PENDING_INVOCATIONS {
                let Some(oldest) = self.pending.keys().next().cloned() else {
                    break;
                };
                self.pending.remove(&oldest);
            }
        }

        if let Some(result) = activity.result.as_ref()
            && let Some(result_output) = exact_result_output(record, result)
            && let Some(result_record_sha256) = decode_sha256(record_sha256)
        {
            let result = PendingResult {
                result_call_id: call_id.clone(),
                result_record_sha256,
                observed_at_unix_ms: result
                    .completed_at_unix_ms
                    .or(record.occurred_at_unix_ms)
                    .unwrap_or(i64::MIN),
                result_succeeded: result_succeeded(result.status.as_deref()),
                result_output,
            };
            match self.pending.remove(&call_id) {
                Some(Some(origin)) => apply_linked_result(&mut facts, &origin, &result),
                Some(None) => {
                    facts.provider_native_context_ambiguous = true;
                }
                None => {
                    let duplicate = match self.pending_results.entry(call_id) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(Some(result));
                            false
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            entry.insert(None);
                            true
                        }
                    };
                    if duplicate {
                        facts.provider_native_context_ambiguous = true;
                    }
                }
            }
            while self.pending_results.len() > MAX_PENDING_INVOCATIONS {
                let Some(oldest) = self.pending_results.keys().next().cloned() else {
                    break;
                };
                self.pending_results.remove(&oldest);
            }
        }

        self.resolver.evaluate(facts)
    }
}

#[derive(Debug, Clone)]
struct PendingResult {
    result_call_id: String,
    result_record_sha256: [u8; 32],
    observed_at_unix_ms: i64,
    result_succeeded: bool,
    result_output: Value,
}

fn apply_linked_result(
    facts: &mut NeutralRepositoryFacts,
    origin: &PendingInvocation,
    result: &PendingResult,
) {
    if facts.command.is_none() {
        facts.command.clone_from(&origin.command);
    }
    if facts.session_cwd.is_none() {
        facts.session_cwd.clone_from(&origin.session_cwd);
    }
    if facts.declared_tool_workdir.is_none() {
        facts
            .declared_tool_workdir
            .clone_from(&origin.declared_workdir);
    }
    let Some(command) = origin.command.as_deref() else {
        return;
    };
    let Some(linked) = linked_outcome_evidence(LinkedOutcomeInput {
        provider: "core",
        command,
        session_cwd: origin.session_cwd.as_deref(),
        declared_workdir: origin.declared_workdir.as_deref(),
        origin_call_id: &origin.call_id,
        result_call_id: &result.result_call_id,
        origin_event_sequence: origin.event_sequence,
        continuation_call_id_sha256: &[],
        result_record_sha256: result.result_record_sha256,
        observed_at_unix_ms: result.observed_at_unix_ms,
        result_succeeded: result.result_succeeded,
        result_output: &result.result_output,
        structured_commit_oid: None,
        output_repository_path: None,
    }) else {
        return;
    };
    facts.apply_linked_outcome_evidence(linked);
}

fn reconcile_codex_exec_command(
    record: &CoreRecord,
    invocation: &ctx_history_core::ActivityInvocation,
    facts: &mut NeutralRepositoryFacts,
) {
    match exact_codex_exec_command(record, invocation) {
        CodexExecCommandExtraction::NotApplicable => {}
        CodexExecCommandExtraction::Rejected => {
            facts.command = None;
            facts.declared_tool_workdir = None;
            facts.provider_native_context_ambiguous = true;
        }
        CodexExecCommandExtraction::Exact { command, workdir } => {
            if facts.provider_native_context_ambiguous {
                facts.command = None;
                facts.declared_tool_workdir = None;
                return;
            }
            let command_conflicts = facts
                .command
                .as_ref()
                .is_some_and(|existing| existing != &command);
            let workdir_conflicts = match (&facts.declared_tool_workdir, &workdir) {
                (Some(existing), Some(decoded)) => existing != decoded,
                (Some(_), None) => true,
                (None, Some(_) | None) => false,
            };
            if command_conflicts || workdir_conflicts {
                facts.command = None;
                facts.declared_tool_workdir = None;
                facts.provider_native_context_ambiguous = true;
                return;
            }
            facts.command.get_or_insert(command);
            if let Some(workdir) = workdir {
                facts.declared_tool_workdir.get_or_insert(workdir);
            }
        }
    }
}

fn exact_codex_exec_command(
    record: &CoreRecord,
    invocation: &ctx_history_core::ActivityInvocation,
) -> CodexExecCommandExtraction {
    let source = &record.source;
    let exact_codex_source = source.provider() == "codex"
        && source.source_format() == "codex_session_jsonl"
        && source.schema_variant() == "codex-nativepath-jsonl-v0"
        && source.provider_identity_version() == 1
        && (record.parser_revision == CODEX_CORE_ACTIVITY_REVISION
            || record.parser_revision == CODEX_RELEASED_V14_REVISION
            || record.parser_revision == CODEX_RELEASED_V11_REVISION);
    if !exact_codex_source || invocation.tool != "exec_command" {
        return CodexExecCommandExtraction::NotApplicable;
    }
    if invocation.protocol.is_some() || invocation.server.is_some() {
        return CodexExecCommandExtraction::Rejected;
    }
    let ActivityJsonCapture::Present {
        value: Value::String(arguments),
    } = &invocation.arguments
    else {
        return CodexExecCommandExtraction::Rejected;
    };
    if arguments.len() > MAX_CODEX_EXEC_COMMAND_ARGUMENT_BYTES {
        return CodexExecCommandExtraction::Rejected;
    }
    let Some(decoded) = exact_json_value(arguments) else {
        return CodexExecCommandExtraction::Rejected;
    };
    let Some(object) = decoded.as_object() else {
        return CodexExecCommandExtraction::Rejected;
    };
    let Some(command) = object.get("cmd").and_then(Value::as_str) else {
        return CodexExecCommandExtraction::Rejected;
    };
    if command.is_empty() || command.len() > MAX_CODEX_EXEC_COMMAND_BYTES || command.contains('\0')
    {
        return CodexExecCommandExtraction::Rejected;
    }
    let workdir = match object.get("workdir") {
        None => None,
        Some(Value::String(workdir))
            if !workdir.is_empty()
                && workdir.len() <= MAX_CODEX_EXEC_COMMAND_WORKDIR_BYTES
                && !workdir.contains('\0') =>
        {
            Some(workdir.clone())
        }
        Some(_) => return CodexExecCommandExtraction::Rejected,
    };
    CodexExecCommandExtraction::Exact {
        command: command.to_owned(),
        workdir,
    }
}

fn neutral_facts(record: &CoreRecord) -> NeutralRepositoryFacts {
    let mut neutral = NeutralRepositoryFacts {
        activity_at_unix_ms: record.occurred_at_unix_ms,
        ..NeutralRepositoryFacts::default()
    };
    let Some(activity) = record.content.activity.as_ref() else {
        return neutral;
    };

    let (session_cwd, session_ambiguous) = unique_literal(activity, LiteralFactKind::SessionCwd);
    let (workdir, workdir_ambiguous) = unique_literal(activity, LiteralFactKind::ToolWorkdir);
    let (command, command_ambiguous) = unique_literal(activity, LiteralFactKind::Command);
    neutral.session_cwd = session_cwd;
    neutral.declared_tool_workdir = workdir;
    neutral.command = command;
    neutral.provider_native_context_ambiguous =
        session_ambiguous || workdir_ambiguous || command_ambiguous;
    if command_ambiguous {
        neutral.command_disposition = CommandLiteralDisposition::CommandTooLarge;
    }

    for fact in &activity.facts {
        match fact.kind {
            LiteralFactKind::File => neutral.file_observations.push(LiteralFileObservation {
                path: fact.value.clone(),
                prior_path: None,
                kind: RepositoryFileObservationKind::Unknown,
            }),
            LiteralFactKind::Vcs => neutral.vcs_observations.push(LiteralVcsObservation {
                path: None,
                kind: RepositoryVcsObservationKind::Reference,
                object_id: None,
                parent_object_ids: Vec::new(),
                reference: Some(fact.value.clone()),
            }),
            LiteralFactKind::Commit => {
                if let Some(object_id) = literal_object_id(&fact.value) {
                    neutral.vcs_observations.push(LiteralVcsObservation {
                        path: None,
                        kind: RepositoryVcsObservationKind::Commit,
                        object_id: Some(object_id),
                        parent_object_ids: Vec::new(),
                        reference: None,
                    });
                }
            }
            LiteralFactKind::Branch => neutral.vcs_observations.push(LiteralVcsObservation {
                path: None,
                kind: RepositoryVcsObservationKind::Branch,
                object_id: None,
                parent_object_ids: Vec::new(),
                reference: Some(fact.value.clone()),
            }),
            LiteralFactKind::SessionCwd
            | LiteralFactKind::ToolWorkdir
            | LiteralFactKind::Url
            | LiteralFactKind::Forge
            | LiteralFactKind::Project
            | LiteralFactKind::PullRequest
            | LiteralFactKind::Command
            | LiteralFactKind::Workspace
            | LiteralFactKind::ProviderDisposition => {}
        }
    }
    neutral
}

fn unique_literal(activity: &CoreActivity, kind: LiteralFactKind) -> (Option<String>, bool) {
    let mut value: Option<&str> = None;
    for candidate in activity
        .facts
        .iter()
        .filter(|fact| fact.kind == kind)
        .map(|fact| fact.value.as_str())
    {
        match value {
            None => value = Some(candidate),
            Some(existing) if existing == candidate => {}
            Some(_) => return (None, true),
        }
    }
    (value.map(str::to_owned), false)
}

fn literal_object_id(value: &str) -> Option<GitObjectId> {
    let format = match value.len() {
        40 => GitObjectFormat::Sha1,
        64 => GitObjectFormat::Sha256,
        _ => return None,
    };
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        .then(|| GitObjectId {
            format,
            hex: value.to_owned(),
        })
}

fn exact_result_output(
    record: &CoreRecord,
    result: &ctx_history_core::ActivityResult,
) -> Option<Value> {
    if let ActivityJsonCapture::Present { value } = &result.structured_content {
        return Some(value.clone());
    }
    match &result.text {
        ActivityTextCapture::Present { value } => Some(Value::String(value.clone())),
        ActivityTextCapture::NormalizedBody => record
            .content
            .normalized_body
            .as_ref()
            .map(|value| Value::String(value.clone())),
        ActivityTextCapture::Absent
        | ActivityTextCapture::Unavailable
        | ActivityTextCapture::Omitted { .. } => None,
    }
}

fn result_succeeded(status: Option<&str>) -> bool {
    match status.map(|value| value.to_ascii_lowercase()) {
        None => true,
        Some(value) => matches!(
            value.as_str(),
            "ok" | "success" | "succeeded" | "complete" | "completed" | "done"
        ),
    }
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = (nibble(*value.as_bytes().get(offset)?)? << 4)
            | nibble(*value.as_bytes().get(offset + 1)?)?;
    }
    Some(decoded)
}

const fn nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
