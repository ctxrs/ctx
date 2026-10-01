use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use super::super::checkpoint::{
    CodexTerminalAuthorityCheckpointV0, MAX_CODEX_TERMINAL_AUTHORITIES,
};
use super::*;

const TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/terminal-call-id/v1\0";

#[derive(Debug, Clone, Copy, Default)]
struct CodexTerminalAuthorityState {
    candidates: u8,
    mcp_terminal: bool,
    mcp_model_output: bool,
    builtin_exec_invocation: bool,
}

impl CodexTerminalAuthorityState {
    fn is_unique(self) -> bool {
        !(self.builtin_exec_invocation && self.mcp_terminal)
            && (self.candidates == 1
                || (self.candidates == 2 && self.mcp_terminal && self.mcp_model_output))
    }
}

#[derive(Debug, Default)]
pub(super) struct CodexTerminalAuthority {
    prior_fingerprints: BTreeSet<u64>,
    // Exact per-scan evidence costs O(C) keys for C distinct calls.
    // Compact checkpoint capacity must not limit ordinary session operations.
    call_ids: BTreeMap<[u8; 32], CodexTerminalAuthorityState>,
    linkage_unknown: bool,
    prior_saturated: bool,
    append_resume: bool,
    replacement_required: bool,
}

impl CodexTerminalAuthority {
    pub(super) fn restore(&mut self, checkpoint: &CodexTerminalAuthorityCheckpointV0) -> bool {
        if self.append_resume
            || self.linkage_unknown
            || self.prior_saturated
            || !self.prior_fingerprints.is_empty()
            || !self.call_ids.is_empty()
            || self.replacement_required
        {
            return false;
        }
        self.append_resume = true;
        match checkpoint.fingerprints() {
            Some(fingerprints) => self.prior_fingerprints.extend(fingerprints.iter().copied()),
            None => self.prior_saturated = true,
        }
        true
    }

    pub(super) fn observe_record(&mut self, record: &[u8]) {
        match classify_codex_record(record) {
            Ok(probe) if probe.lineage_malformed() => {
                if terminal_call_id(&probe).is_some()
                    || classify_after_selector_ambiguity(record)
                        .as_ref()
                        .and_then(terminal_call_id)
                        .is_some()
                {
                    // The bounded classifier deliberately retains only the
                    // first and last duplicate selectors. Once a terminal's
                    // linkage selectors are ambiguous, fail open for every
                    // result instead of guessing which hidden value owns it.
                    self.invalidate_linkage();
                }
            }
            Ok(probe) if matches!(probe.class, CodexRecordClass::ExcludedResult(kind) if kind.is_call_terminal()) => {
                if let Some(call_id) = terminal_call_id(&probe) {
                    self.observe_call_id(call_id, mcp_carrier(record, probe.class));
                }
            }
            Ok(probe) if probe.class == CodexRecordClass::Retained(CodexRetainedKind::ToolCall) => {
                if let Some(call_id) = probe.call_id.as_deref() {
                    if builtin_exec_invocation(record) {
                        let digest = terminal_call_id_digest(call_id);
                        self.call_ids
                            .entry(digest)
                            .or_default()
                            .builtin_exec_invocation = true;
                        // An appended invocation can contradict a prefix MCP
                        // result too; the checkpoint does not retain its kind.
                        self.replacement_required |= self.append_resume
                            && (self.prior_saturated
                                || self
                                    .prior_fingerprints
                                    .contains(&terminal_call_id_fingerprint(&digest)));
                    }
                }
            }
            // Only an unambiguous notification can be excluded. The malformed
            // selector branch above still handles every result-shaped record.
            Ok(_) => {}
            Err(_) => {
                if let Some(call_id) = classify_after_selector_ambiguity(record)
                    .as_ref()
                    .and_then(terminal_call_id)
                {
                    self.observe_call_id(call_id, None);
                }
            }
        }
    }

    pub(super) fn invalidate_linkage(&mut self) {
        self.replacement_required |= self.append_resume;
        self.prior_fingerprints.clear();
        self.call_ids.clear();
        self.linkage_unknown = true;
    }

    pub(super) fn is_unique(&self, call_id: &str) -> bool {
        !self.linkage_unknown
            && !self.replacement_required
            && self
                .call_ids
                .get(&terminal_call_id_digest(call_id))
                .is_some_and(|state| state.is_unique())
    }

