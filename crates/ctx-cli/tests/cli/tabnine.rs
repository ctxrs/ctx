#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn tabnine_cli_imports_explicit_agent_home_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("tabnine-cli/.tabnine/agent");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "tabnine",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "tabnine");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "tabnine_cli_chat_recording_jsonl"
    );
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 2);
    assert_eq!(imported["totals"]["imported_events"], 6);

    let search = json_output(ctx(&temp).args([
        "search",
        "tabnine jsonl oracle answer",
        "--provider",
        "tabnine",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(
        &search,
        "tabnine",
        "tabnine jsonl oracle answer",
        1,
        "message",
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "tabnine",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}
