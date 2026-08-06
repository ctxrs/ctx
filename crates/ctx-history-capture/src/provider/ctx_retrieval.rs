use ctx_history_core::CoreDiscoveryExclusion;
use serde_json::Value;

use super::tool_input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContributionClass {
    RetrievalDerived,
    Ordinary,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CtxRetrievalRoute {
    Search,
    ShowEvent,
    ShowSession,
    ListEvents,
    LocateEvent,
    LocateSession,
    QueryEvents,
    Blame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultTerminalStatus {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultAtom {
    Payload,
    /// Exact provider-native structure, never normalized body-text matching.
    KnownProviderEnvelope,
    Diagnostic,
    Unknown,
}

pub(crate) fn reduce_contributions(
    contributions: impl IntoIterator<Item = ContributionClass>,
) -> ContributionClass {
    let mut saw_derived = false;
    let mut saw_unknown = false;
    for contribution in contributions {
        match contribution {
            ContributionClass::RetrievalDerived => saw_derived = true,
            ContributionClass::Ordinary => return ContributionClass::Ordinary,
            ContributionClass::Unknown => saw_unknown = true,
        }
    }
    if saw_derived && !saw_unknown {
        ContributionClass::RetrievalDerived
    } else {
        ContributionClass::Unknown
    }
}

pub(crate) fn discovery_exclusion_for(
    contributions: impl IntoIterator<Item = ContributionClass>,
) -> Option<CoreDiscoveryExclusion> {
    (reduce_contributions(contributions) == ContributionClass::RetrievalDerived)
        .then_some(CoreDiscoveryExclusion::CtxRetrievalDerived)
}

pub(crate) fn classify_direct_cli_tool_input(value: &Value) -> ContributionClass {
    tool_input::direct_argv(value).map_or(ContributionClass::Unknown, |argv| {
        classify_direct_cli_argv(&argv)
    })
}

pub(crate) fn classify_direct_cli_command(command: &str) -> ContributionClass {
    tool_input::direct_command_argv(command).map_or(ContributionClass::Unknown, |argv| {
        classify_direct_cli_argv(&argv)
    })
}

pub(crate) fn classify_direct_cli_argv<T: AsRef<str>>(argv: &[T]) -> ContributionClass {
    let Some(executable) = argv.first().map(AsRef::as_ref) else {
        return ContributionClass::Unknown;
    };
    if !matches!(executable, "ctx" | "ctx.exe") {
        return ContributionClass::Unknown;
    }
    classify_attested_ctx_cli_args(&argv[1..])
}

pub(crate) fn classify_attested_ctx_cli_args<T: AsRef<str>>(args: &[T]) -> ContributionClass {
    let args = args.iter().map(AsRef::as_ref).collect::<Vec<_>>();
    let Some((command, tail, root)) = split_command(&args) else {
        return ContributionClass::Unknown;
    };
    if ctx_cli_route(command, tail, root).is_some() {
        ContributionClass::RetrievalDerived
    } else if is_operational_command(command) {
        ContributionClass::Ordinary
    } else {
        ContributionClass::Unknown
    }
}

fn is_operational_command(command: &str) -> bool {
    matches!(
        command,
        "setup"
            | "status"
            | "stats"
            | "index"
            | "sources"
            | "import"
            | "pro"
            | "referral"
            | "docs"
            | "integrations"
            | "mcp"
            | "daemon"
            | "upgrade"
            | "doctor"
    )
}

pub(crate) fn classify_mcp_invocation(server: &str, tool: &str) -> ContributionClass {
    if server != "ctx" {
        return ContributionClass::Unknown;
    }
    if canonical_mcp_route(server, tool).is_some() {
        ContributionClass::RetrievalDerived
    } else {
        ContributionClass::Ordinary
    }
}

pub(crate) fn canonical_mcp_route(server: &str, tool: &str) -> Option<CtxRetrievalRoute> {
    if server != "ctx" {
        return None;
    }
    match tool {
        "search" => Some(CtxRetrievalRoute::Search),
        "show_event" => Some(CtxRetrievalRoute::ShowEvent),
        "show_session" => Some(CtxRetrievalRoute::ShowSession),
        "query_events" => Some(CtxRetrievalRoute::QueryEvents),
        "blame" => Some(CtxRetrievalRoute::Blame),
        _ => None,
    }
}

pub(crate) fn classify_linked_result(
    linked_invocation: Option<ContributionClass>,
    terminal_status: ResultTerminalStatus,
    atoms: impl IntoIterator<Item = ResultAtom>,
) -> ContributionClass {
    match terminal_status {
        ResultTerminalStatus::Failed => return ContributionClass::Ordinary,
        ResultTerminalStatus::Unknown => return ContributionClass::Unknown,
        ResultTerminalStatus::Succeeded => {}
    }

    let mut saw_payload = false;
    let mut saw_unknown = false;
    for atom in atoms {
        match atom {
            ResultAtom::Payload => saw_payload = true,
            ResultAtom::KnownProviderEnvelope => {}
            ResultAtom::Diagnostic => return ContributionClass::Ordinary,
            ResultAtom::Unknown => saw_unknown = true,
        }
    }
    if !saw_payload || saw_unknown {
        return ContributionClass::Unknown;
    }
    linked_invocation.unwrap_or(ContributionClass::Unknown)
}

#[derive(Clone, Copy, Default)]
struct RootOptions {
    data_root: bool,
    color: bool,
    quiet: bool,
}

fn split_command<'a>(args: &'a [&'a str]) -> Option<(&'a str, &'a [&'a str], RootOptions)> {
    let mut root = RootOptions::default();
    let mut index = 0;
    while consume_root_option(args, &mut index, &mut root)? {}
    let command = *args.get(index)?;
    if command.starts_with('-') {
        return None;
    }
    Some((command, &args[index + 1..], root))
}

fn consume_root_option(args: &[&str], index: &mut usize, seen: &mut RootOptions) -> Option<bool> {
    let Some(argument) = args.get(*index).copied() else {
        return Some(false);
    };
    let (slot, consumed) = if argument == "--quiet" {
        (&mut seen.quiet, 1)
    } else if argument == "--data-root" {
        let value = args.get(*index + 1)?;
        if value.is_empty() || value.starts_with('-') {
            return None;
        }
        (&mut seen.data_root, 2)
    } else if let Some(value) = argument.strip_prefix("--data-root=") {
        if value.is_empty() {
            return None;
        }
        (&mut seen.data_root, 1)
    } else if argument == "--color" {
        let value = *args.get(*index + 1)?;
        if !matches!(value, "auto" | "always" | "never") {
            return None;
        }
        (&mut seen.color, 2)
    } else if let Some(value) = argument.strip_prefix("--color=") {
        if !matches!(value, "auto" | "always" | "never") {
            return None;
        }
        (&mut seen.color, 1)
    } else {
        return Some(false);
    };
    if *slot {
        return None;
    }
    *slot = true;
    *index += consumed;
    Some(true)
}

#[derive(Clone, Copy)]
struct OptionSpec {
    name: &'static str,
    takes_value: bool,
    repeatable: bool,
    validator: Option<fn(&str) -> bool>,
}

impl OptionSpec {
    const fn flag(name: &'static str) -> Self {
        Self {
            name,
            takes_value: false,
            repeatable: false,
            validator: None,
        }
    }

    const fn value(name: &'static str) -> Self {
        Self {
            name,
            takes_value: true,
            repeatable: false,
            validator: None,
        }
    }

    const fn checked(name: &'static str, validator: fn(&str) -> bool) -> Self {
        Self {
            name,
            takes_value: true,
            repeatable: false,
            validator: Some(validator),
        }
    }

    const fn repeated(name: &'static str) -> Self {
        Self {
            name,
            takes_value: true,
            repeatable: true,
            validator: None,
        }
    }
}

struct ParsedTail<'a> {
    positionals: Vec<&'a str>,
    options: Vec<(&'static str, Option<&'a str>)>,
}

impl ParsedTail<'_> {
    fn has(&self, name: &str) -> bool {
        self.options.iter().any(|(seen, _)| *seen == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find_map(|(seen, value)| (*seen == name).then_some(*value).flatten())
    }

    fn has_nonempty_value(&self, name: &str) -> bool {
        self.options
            .iter()
            .any(|(seen, value)| *seen == name && value.is_some_and(|value| !value.is_empty()))
    }
}

fn parse_tail<'a>(
    args: &[&'a str],
    specs: &[OptionSpec],
    mut root: RootOptions,
) -> Option<ParsedTail<'a>> {
    let mut parsed = ParsedTail {
        positionals: Vec::new(),
        options: Vec::new(),
    };
    let mut index = 0;
    let mut parse_options = true;
    while index < args.len() {
        if parse_options && consume_root_option(args, &mut index, &mut root)? {
            continue;
        }
        let argument = args[index];
        if parse_options && argument == "--" {
            parse_options = false;
            index += 1;
            continue;
        }
        if !parse_options || !argument.starts_with('-') {
            parsed.positionals.push(argument);
            index += 1;
            continue;
        }
        let (name, attached) = argument
            .split_once('=')
            .map_or((argument, None), |(name, value)| (name, Some(value)));
        let spec = specs.iter().find(|spec| spec.name == name)?;
        if !spec.repeatable && parsed.has(name) {
            return None;
        }
        let allow_empty = spec.name == "--term";
        let value = if spec.takes_value {
            let value = match attached {
                Some(value) if !value.is_empty() || allow_empty => value,
                Some(_) => return None,
                None => {
                    index += 1;
                    let value = args.get(index).copied()?;
                    if (!allow_empty && value.is_empty()) || value.starts_with('-') {
                        return None;
                    }
                    value
                }
            };
            if spec.validator.is_some_and(|validator| !validator(value)) {
                return None;
            }
            Some(value)
        } else if attached.is_some() {
            return None;
        } else {
            None
        };
        parsed.options.push((spec.name, value));
        index += 1;
    }
    Some(parsed)
}

