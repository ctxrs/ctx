use std::{
    error::Error,
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

use ctx_history_core::TypedKey;
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRangeCursor, CoreEventRangeDirection, CoreEventRangeDomain,
    CoreEventRangeFilters, CoreEventRangeScope, CoreEventRangeSelection, EventRecord,
    VerifiedIndex, DEFAULT_CORE_EVENT_PAGE_BUDGET, MAX_CORE_EVENT_RANGE_PAGE_ITEMS,
    MAX_SOURCE_EVENT_PAGE_ITEMS,
};
use serde::Serialize;
use uuid::Uuid;

const HELP: &str = r#"Generic Core event-range benchmark probe

Usage:
  cargo run -p ctx-history-index --example event_range_benchmark -- \
    (--data-root PATH | --lexical-root PATH) \
    (--all | --range SINCE_MS UNTIL_MS) [OPTIONS]

Roots:
  --data-root PATH               Use PATH/search/lexical
  --lexical-root PATH            Use this lexical directory directly

Selection:
  --all                         Traverse timestamped and untimestamped events
  --range SINCE_MS UNTIL_MS     Traverse the half-open timestamped range
  --filter FIELD=VALUE          Repeatable exact/contains filter
  --direction asc|desc          Traversal direction (default: asc)

Filter fields:
  provider, source_identity, history_source, provider_key, source_id,
  source_format, provider_session_id, session_id, parent_session_id,
  root_session_id, branch, workspace, event_type, role, agent_type, scope,
  file. Scope values are all, primary, or subagent.

Paging and bounds:
  --page-items N                Item limit per page (default: 256)
  --max-pages N                 Stop after N pages and emit next_cursor
  --max-encoded-core-bytes N    Aggregate encoded-Core page budget
  --max-content-bytes N         Aggregate decoded-content page budget
  --cursor HEX                  Resume from an emitted next_cursor

Validation:
  --check                       Compare a complete traversal with independent
                                exact per-source Core enumeration. This pass is
                                untimed and fully materializes oracle identities;
                                it is not the bounded production query path.
  -h, --help                    Print this help

The probe emits one compact JSON object and never emits event bodies.
"#;

#[derive(Debug)]
enum DomainArgs {
    All,
    Range { since: i64, until: i64 },
}

#[derive(Debug)]
struct Args {
    lexical_root: PathBuf,
    domain: DomainArgs,
    filters: CoreEventRangeFilters,
    page_items: usize,
    maximum_pages: Option<usize>,
    budget: CoreEventPageBudget,
    cursor: Option<CoreEventRangeCursor>,
    check: bool,
}