    pub(super) fn append_requires_replacement(&self) -> bool {
        self.replacement_required
    }

    pub(super) fn pending_mcp_requires_replacement(
        &self,
        pending: &BTreeMap<String, CodexPendingCallV0>,
    ) -> bool {
        // Even a standalone MCP end must revisit an unresolved prefix call.
        // Its original tool identity is absent from the compact checkpoint.
        self.append_resume
            && pending.keys().any(|call_id| {
                self.call_ids
                    .get(&terminal_call_id_digest(call_id))
                    .is_some_and(|state| state.mcp_terminal)
            })
    }

    pub(super) fn settle_invocation_linkage(&self, row: &mut CodexCoreRecordDraft) {
        let Some(activity) = row.activity.as_mut() else {
            return;
        };
        let Some(ctx_history_core::TypedKey::Utf8(call_id)) = activity.provider_call_id.as_ref()
        else {
            return;
        };
        if activity.invocation.is_some()
            && (self.linkage_unknown
                || self
                    .call_ids
                    .get(&terminal_call_id_digest(call_id))
                    .is_some_and(|state| state.candidates != 0 && !state.is_unique()))
        {
            // A reverse join may have published its fact on this invocation.
            // Change that owner's Core hash too; absence of a terminal is valid.
            // Unreadable linkage can conceal this ID; checkpoint capacity cannot.
            row.activity = row
                .activity
                .take()
                .and_then(|activity| super::super::rows::facts_only_activity(activity.facts));
        }
    }

    pub(super) fn checkpoint(&self) -> CodexTerminalAuthorityCheckpointV0 {
        if self.linkage_unknown || self.prior_saturated {
            return CodexTerminalAuthorityCheckpointV0::saturated();
        }
        let mut fingerprints = self.prior_fingerprints.clone();
        for (digest, state) in &self.call_ids {
            if state.candidates == 0 {
                continue;
            }
            // Truncation collisions and capacity only prevent compact resume;
            // the full-digest scan above still distinguishes unique calls.
            if !fingerprints.insert(terminal_call_id_fingerprint(digest))
                || fingerprints.len() > MAX_CODEX_TERMINAL_AUTHORITIES
            {
                return CodexTerminalAuthorityCheckpointV0::saturated();
            }
        }
        CodexTerminalAuthorityCheckpointV0::exact(fingerprints.into_iter().collect())
    }

    fn observe_call_id(&mut self, call_id: &str, mcp_carrier: Option<CodexResultKind>) {
        if call_id.is_empty() || call_id.len() > super::super::checkpoint::MAX_CODEX_CALL_ID_BYTES {
            return;
        }
        if self.linkage_unknown {
            self.replacement_required |= self.append_resume;
            return;
        }
        let digest = terminal_call_id_digest(call_id);
        let fingerprint = terminal_call_id_fingerprint(&digest);
        if self.append_resume
            && (self.prior_saturated || self.prior_fingerprints.contains(&fingerprint))
        {
            // Fingerprint matches are deliberately conservative. Equality is
            // possible, so only a replacement scan may decide linkage.
            self.replacement_required = true;
            return;
        }
        let state = self.call_ids.entry(digest).or_default();
        // Legacy MCP persists one execution twice: its self-contained event and
        // its model-facing function output. A second of either kind, or any
        // third candidate, is still ambiguous. This is independent of ordering.
        state.candidates = state.candidates.saturating_add(1).min(3);
        state.mcp_terminal |= mcp_carrier == Some(CodexResultKind::McpToolCallEnd);
        state.mcp_model_output |= mcp_carrier == Some(CodexResultKind::FunctionCallOutput);
        // A suffix-only pair cannot prove there was no conflicting invocation
        // in the certified prefix, including one evicted from pending calls.
        // Reuse full replacement; do not extend the checkpoint wire contract.
        self.replacement_required |=
            self.append_resume && state.mcp_terminal && state.mcp_model_output;
    }
}