fn after_leading_root<'a>(args: &'a [&'a str], root: &mut RootOptions) -> Option<&'a [&'a str]> {
    let mut index = 0;
    while consume_root_option(args, &mut index, root)? {}
    Some(&args[index..])
}

fn ctx_cli_route(command: &str, tail: &[&str], mut root: RootOptions) -> Option<CtxRetrievalRoute> {
    match command {
        "search" => valid_search(tail, root).then_some(CtxRetrievalRoute::Search),
        "show" => {
            let tail = after_leading_root(tail, &mut root)?;
            match tail {
                ["event", rest @ ..] if one_positional(rest, SHOW_EVENT_OPTIONS, root) => {
                    Some(CtxRetrievalRoute::ShowEvent)
                }
                ["session", rest @ ..] if valid_session(rest, SHOW_SESSION_OPTIONS, root) => {
                    Some(CtxRetrievalRoute::ShowSession)
                }
                _ => None,
            }
        }
        "list" => {
            let tail = after_leading_root(tail, &mut root)?;
            matches!(tail, ["events", rest @ ..] if no_positionals(rest, LIST_EVENTS_OPTIONS, root))
                .then_some(CtxRetrievalRoute::ListEvents)
        }
        "locate" => {
            let tail = after_leading_root(tail, &mut root)?;
            match tail {
                ["event", rest @ ..] if one_positional(rest, LOCATE_EVENT_OPTIONS, root) => {
                    Some(CtxRetrievalRoute::LocateEvent)
                }
                ["session", rest @ ..] if valid_session(rest, LOCATE_SESSION_OPTIONS, root) => {
                    Some(CtxRetrievalRoute::LocateSession)
                }
                _ => None,
            }
        }
        "blame" => valid_blame(tail, root).then_some(CtxRetrievalRoute::Blame),
        _ => None,
    }
}

fn valid_search(args: &[&str], root: RootOptions) -> bool {
    parse_tail(args, SEARCH_OPTIONS, root).is_some_and(|tail| {
        tail.positionals.len() <= 1
            && tail.positionals.iter().all(|value| !value.is_empty())
            && (!tail.positionals.is_empty()
                || tail.has_nonempty_value("--term")
                || tail.has("--file"))
            && !(tail.has("--content-scope") && tail.has("--event-type"))
    })
}

fn one_positional(args: &[&str], specs: &[OptionSpec], root: RootOptions) -> bool {
    parse_tail(args, specs, root)
        .is_some_and(|tail| matches!(tail.positionals.as_slice(), [value] if !value.is_empty()))
}

fn no_positionals(args: &[&str], specs: &[OptionSpec], root: RootOptions) -> bool {
    parse_tail(args, specs, root).is_some_and(|tail| {
        tail.positionals.is_empty() && tail.has("--since") == tail.has("--until")
    })
}

fn valid_session(args: &[&str], specs: &[OptionSpec], root: RootOptions) -> bool {
    parse_tail(args, specs, root).is_some_and(|tail| {
        match (tail.positionals.as_slice(), tail.has("--provider-session")) {
            ([value], false) => !value.is_empty(),
            ([], true) => true,
            _ => false,
        }
    })
}

fn valid_blame(args: &[&str], mut root: RootOptions) -> bool {
    let Some(args) = after_leading_root(args, &mut root) else {
        return false;
    };
    match args {
        ["file", tail @ ..] => one_positional(tail, BLAME_FILE_OPTIONS, root),
        ["commit" | "pr", tail @ ..] => one_positional(tail, BLAME_OPTIONS, root),
        _ => parse_tail(args, BLAME_LEGACY_OPTIONS, root).is_some_and(|tail| {
            matches!(tail.positionals.as_slice(), [value] if !value.is_empty())
                && !(tail.has("--lines") && matches!(tail.value("--type"), Some("commit" | "pr")))
        }),
    }
}

macro_rules! unsigned_range {
    ($name:ident, $minimum:literal, $maximum:literal) => {
        fn $name(value: &str) -> bool {
            value
                .parse::<u64>()
                .is_ok_and(|value| ($minimum..=$maximum).contains(&value))
        }
    };
}

unsigned_range!(search_limit, 1, 200);
unsigned_range!(event_window, 0, 50);
unsigned_range!(event_limit, 1, 10_000_000);
unsigned_range!(blame_limit, 1, 100);

fn usize_value(value: &str) -> bool {
    value.parse::<usize>().is_ok()
}

fn semantic_weight(value: &str) -> bool {
    value
        .parse::<f32>()
        .is_ok_and(|value| value.is_finite() && (0.0..=1.0).contains(&value))
}

fn line_range(value: &str) -> bool {
    let mut parts = value.split(':');
    let positive = |value: &str| value.parse::<u32>().ok().filter(|value| *value > 0);
    let Some(start) = parts.next().and_then(positive) else {
        return false;
    };
    let end = parts.next().map_or(Some(start), positive);
    parts.next().is_none() && end.is_some_and(|end| end >= start)
}

macro_rules! one_of {
    ($name:ident: $($value:literal)|+) => {
        fn $name(value: &str) -> bool {
            matches!(value, $($value)|+)
        }
    };
}

one_of!(content_scope: "all" | "transcript" | "calls" | "outputs");
one_of!(backend: "hybrid" | "semantic" | "lexical");
one_of!(refresh: "background" | "off" | "wait");
one_of!(output_format: "text" | "markdown" | "json" | "jsonl");
one_of!(json_format: "text" | "json");
one_of!(transcript_mode: "full" | "lite" | "log");
one_of!(event_scope: "all" | "primary" | "subagent");
one_of!(event_direction: "ascending" | "descending");
one_of!(event_content: "full" | "text" | "none");
one_of!(event_format: "json" | "jsonl");
one_of!(blame_type: "file" | "commit" | "pr");

const SEARCH_OPTIONS: &[OptionSpec] = &[
    OptionSpec::repeated("--term"),
    OptionSpec::checked("--limit", search_limit),
    OptionSpec::value("--provider"),
    OptionSpec::value("--history-source"),
    OptionSpec::value("--provider-key"),
    OptionSpec::value("--source-id"),
    OptionSpec::value("--source-format"),
    OptionSpec::value("--workspace"),
    OptionSpec::value("--since"),
    OptionSpec::flag("--primary-only"),
    OptionSpec::flag("--include-subagents"),
    OptionSpec::checked("--content-scope", content_scope),
    OptionSpec::value("--event-type"),
    OptionSpec::value("--file"),
    OptionSpec::value("--session"),
    OptionSpec::flag("--events"),
    OptionSpec::checked("--backend", backend),
    OptionSpec::checked("--semantic-weight", semantic_weight),
    OptionSpec::checked("--refresh", refresh),
    OptionSpec::flag("--include-current-session"),
    OptionSpec::checked("--format", json_format),
    OptionSpec::flag("--verbose"),
];

const SHOW_EVENT_OPTIONS: &[OptionSpec] = &[
    OptionSpec::checked("--before", event_window),
    OptionSpec::checked("--after", event_window),
    OptionSpec::checked("--window", event_window),
    OptionSpec::checked("--format", output_format),
];
const SHOW_SESSION_OPTIONS: &[OptionSpec] = &[
    OptionSpec::value("--provider"),
    OptionSpec::value("--provider-session"),
    OptionSpec::checked("--mode", transcript_mode),
    OptionSpec::checked("--max-events", usize_value),
    OptionSpec::checked("--format", output_format),
    OptionSpec::value("--out"),
];
const LIST_EVENTS_OPTIONS: &[OptionSpec] = &[
    OptionSpec::value("--since"),
    OptionSpec::value("--until"),
    OptionSpec::repeated("--provider"),
    OptionSpec::value("--source"),
    OptionSpec::value("--history-source"),
    OptionSpec::value("--provider-key"),
    OptionSpec::value("--source-id"),
    OptionSpec::value("--source-format"),
    OptionSpec::value("--provider-session"),
    OptionSpec::value("--session"),
    OptionSpec::value("--parent-session"),
    OptionSpec::value("--root-session"),
    OptionSpec::value("--branch"),
    OptionSpec::value("--workspace"),
    OptionSpec::value("--event-type"),
    OptionSpec::value("--role"),
    OptionSpec::value("--agent-type"),
    OptionSpec::checked("--scope", event_scope),
    OptionSpec::value("--file"),
    OptionSpec::checked("--direction", event_direction),
    OptionSpec::value("--cursor"),
    OptionSpec::checked("--limit", event_limit),
    OptionSpec::checked("--content", event_content),
    OptionSpec::checked("--format", event_format),
];
const LOCATE_EVENT_OPTIONS: &[OptionSpec] = &[OptionSpec::checked("--format", json_format)];
const LOCATE_SESSION_OPTIONS: &[OptionSpec] = &[
    OptionSpec::value("--provider"),
    OptionSpec::value("--provider-session"),
    OptionSpec::checked("--format", json_format),
];
const BLAME_OPTIONS: &[OptionSpec] = &[
    OptionSpec::value("--repository"),
    OptionSpec::checked("--limit", blame_limit),
    OptionSpec::value("--cursor"),
    OptionSpec::checked("--format", json_format),
];
const BLAME_FILE_OPTIONS: &[OptionSpec] = &[
    OptionSpec::checked("--lines", line_range),
    OptionSpec::value("--repository"),
    OptionSpec::checked("--limit", blame_limit),
    OptionSpec::value("--cursor"),
    OptionSpec::checked("--format", json_format),
];
const BLAME_LEGACY_OPTIONS: &[OptionSpec] = &[
    OptionSpec::checked("--type", blame_type),
    OptionSpec::checked("--lines", line_range),
    OptionSpec::value("--repository"),
    OptionSpec::checked("--limit", blame_limit),
    OptionSpec::value("--cursor"),
    OptionSpec::checked("--format", json_format),
];