#[derive(Serialize)]
struct Summary {
    format_version: u32,
    generation: String,
    domain: &'static str,
    direction: &'static str,
    document_count: u64,
    returned_events: u64,
    timestamped_events: u64,
    untimestamped_events: u64,
    pages: u64,
    oversized_singleton_pages: u64,
    encoded_core_bytes: u64,
    content_bytes: u64,
    page_items: usize,
    maximum_encoded_core_bytes: usize,
    maximum_content_bytes: usize,
    terminal: bool,
    next_cursor: Option<String>,
    open_ms: f64,
    first_page_ms: f64,
    total_ms: f64,
    events_per_second: f64,
    encoded_core_bytes_per_second: f64,
    peak_rss_bytes: Option<u64>,
    check_passed: Option<bool>,
    oracle_events: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OracleOrderKey {
    time_class: u8,
    occurred_at_unix_ms: i64,
    event_sequence: u64,
    event_digest: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
struct OracleItem {
    order: OracleOrderKey,
    event_digest: [u8; 32],
}

fn main() {
    if let Err(error) = run() {
        eprintln!("event_range_benchmark: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let raw = std::env::args().skip(1).collect::<Vec<_>>();
    if raw
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        print!("{HELP}");
        return Ok(());
    }
    let args = parse_args(&raw)?;
    if args.check && (args.cursor.is_some() || args.maximum_pages.is_some()) {
        return Err(argument_error(
            "--check requires a complete traversal without --cursor or --max-pages",
        )
        .into());
    }

    let selection = match args.domain {
        DomainArgs::All => CoreEventRangeSelection::all(args.filters)?,
        DomainArgs::Range { since, until } => {
            CoreEventRangeSelection::with_filters(since, until, args.filters)?
        }
    };
    let domain_name = match selection.domain() {
        CoreEventRangeDomain::All => "all",
        CoreEventRangeDomain::Timestamped { .. } => "range",
    };
    let direction_name = match selection.filters().direction {
        CoreEventRangeDirection::Ascending => "asc",
        CoreEventRangeDirection::Descending => "desc",
    };

    let total_started = Instant::now();
    let open_started = Instant::now();
    let index = VerifiedIndex::open_pinned(&args.lexical_root)?;
    let open_duration = open_started.elapsed();
    let generation = index.generation_id().to_owned();
    let document_count = index.document_count();

    let query_started = Instant::now();
    let mut cursor = args.cursor;
    let mut returned_digests = args.check.then(Vec::new);
    let mut returned_events = 0_u64;
    let mut timestamped_events = 0_u64;
    let mut untimestamped_events = 0_u64;
    let mut encoded_core_bytes = 0_u64;
    let mut content_bytes = 0_u64;
    let mut pages = 0_u64;
    let mut oversized_singleton_pages = 0_u64;
    let mut first_page_duration = Duration::ZERO;
    let (terminal, next_cursor) = loop {
        let page_started = Instant::now();
        let page = index.core_event_range_page_with_budget(
            &selection,
            cursor.as_ref(),
            args.page_items,
            args.budget,
        )?;
        if pages == 0 {
            first_page_duration = page_started.elapsed();
        }
        pages = pages
            .checked_add(1)
            .ok_or_else(|| argument_error("page count overflow"))?;
        returned_events = returned_events
            .checked_add(u64::try_from(page.items.len())?)
            .ok_or_else(|| argument_error("event count overflow"))?;
        encoded_core_bytes = encoded_core_bytes
            .checked_add(u64::try_from(page.encoded_core_bytes)?)
            .ok_or_else(|| argument_error("encoded byte count overflow"))?;
        content_bytes = content_bytes
            .checked_add(u64::try_from(page.content_bytes)?)
            .ok_or_else(|| argument_error("content byte count overflow"))?;
        oversized_singleton_pages += u64::from(page.oversized_singleton);
        for event in &page.items {
            if event.occurred_at_unix_ms.is_some() {
                timestamped_events += 1;
            } else {
                untimestamped_events += 1;
            }
            if let Some(digests) = returned_digests.as_mut() {
                digests.push(event.event_id.digest());
            }
        }
        if page.terminal {
            break (true, None);
        }
        cursor = page.next_cursor;
        if args
            .maximum_pages
            .is_some_and(|maximum| pages >= maximum as u64)
        {
            break (false, cursor);
        }
    };
    let query_duration = query_started.elapsed();
    let benchmark_duration = total_started.elapsed();
    // Capture the bounded query path's process peak before the optional oracle
    // performs its intentionally whole-result correctness materialization.
    let benchmark_peak_rss_bytes = peak_rss_bytes();

    let (check_passed, oracle_events) = if args.check {
        let expected = independent_oracle(&index, &selection)?;
        let actual = returned_digests
            .as_ref()
            .expect("check allocates result identities");
        if actual != &expected {
            return Err(argument_error(format!(
                "oracle mismatch: range returned {} identities, per-source enumeration returned {}",
                actual.len(),
                expected.len()
            ))
            .into());
        }
        (Some(true), Some(u64::try_from(expected.len())?))
    } else {
        (None, None)
    };

    let query_seconds = query_duration.as_secs_f64();
    let events_per_second = rate(returned_events, query_seconds);
    let encoded_core_bytes_per_second = rate(encoded_core_bytes, query_seconds);
    let summary = Summary {
        format_version: 1,
        generation,
        domain: domain_name,
        direction: direction_name,
        document_count,
        returned_events,
        timestamped_events,
        untimestamped_events,
        pages,
        oversized_singleton_pages,
        encoded_core_bytes,
        content_bytes,
        page_items: args.page_items,
        maximum_encoded_core_bytes: args.budget.maximum_encoded_core_bytes,
        maximum_content_bytes: args.budget.maximum_content_bytes,
        terminal,
        next_cursor: next_cursor.as_ref().map(|cursor| hex(&cursor.encode())),
        open_ms: millis(open_duration),
        first_page_ms: millis(first_page_duration),
        total_ms: millis(benchmark_duration),
        events_per_second,
        encoded_core_bytes_per_second,
        peak_rss_bytes: benchmark_peak_rss_bytes,
        check_passed,
        oracle_events,
    };
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

fn parse_args(raw: &[String]) -> Result<Args, Box<dyn Error>> {
    let mut lexical_root = None;
    let mut domain = None;
    let mut filters = CoreEventRangeFilters::default();
    let mut page_items = 256_usize;
    let mut maximum_pages = None;
    let mut maximum_encoded_core_bytes = DEFAULT_CORE_EVENT_PAGE_BUDGET.maximum_encoded_core_bytes;
    let mut maximum_content_bytes = DEFAULT_CORE_EVENT_PAGE_BUDGET.maximum_content_bytes;
    let mut cursor = None;
    let mut check = false;
    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--data-root" => {
                let value = next_value(raw, &mut index, "--data-root")?;
                set_once(
                    &mut lexical_root,
                    PathBuf::from(value).join("search").join("lexical"),
                    "lexical root",
                )?;
            }
            "--lexical-root" => {
                let value = next_value(raw, &mut index, "--lexical-root")?;
                set_once(&mut lexical_root, PathBuf::from(value), "lexical root")?;
            }
            "--all" => set_once(&mut domain, DomainArgs::All, "domain")?,
            "--range" => {
                let since = parse_i64(next_value(raw, &mut index, "--range")?, "range start")?;
                let until = parse_i64(next_value(raw, &mut index, "--range")?, "range end")?;
                set_once(&mut domain, DomainArgs::Range { since, until }, "domain")?;
            }
            "--filter" => {
                let value = next_value(raw, &mut index, "--filter")?;
                apply_filter(&mut filters, value)?;
            }
            "--direction" => {
                filters.direction = match next_value(raw, &mut index, "--direction")? {
                    "asc" | "ascending" => CoreEventRangeDirection::Ascending,
                    "desc" | "descending" => CoreEventRangeDirection::Descending,
                    value => {
                        return Err(argument_error(format!("invalid direction {value:?}")).into())
                    }
                };
            }
            "--page-items" => {
                page_items = parse_usize(
                    next_value(raw, &mut index, "--page-items")?,
                    "page item limit",
                )?;
            }
            "--max-pages" => {
                let value =
                    parse_usize(next_value(raw, &mut index, "--max-pages")?, "maximum pages")?;
                if value == 0 {
                    return Err(argument_error("--max-pages must be positive").into());
                }
                maximum_pages = Some(value);
            }
            "--max-encoded-core-bytes" => {
                maximum_encoded_core_bytes = parse_usize(
                    next_value(raw, &mut index, "--max-encoded-core-bytes")?,
                    "encoded-Core byte budget",
                )?;
            }
            "--max-content-bytes" => {
                maximum_content_bytes = parse_usize(
                    next_value(raw, &mut index, "--max-content-bytes")?,
                    "content byte budget",
                )?;
            }
            "--cursor" => {
                let encoded = decode_hex(next_value(raw, &mut index, "--cursor")?)?;
                set_once(
                    &mut cursor,
                    CoreEventRangeCursor::decode(&encoded)?,
                    "--cursor",
                )?;
            }
            "--check" => check = true,
            value => return Err(argument_error(format!("unknown argument {value:?}")).into()),
        }
        index += 1;
    }
    let lexical_root = lexical_root.ok_or_else(|| {
        argument_error("missing required --data-root PATH or --lexical-root PATH")
    })?;
    let domain = domain.ok_or_else(|| argument_error("choose exactly one of --all or --range"))?;
    if !(1..=MAX_CORE_EVENT_RANGE_PAGE_ITEMS).contains(&page_items) {
        return Err(argument_error(format!(
            "--page-items must be in 1..={MAX_CORE_EVENT_RANGE_PAGE_ITEMS}"
        ))
        .into());
    }
    Ok(Args {
        lexical_root,
        domain,
        filters,
        page_items,
        maximum_pages,
        budget: CoreEventPageBudget::new(maximum_encoded_core_bytes, maximum_content_bytes),
        cursor,
        check,
    })
}

fn apply_filter(
    filters: &mut CoreEventRangeFilters,
    specification: &str,
) -> Result<(), Box<dyn Error>> {
    let (field, value) = specification
        .split_once('=')
        .ok_or_else(|| argument_error("--filter must use FIELD=VALUE"))?;
    match field {
        "provider" => filters.providers.push(value.to_owned()),
        "source_identity" => set_once(
            &mut filters.source_identity,
            parse_uuid(value, field)?,
            field,
        )?,
        "history_source" => set_once(&mut filters.history_source, value.to_owned(), field)?,
        "provider_key" => set_once(&mut filters.provider_key, value.to_owned(), field)?,
        "source_id" => set_once(&mut filters.source_id, value.to_owned(), field)?,
        "source_format" => set_once(&mut filters.source_format, value.to_owned(), field)?,
        "provider_session_id" => {
            set_once(&mut filters.provider_session_id, value.to_owned(), field)?
        }
        "session_id" => set_once(&mut filters.session_id, parse_uuid(value, field)?, field)?,
        "parent_session_id" => set_once(
            &mut filters.parent_session_id,
            parse_uuid(value, field)?,
            field,
        )?,
        "root_session_id" => set_once(
            &mut filters.root_session_id,
            parse_uuid(value, field)?,
            field,
        )?,
        "branch" => set_once(&mut filters.branch, value.to_owned(), field)?,
        "workspace" => set_once(&mut filters.workspace, value.to_owned(), field)?,
        "event_type" => set_once(&mut filters.event_type, value.to_owned(), field)?,
        "role" => set_once(&mut filters.role, value.to_owned(), field)?,
        "agent_type" => set_once(&mut filters.agent_type, value.to_owned(), field)?,
        "file" => set_once(&mut filters.file, value.to_owned(), field)?,
        "scope" => {
            filters.scope = match value {
                "all" => CoreEventRangeScope::All,
                "primary" => CoreEventRangeScope::Primary,
                "subagent" => CoreEventRangeScope::Subagent,
                _ => return Err(argument_error(format!("invalid scope {value:?}")).into()),
            };
        }
        _ => return Err(argument_error(format!("unknown filter field {field:?}")).into()),
    }
    Ok(())
}

fn independent_oracle(
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
) -> Result<Vec<[u8; 32]>, Box<dyn Error>> {
    let mut items = Vec::new();
    for certificate in &index.manifest().sources {
        let source = certificate.observation().source();
        let mut cursor = None;
        loop {
            let page = index.core_source_event_page(
                source,
                cursor.as_ref(),
                MAX_SOURCE_EVENT_PAGE_ITEMS,
            )?;
            for item in &page.items {
                if oracle_matches(selection, &item.event) {
                    let (time_class, occurred_at_unix_ms) = match item.occurred_at_unix_ms {
                        Some(timestamp) => (0, timestamp),
                        None => (1, 0),
                    };
                    items.push(OracleItem {
                        order: OracleOrderKey {
                            time_class,
                            occurred_at_unix_ms,
                            event_sequence: item.event_sequence,
                            event_digest: item.event_id.digest(),
                        },
                        event_digest: item.event_id.digest(),
                    });
                }
            }
            if page.terminal {
                break;
            }
            cursor = page.next_cursor;
        }
    }
    items.sort_unstable_by_key(|item| item.order);
    if selection.filters().direction == CoreEventRangeDirection::Descending {
        items.reverse();
    }
    Ok(items.into_iter().map(|item| item.event_digest).collect())
}

fn oracle_matches(selection: &CoreEventRangeSelection, event: &EventRecord) -> bool {
    if let CoreEventRangeDomain::Timestamped {
        since_unix_ms,
        until_unix_ms,
    } = selection.domain()
    {
        if !event
            .occurred_at_unix_ms
            .is_some_and(|timestamp| (since_unix_ms..until_unix_ms).contains(&timestamp))
        {
            return false;
        }
    }
    let filters = selection.filters();
    if (!filters.providers.is_empty()
        && filters
            .providers
            .binary_search_by(|provider| provider.as_str().cmp(&event.provider))
            .is_err())
        || filters
            .source_identity
            .is_some_and(|identity| event.source.identity().as_uuid() != identity)
        || filters
            .source_format
            .as_deref()
            .is_some_and(|value| event.source_format != value)
        || filters
            .provider_session_id
            .as_deref()
            .is_some_and(|value| event.provider_session_id.as_deref() != Some(value))
        || filters
            .session_id
            .is_some_and(|identity| event.session_id.as_uuid() != identity)
        || filters.parent_session_id.is_some_and(|identity| {
            event.parent_session_id.map(|value| value.as_uuid()) != Some(identity)
        })
        || filters
            .root_session_id
            .is_some_and(|identity| event.root_session_id.as_uuid() != identity)
        || filters
            .branch
            .as_deref()
            .is_some_and(|value| event.branch.as_deref() != Some(value))
        || filters
            .event_type
            .as_deref()
            .is_some_and(|value| event.event_type != value)
        || filters
            .role
            .as_deref()
            .is_some_and(|value| event.role.as_deref() != Some(value))
        || filters
            .agent_type
            .as_deref()
            .is_some_and(|value| event.agent_type != value)
        || (filters.scope == CoreEventRangeScope::Primary && !event.is_primary)
        || (filters.scope == CoreEventRangeScope::Subagent && event.is_primary)
    {
        return false;
    }
    if filters.workspace.as_deref().is_some_and(|value| {
        !event
            .workspace
            .as_deref()
            .into_iter()
            .chain(event.cwd.as_deref())
            .any(|candidate| candidate.to_lowercase().contains(value))
    }) {
        return false;
    }
    if filters.file.as_deref().is_some_and(|value| {
        !event
            .touched_files
            .iter()
            .any(|candidate| candidate.to_lowercase().contains(value))
    }) {
        return false;
    }
    if filters.history_source.is_some()
        || filters.provider_key.is_some()
        || filters.source_id.is_some()
    {
        let Some((provider_key, source_id)) = custom_source_identity(event) else {
            return false;
        };
        if filters
            .provider_key
            .as_deref()
            .is_some_and(|value| value != provider_key)
            || filters
                .source_id
                .as_deref()
                .is_some_and(|value| value != source_id)
        {
            return false;
        }
        if let Some(history_source) = filters.history_source.as_deref() {
            let Some((expected_provider, expected_source)) = history_source.split_once('/') else {
                return false;
            };
            if provider_key != expected_provider || source_id != expected_source {
                return false;
            }
        }
    }
    true
}

fn custom_source_identity(event: &EventRecord) -> Option<(&str, &str)> {
    if event.provider != "custom" {
        return None;
    }
    let Some(TypedKey::Composite(values)) = event.native_event_id.as_ref() else {
        return None;
    };
    let [TypedKey::Utf8(provider_key), TypedKey::Utf8(source_id), TypedKey::Utf8(_)] =
        values.as_slice()
    else {
        return None;
    };
    Some((provider_key, source_id))
}

fn next_value<'a>(
    raw: &'a [String],
    index: &mut usize,
    option: &str,
) -> Result<&'a str, Box<dyn Error>> {
    *index += 1;
    raw.get(*index)
        .map(String::as_str)
        .ok_or_else(|| argument_error(format!("{option} requires a value")).into())
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), Box<dyn Error>> {
    if slot.replace(value).is_some() {
        return Err(argument_error(format!("{name} may be specified only once")).into());
    }
    Ok(())
}

fn parse_i64(value: &str, name: &str) -> Result<i64, Box<dyn Error>> {
    value
        .parse()
        .map_err(|_| argument_error(format!("invalid {name} {value:?}")).into())
}

fn parse_usize(value: &str, name: &str) -> Result<usize, Box<dyn Error>> {
    value
        .parse()
        .map_err(|_| argument_error(format!("invalid {name} {value:?}")).into())
}

fn parse_uuid(value: &str, name: &str) -> Result<Uuid, Box<dyn Error>> {
    Uuid::parse_str(value)
        .map_err(|_| argument_error(format!("invalid {name} UUID {value:?}")).into())
}

fn decode_hex(value: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    if !value.len().is_multiple_of(2) {
        return Err(argument_error("cursor hex must contain an even number of digits").into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|_| argument_error("cursor hex is not valid UTF-8"))?;
            u8::from_str_radix(text, 16)
                .map_err(|_| argument_error("cursor contains a non-hex digit"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kibibytes = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kibibytes.checked_mul(1024)
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn rate(value: u64, seconds: f64) -> f64 {
    if seconds > 0.0 {
        value as f64 / seconds
    } else {
        0.0
    }
}

fn argument_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