fn builtin_exec_invocation(record: &[u8]) -> bool {
    let Ok(audit) = audit_codex_record(record) else {
        return false;
    };
    let Ok(envelope) = serde_json::from_slice::<Value>(record) else {
        return false;
    };
    let Some(payload) = envelope.get("payload") else {
        return false;
    };
    // Reuse the emitted invocation's exact identity, including its namespace
    // abstention. The MCP event cannot contradict a discarded builtin guess.
    super::super::rows::codex_invocation_activity(payload, &audit, DateTime::<Utc>::UNIX_EPOCH)
        .and_then(|activity| activity.invocation)
        .is_some_and(|invocation| {
            invocation.tool == "exec_command"
                && invocation.protocol.is_none()
                && invocation.server.is_none()
        })
}

fn mcp_carrier(record: &[u8], class: CodexRecordClass) -> Option<CodexResultKind> {
    let CodexRecordClass::ExcludedResult(
        kind @ (CodexResultKind::McpToolCallEnd | CodexResultKind::FunctionCallOutput),
    ) = class
    else {
        return None;
    };
    let audit = audit_codex_record(record).ok()?;
    if audit.any_selector_ambiguous() {
        return None;
    }
    let envelope: Value = serde_json::from_slice(record).ok()?;
    let payload = envelope.get("payload")?;
    if kind == CodexResultKind::McpToolCallEnd {
        super::super::rows::codex_mcp_terminal_invocation(
            payload,
            &audit,
            DateTime::<Utc>::UNIX_EPOCH,
        )?;
        let result = payload.get("result")?.as_object()?;
        if result.len() != 1
            || !(result.get("Ok").is_some_and(Value::is_object)
                || result.get("Err").is_some_and(Value::is_string))
        {
            return None;
        }
    } else {
        // Codex McpToolOutput::response_payload adds this header to either a
        // text body or the first input_text item. Exec output has a different
        // envelope. Do not treat an arbitrary function result as its MCP peer.
        if payload.get("status").is_some() {
            return None;
        }
        let output = payload.get("output")?;
        let text = if let Some(text) = output.as_str() {
            text
        } else {
            let first = output.as_array()?.first()?;
            if first.get("type")?.as_str()? != "input_text" {
                return None;
            }
            first.get("text")?.as_str()?
        };
        let (duration, body) = text
            .strip_prefix("Wall time: ")?
            .split_once(" seconds\nOutput:")?;
        let (whole, fraction) = duration.split_once('.')?;
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || fraction.len() != 4
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            || !(body.is_empty() || body.starts_with('\n'))
        {
            return None;
        }
    }
    Some(kind)
}

fn terminal_call_id<'a>(probe: &'a CodexRecordProbe<'_>) -> Option<&'a str> {
    matches!(probe.class, CodexRecordClass::ExcludedResult(_))
        .then_some(probe.call_id.as_deref())
        .flatten()
}

fn terminal_call_id_digest(call_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TERMINAL_CALL_ID_DOMAIN);
    hasher.update(call_id.as_bytes());
    hasher.finalize().into()
}

