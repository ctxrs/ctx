#[path = "../src/provider/ctx_retrieval.rs"]
mod ctx_retrieval;
#[path = "../src/provider/tool_input.rs"]
mod tool_input;

use ctx_history_core::CoreDiscoveryExclusion;
use ctx_retrieval::*;
use serde_json::json;

#[test]
fn direct_cli_accepts_representative_closed_route_syntax() {
    for command in [
        "ctx search incident",
        "ctx search needle --term=",
        "ctx search --term incident --term regression",
        "ctx search --file src/lib.rs --limit 200",
        "ctx show event event-selector --window 50",
        "ctx show session session-selector --mode full",
        "ctx show session --provider-session native --provider opaque",
        "ctx list events",
        "ctx list events --since opaque --until opaque --provider one --provider two",
        "ctx locate event event-selector --format json",
        "ctx locate session --provider-session native",
        "ctx blame src/lib.rs --limit 100",
        "ctx blame file src/lib.rs --lines 1:2",
        "ctx blame commit commit-selector",
        "ctx blame pr 12",
        "ctx --data-root /tmp/ctx --color=never search --quiet incident",
        "ctx.exe search windows",
    ] {
        assert_eq!(
            classify_direct_cli_command(command),
            ContributionClass::RetrievalDerived,
            "rejected {command:?}"
        );
    }
    assert_eq!(
        classify_attested_ctx_cli_args(&["show", "event", "selector"]),
        ContributionClass::RetrievalDerived
    );
}

#[test]
fn direct_cli_rejects_missing_unknown_duplicate_conflicting_and_extra_syntax() {
    for command in [
        "ctx search",
        "ctx search --term",
        "ctx search --term=",
        "ctx search '' --term=",
        "ctx search incident extra",
        "ctx search incident --unknown",
        "ctx search incident --limit --quiet",
        "ctx search incident --limit one --limit two",
        "ctx search incident --limit 0",
        "ctx search incident --limit 201",
        "ctx search incident --limit many",
        "ctx search incident --backend unknown",
        "ctx search incident --refresh unknown",
        "ctx search incident --content-scope unknown",
        "ctx search incident --semantic-weight NaN",
        "ctx search incident --format yaml",
        "ctx search incident --content-scope calls --event-type tool_call",
        "ctx show",
        "ctx show event",
        "ctx show event selector extra",
        "ctx show event selector --help",
        "ctx show event selector --window 51",
        "ctx show event selector --format yaml",
        "ctx show session",
        "ctx show session selector --provider-session native",
        "ctx show session selector --mode unknown",
        "ctx show session selector --max-events many",
        "ctx list",
        "ctx list events extra",
        "ctx list events --since only",
        "ctx list events --scope unknown",
        "ctx list events --direction unknown",
        "ctx list events --content unknown",
        "ctx list events --format yaml",
        "ctx list events --limit 0",
        "ctx list events --limit 10000001",
        "ctx locate unknown selector",
        "ctx locate event",
        "ctx locate event selector --format yaml",
        "ctx locate session selector extra",
        "ctx blame",
        "ctx blame file",
        "ctx blame commit selector --lines 1",
        "ctx blame target --type unknown",
        "ctx blame target --lines 0",
        "ctx blame target --lines 4:2",
        "ctx blame target --limit 101",
        "ctx blame target --format yaml",
        "ctx blame target --type commit --lines 1",
        "ctx blame target extra",
        "ctx --help",
        "ctx --version",
        "ctx search --help",
        "ctx search --version",
        "ctx --quiet --quiet search incident",
        "ctx --color unknown search incident",
    ] {
        assert_eq!(
            classify_direct_cli_command(command),
            ContributionClass::Unknown,
            "accepted {command:?}"
        );
    }
}