fn terminal_call_id_fingerprint(digest: &[u8; 32]) -> u64 {
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(call_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": "result"
            }
        }))
        .unwrap()
    }

    #[test]
    fn terminal_authority_detects_cold_and_cross_prefix_duplicates() {
        let mut cold = CodexTerminalAuthority::default();
        cold.observe_record(&terminal("duplicate"));
        cold.observe_record(&terminal("duplicate"));
        assert!(!cold.is_unique("duplicate"));
        assert!(!cold.append_requires_replacement());

        let mut prefix = CodexTerminalAuthority::default();
        prefix.observe_record(&terminal("duplicate"));
        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&terminal("duplicate"));
        assert!(appended.append_requires_replacement());
    }

    fn legacy_mcp_pair(text_output: bool) -> [Value; 2] {
        let result =
            serde_json::json!({"content":[{"type":"text","text":"found"}],"isError":false});
        let mut event = serde_json::json!({"type":"event_msg","payload":{
            "type":"mcp_tool_call_end","call_id":"mcp",
            "invocation":{"server":"ctx","tool":"search","arguments":{"query":"proof"}},
            "duration":{"secs":0,"nanos":42},"result":{"Ok":result}
        }});
        let output = if text_output {
            event["payload"]["result"]["Ok"]["structuredContent"] =
                serde_json::json!({"found":true});
            serde_json::json!("Wall time: 0.0000 seconds\nOutput:\n{\"found\":true}")
        } else {
            serde_json::json!([
                {"type":"input_text","text":"Wall time: 0.0000 seconds\nOutput:"},
                {"type":"input_text","text":"found"}
            ])
        };
        [
            event,
            serde_json::json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"mcp","output":output
            }}),
        ]
    }

    #[test]
    fn legacy_mcp_dual_carriers_are_one_execution_but_same_family_duplicates_abstain() {
        for text_output in [false, true] {
            let pair = legacy_mcp_pair(text_output);
            for order in [[0, 1], [1, 0]] {
                let bytes: Vec<_> = order
                    .iter()
                    .map(|index| serde_json::to_vec(&pair[*index]).unwrap())
                    .collect();
                let mut prefix = CodexTerminalAuthority::default();
                prefix.observe_record(&bytes[0]);
                assert!(prefix.is_unique("mcp"));
                let mut appended = CodexTerminalAuthority::default();
                assert!(appended.restore(&prefix.checkpoint()));
                appended.observe_record(&bytes[1]);
                assert!(
                    appended.append_requires_replacement(),
                    "complementary suffix still needs complete source counts"
                );
                for (duplicate_family, carrier) in pair.iter().enumerate() {
                    for mode in ["unique", "identical", "conflicting", "failed"] {
                        let mut authority = CodexTerminalAuthority::default();
                        for record in &bytes {
                            authority.observe_record(record);
                        }
                        if mode != "unique" {
                            let mut duplicate = carrier.clone();
                            let payload = &mut duplicate["payload"];
                            if mode == "conflicting" {
                                if duplicate_family == 0 {
                                    payload["result"]["Ok"]["content"][0]["text"] =
                                        serde_json::json!("different");
                                } else {
                                    payload["output"] = serde_json::json!(
                                        "Wall time: 0.0000 seconds\nOutput:\ndifferent"
                                    );
                                }
                            } else if mode == "failed" {
                                if duplicate_family == 0 {
                                    payload["result"] = serde_json::json!({"Err":"failed"});
                                } else {
                                    payload["output"] = serde_json::json!("failed");
                                    payload["status"] = serde_json::json!("failed");
                                }
                            }
                            authority.observe_record(&serde_json::to_vec(&duplicate).unwrap());
                        }
                        assert_eq!(
                            authority.is_unique("mcp"),
                            mode == "unique",
                            "{order:?} {duplicate_family} {mode}"
                        );
                        authority.observe_record(&terminal("unrelated"));
                        assert!(authority.is_unique("unrelated"));
                    }
                }
            }
        }
    }

    #[test]
    fn legacy_mcp_pair_requires_supported_invocation_and_model_output_shapes() {
        for mode in [
            "missing-invocation",
            "missing-result",
            "both-result-variants",
            "exec-output",
            "wrong-output-kind",
            "status-claim",
        ] {
            let [mut event, mut output] = legacy_mcp_pair(false);
            match mode {
                "missing-invocation" => {
                    event["payload"]
                        .as_object_mut()
                        .unwrap()
                        .remove("invocation");
                }
                "missing-result" => {
                    event["payload"].as_object_mut().unwrap().remove("result");
                }
                "both-result-variants" => {
                    event["payload"]["result"]["Err"] = serde_json::json!("failed");
                }
                "exec-output" => {
                    output["payload"]["output"] = serde_json::json!(
                        "Script completed\nProcess exited with code 0\nFinal output:\nresult"
                    );
                }
                "wrong-output-kind" => {
                    output["payload"]["type"] = serde_json::json!("custom_tool_call_output");
                }
                "status-claim" => {
                    output["payload"]["status"] = serde_json::json!("failed");
                }
                _ => unreachable!(),
            }
            let mut authority = CodexTerminalAuthority::default();
            authority.observe_record(&serde_json::to_vec(&event).unwrap());
            authority.observe_record(&serde_json::to_vec(&output).unwrap());
            assert!(!authority.is_unique("mcp"), "{mode}");
        }
    }

    #[test]
    fn original_builtin_conflict_settles_before_any_mcp_carrier_order() {
        for original in [
            "exec_command",
            "custom-exec",
            "mcp__ctx__search",
            "namespaced-exec",
            "explicit-mcp",
        ] {
            let mut invocation = serde_json::json!({"type":"response_item","payload":{
                "type":"function_call","name":original,"call_id":"mcp",
                "arguments":"{\"cmd\":\"git commit\"}"
            }});
            if original == "custom-exec" {
                invocation["payload"]["type"] = serde_json::json!("custom_tool_call");
                invocation["payload"]["name"] = serde_json::json!("exec_command");
                let arguments = invocation["payload"]
                    .as_object_mut()
                    .unwrap()
                    .remove("arguments")
                    .unwrap();
                invocation["payload"]["input"] = arguments;
            } else if original == "namespaced-exec" {
                invocation["payload"]["name"] = serde_json::json!("exec_command");
                invocation["payload"]["namespace"] = serde_json::json!("mcp__ctx");
            } else if original == "explicit-mcp" {
                invocation["payload"]["name"] = serde_json::json!("exec_command");
                invocation["payload"]["protocol"] = serde_json::json!("mcp");
                invocation["payload"]["server"] = serde_json::json!("ctx");
                invocation["payload"]["tool"] = serde_json::json!("exec_command");
            }
            for failed in [false, true] {
                let [mut mcp, output] = legacy_mcp_pair(true);
                if original == "namespaced-exec" || original == "explicit-mcp" {
                    mcp["payload"]["invocation"]["tool"] = serde_json::json!("exec_command");
                }
                if failed {
                    mcp["payload"]["result"] = serde_json::json!({"Err":"failed"});
                }
                let records =
                    [&invocation, &mcp, &output].map(|row| serde_json::to_vec(row).unwrap());
                for order in [
                    [0, 1, 2],
                    [0, 2, 1],
                    [1, 0, 2],
                    [1, 2, 0],
                    [2, 0, 1],
                    [2, 1, 0],
                ] {
                    let mut authority = CodexTerminalAuthority::default();
                    for index in order {
                        authority.observe_record(&records[index]);
                    }
                    assert_eq!(
                        authority.is_unique("mcp"),
                        !matches!(original, "exec_command" | "custom-exec"),
                        "{original} {failed} {order:?}"
                    );
                    authority.observe_record(&terminal("unrelated"));
                    assert!(authority.is_unique("unrelated"));
                }
            }
        }
    }

    #[test]
    fn invocation_only_prefix_is_not_a_terminal_but_mcp_pair_revisits_it() {
        let invocation = br#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"mcp","arguments":"{}"}}"#;
        let mut prefix = CodexTerminalAuthority::default();
        prefix.observe_record(invocation);
        assert_eq!(prefix.checkpoint().fingerprints(), Some(&[][..]));
        let pair = legacy_mcp_pair(true).map(|row| serde_json::to_vec(&row).unwrap());
        let pending = BTreeMap::from([(
            "mcp".to_owned(),
            CodexPendingCallV0 {
                raw_ordinal: 0,
                origin:
                    crate::codex::nativepath::checkpoint::CodexPendingCallOriginV0::CurrentSession,
                result_event_type: ctx_history_core::EventType::CommandOutput,
                discovery_exclusion: None,
            },
        )]);
        for order in [[0, 1], [1, 0]] {
            let mut appended = CodexTerminalAuthority::default();
            assert!(appended.restore(&prefix.checkpoint()));
            appended.observe_record(&pair[order[0]]);
            assert!(!appended.append_requires_replacement());
            assert_eq!(
                appended.pending_mcp_requires_replacement(&pending),
                order[0] == 0
            );
            appended.observe_record(&pair[order[1]]);
            assert!(appended.append_requires_replacement());
        }
        // A normal single output remains eligible, including after append.
        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&pair[1]);
        assert!(appended.is_unique("mcp"));
        assert!(!appended.append_requires_replacement());
        prefix.observe_record(&pair[1]);
        assert!(prefix.is_unique("mcp"));
    }

    #[test]
    fn terminal_authority_keeps_unique_suffix_direct_across_restart() {
        let mut prefix = CodexTerminalAuthority::default();
        prefix.observe_record(&terminal("prefix"));

        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&terminal("suffix"));
        assert!(!appended.append_requires_replacement());
        assert!(appended.is_unique("suffix"));

        let mut restarted = CodexTerminalAuthority::default();
        assert!(restarted.restore(&appended.checkpoint()));
        restarted.observe_record(&terminal("after-restart"));
        assert!(!restarted.append_requires_replacement());
        assert!(restarted.is_unique("after-restart"));
    }

    #[test]
    fn patch_notification_does_not_claim_checkpoint_terminal_authority() {
        let notification = br#"{"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"patch","status":"success","success":true,"stdout":"patched src/main.rs"}}"#;
        let mut prefix = CodexTerminalAuthority::default();
        prefix.observe_record(notification);
        assert_eq!(prefix.checkpoint().fingerprints(), Some(&[][..]));

        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&terminal("patch"));
        assert!(appended.is_unique("patch"));
        assert!(!appended.append_requires_replacement());

        let mut restarted = CodexTerminalAuthority::default();
        assert!(restarted.restore(&appended.checkpoint()));
        restarted.observe_record(notification);
        assert!(!restarted.append_requires_replacement());
        restarted.observe_record(&terminal("patch"));
        assert!(restarted.append_requires_replacement());

        let mut saturated = CodexTerminalAuthority::default();
        assert!(saturated.restore(&CodexTerminalAuthorityCheckpointV0::saturated()));
        saturated.observe_record(notification);
        assert!(!saturated.append_requires_replacement());
        saturated.observe_record(&terminal("patch"));
        assert!(saturated.append_requires_replacement());

        // First/last notification selectors cannot hide a competing terminal.
        prefix.observe_record(br#"{"type":"event_msg","payload":{"type":"patch_apply_end","type":"mcp_tool_call_end","type":"patch_apply_end","call_id":"patch"}}"#);
        prefix.observe_record(&terminal("unrelated"));
        assert!(!prefix.is_unique("unrelated"));
        assert!(prefix.linkage_unknown);
    }

    #[test]
    fn checkpoint_capacity_does_not_limit_unique_terminal_linkage() {
        let mut prefix = CodexTerminalAuthority::default();
        for index in 0..MAX_CODEX_TERMINAL_AUTHORITIES {
            prefix.observe_record(&terminal(&format!("terminal-{index}")));
        }
        assert!(prefix.checkpoint().fingerprints().is_some());

        let mut appended = CodexTerminalAuthority::default();
        assert!(appended.restore(&prefix.checkpoint()));
        appended.observe_record(&terminal("terminal-overflow"));
        assert!(!appended.append_requires_replacement());
        assert!(appended.is_unique("terminal-overflow"));
        assert!(appended.checkpoint().fingerprints().is_none());

        // A saturated prefix cannot rule out overlap. Either terminal forces
        // the existing replacement scan; a message-only append need not.
        for call_id in ["terminal-0", "new-unique-terminal"] {
            let mut restarted = CodexTerminalAuthority::default();
            assert!(restarted.restore(&appended.checkpoint()));
            restarted.observe_record(br#"{"type":"response_item","payload":{"type":"message"}}"#);
            assert!(!restarted.append_requires_replacement());
            restarted.observe_record(&terminal(call_id));
            assert!(restarted.append_requires_replacement());
        }

        prefix.observe_record(&terminal("terminal-overflow"));
        assert!(prefix.checkpoint().fingerprints().is_none());
        for call_id in ["terminal-0", "terminal-4095", "terminal-overflow"] {
            assert!(prefix.is_unique(call_id));
        }
        prefix.observe_record(&terminal("terminal-0"));
        assert!(!prefix.is_unique("terminal-0"));
        assert!(prefix.is_unique("terminal-4095"));
        assert!(prefix.is_unique("terminal-overflow"));
    }

    #[test]
    fn compact_fingerprint_collision_does_not_discard_exact_scan_counts() {
        let mut authority = CodexTerminalAuthority::default();
        let first = [0; 32];
        let mut second = first;
        second[31] = 1;
        for digest in [first, second] {
            authority.call_ids.insert(
                digest,
                CodexTerminalAuthorityState {
                    candidates: 1,
                    ..Default::default()
                },
            );
        }
        assert!(authority.checkpoint().fingerprints().is_none());
        assert_eq!(authority.call_ids.len(), 2);
        assert!(!authority.linkage_unknown);
        assert!(!authority.append_requires_replacement());
    }
}