#[test]
fn direct_cli_requires_bare_static_execution_and_exact_tool_input_fields() {
    for command in [
        "/tmp/ctx search incident",
        "myctx search incident",
        "echo 'ctx search incident'",
        "env ctx search incident",
        "cd /tmp && ctx search incident",
        "ctx search incident | tee result",
        "ctx search $(dynamic)",
        "ctx search 'unterminated",
    ] {
        assert_ne!(
            classify_direct_cli_command(command),
            ContributionClass::RetrievalDerived,
            "accepted {command:?}"
        );
    }

    for input in [
        json!("ctx search incident"),
        json!({"cmd": "ctx search incident"}),
        json!({"command": "ctx show event selector"}),
        json!({"shell_command": "ctx list events"}),
        json!(r#"{"cmd":"ctx locate event selector"}"#),
    ] {
        assert_eq!(
            classify_direct_cli_tool_input(&input),
            ContributionClass::RetrievalDerived,
            "rejected {input:?}"
        );
    }
    for input in [
        json!({"arguments": {"cmd": "ctx search nested"}}),
        json!({"cmd": "ctx search one", "command": "ctx search two"}),
        json!("echo \"cmd: 'ctx search incident'\""),
        json!("tools.exec_command({cmd:'ctx search incident'});"),
        json!(r#"{"input":{"cmd":"ctx search nested"}}"#),
    ] {
        assert_eq!(
            classify_direct_cli_tool_input(&input),
            ContributionClass::Unknown,
            "accepted {input:?}"
        );
    }
}

#[test]
fn operational_cli_and_canonical_mcp_identity_remain_exact() {
    assert_eq!(
        classify_direct_cli_command("ctx docs search ranking"),
        ContributionClass::Ordinary
    );
    for tool in [
        "search",
        "show_event",
        "show_session",
        "query_events",
        "blame",
    ] {
        assert_eq!(
            classify_mcp_invocation("ctx", tool),
            ContributionClass::RetrievalDerived
        );
    }
    for (server, tool, expected) in [
        ("CTX", "search", ContributionClass::Unknown),
        ("renamed-ctx", "search", ContributionClass::Unknown),
        ("ctx", "Search", ContributionClass::Ordinary),
        ("ctx", "status", ContributionClass::Ordinary),
    ] {
        assert_eq!(classify_mcp_invocation(server, tool), expected);
    }
}

#[test]
fn contribution_reduction_requires_nonempty_all_derived_input() {
    assert_eq!(reduce_contributions([]), ContributionClass::Unknown);
    assert_eq!(
        reduce_contributions([
            ContributionClass::RetrievalDerived,
            ContributionClass::RetrievalDerived,
        ]),
        ContributionClass::RetrievalDerived
    );
    assert_eq!(
        reduce_contributions([
            ContributionClass::RetrievalDerived,
            ContributionClass::Ordinary,
        ]),
        ContributionClass::Ordinary
    );
    assert_eq!(
        reduce_contributions([
            ContributionClass::RetrievalDerived,
            ContributionClass::Unknown,
        ]),
        ContributionClass::Unknown
    );
    assert_eq!(
        discovery_exclusion_for([ContributionClass::RetrievalDerived]),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
}

#[test]
fn linked_results_require_success_payload_only_and_keep_diagnostics_searchable() {
    let derived = Some(ContributionClass::RetrievalDerived);
    for atoms in [
        vec![ResultAtom::Payload],
        vec![ResultAtom::KnownProviderEnvelope, ResultAtom::Payload],
    ] {
        assert_eq!(
            classify_linked_result(derived, ResultTerminalStatus::Succeeded, atoms),
            ContributionClass::RetrievalDerived
        );
    }
    assert_eq!(
        classify_linked_result(
            derived,
            ResultTerminalStatus::Succeeded,
            [ResultAtom::Payload, ResultAtom::Diagnostic]
        ),
        ContributionClass::Ordinary
    );
    assert_eq!(
        classify_linked_result(
            derived,
            ResultTerminalStatus::Succeeded,
            [ResultAtom::Payload, ResultAtom::Unknown]
        ),
        ContributionClass::Unknown
    );
    assert_eq!(
        classify_linked_result(
            derived,
            ResultTerminalStatus::Succeeded,
            [ResultAtom::KnownProviderEnvelope]
        ),
        ContributionClass::Unknown
    );
    assert_eq!(
        classify_linked_result(derived, ResultTerminalStatus::Failed, [ResultAtom::Payload]),
        ContributionClass::Ordinary
    );
    assert_eq!(
        classify_linked_result(
            derived,
            ResultTerminalStatus::Unknown,
            [ResultAtom::Payload]
        ),
        ContributionClass::Unknown
    );
    assert_eq!(
        classify_linked_result(None, ResultTerminalStatus::Succeeded, [ResultAtom::Payload]),
        ContributionClass::Unknown
    );
}
