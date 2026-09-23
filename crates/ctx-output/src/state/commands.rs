// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use super::*;

#[derive(Default, Debug, Serialize)]
pub struct Usage {
    pub events: Vec<RecordedEvent>,
    pub malformed_records: u64,
}
pub fn usage_at(dir: &Path) -> Result<Usage> {
    let _lock = lock(dir)?;
    let mut usage = Usage::default();
    for name in ["metrics.1.jsonl", "metrics.jsonl"] {
        let file = match File::open(dir.join(name)) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        // Reads are bounded even if an external editor corrupts the log.
        ensure!(
            file.metadata()?.len() <= METRICS_LIMIT,
            "metrics file exceeds 10 MiB limit"
        );
        for line in BufReader::new(file).split(b'\n') {
            match serde_json::from_slice::<RecordedEvent>(&line?) {
                Ok(event) if validate(&event).is_ok() => usage.events.push(event),
                _ => usage.malformed_records += 1,
            }
        }
    }
    Ok(usage)
}
#[derive(Default, Debug, Serialize)]
pub struct Totals {
    pub events: u64,
    pub measured_events: u64,
    pub input_tokens: u128,
    pub output_tokens: u128,
    pub saved_tokens: i128,
    pub input_bytes: u128,
    pub output_bytes: u128,
}
impl Totals {
    fn add(&mut self, event: &Event) {
        self.events += 1;
        self.input_bytes += event.input_bytes as u128;
        self.output_bytes += event.output_bytes as u128;
        if let (Some(input), Some(output)) = (event.input_tokens, event.output_tokens) {
            self.measured_events += 1;
            self.input_tokens += input as u128;
            self.output_tokens += output as u128;
            self.saved_tokens += input as i128 - output as i128;
        }
    }
}
impl Usage {
    pub fn totals(&self) -> Totals {
        let mut total = Totals::default();
        for event in &self.events {
            total.add(event);
        }
        total
    }
}
/// All filters are intersected. Times are Unix milliseconds, inclusive since,
/// exclusive until. Project/command/source comparisons are exact.
#[derive(Default, Debug)]
pub struct UsageQuery {
    pub project: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub command: Option<String>,
    pub source: Option<String>,
}
pub fn query_at(dir: &Path, query: &UsageQuery) -> Result<Usage> {
    ensure!(
        query.since.zip(query.until).is_none_or(|(a, b)| a < b),
        "--since must precede --until"
    );
    let mut usage = usage_at(dir)?;
    usage.events.retain(|event| {
        query
            .project
            .as_ref()
            .is_none_or(|p| event.project.as_ref() == Some(p))
            && query.command.as_ref().is_none_or(|c| &event.command == c)
            && query
                .source
                .as_ref()
                .is_none_or(|s| event.source.as_ref() == Some(s))
            && query.since.is_none_or(|t| event.unix_millis >= t)
            && query.until.is_none_or(|t| event.unix_millis < t)
    });
    Ok(usage)
}
pub fn reset_at(dir: &Path) -> Result<()> {
    let _lock = lock(dir)?;
    for name in ["metrics.jsonl", "metrics.1.jsonl"] {
        remove_if_exists(&dir.join(name))?;
    }
    Ok(())
}
fn args_utf8(args: &[OsString]) -> Result<Vec<&str>> {
    args.iter()
        .map(|s| s.to_str().context("options must be UTF-8"))
        .collect()
}
// Gregorian leap years repeat every 400 years (146097 days). Skip whole
// cycles from 1970, then at most 399 years and eleven months; no locale or TZ.
fn utc_date(epoch_days: u64) -> String {
    let mut year = 1970 + epoch_days / 146097 * 400;
    let mut days = epoch_days % 146097;
    let leap = |year: u64| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    loop {
        let length = if leap(year) { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
    }
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while days >= months[month] {
        days -= months[month];
        month += 1;
    }
    format!("{year:04}-{:02}-{:02}", month + 1, days + 1)
}
const GAIN_HELP: &str = "Usage: ctx output gain [--json|--csv|--format text|json|csv] [--history]
       [--daily] [--weekly] [--monthly] [--graph] [--project [PATH]]
       [--since TIME] [--until TIME] [--command NAME] [--source NAME]
       ctx output gain --reset
Show measured token savings from retained records (all projects by default).
--project defaults to the current checkout; legacy records have no project.
TIME is YYYY-MM-DD at UTC midnight or Unix milliseconds; since includes, until excludes.
Weeks start Monday UTC. --graph uses daily buckets unless a period is selected.
CSV exports totals, selected periods, or --history records. --reset clears metrics only.";
const RECALL_HELP: &str = "Usage: ctx output recall --list
       ctx output recall ID-OR-PREFIX [--stderr] [--from LINE] [--lines COUNT] [--grep TEXT]
Without navigation, writes the entire saved stream as raw bytes.
--from is one-based; --grep is literal and case-sensitive; --lines limits matches.
Navigation returns at most 200 lines by default (maximum --lines 10000).
Only opt-in saved originals are available; commands are never rerun.";
const CONFIG_HELP: &str = "Usage: ctx output config [show|--create]
Show effective settings. --create writes defaults only if no config exists.
SIFT_CONFIG_DIR and SIFT_STATE_DIR override ctx output directories independently.
Otherwise use XDG config/state ctx/output (APPDATA/LOCALAPPDATA on Windows).
Existing Sift data is never moved or searched automatically; select it with overrides.
Output state is independent of the history --data-root option.
Set originals_max_entries, originals_max_bytes (total), and originals_max_days in config.json.
Caps must be positive. Ordinary originals require keep_originals and record_usage.
Explicit semantic selection saves its complete input before omitting passages.
Oversized originals are skipped; measurements are still recorded.";
fn help(args: &[OsString], text: &str, output: &mut impl Write) -> Result<bool> {
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        writeln!(output, "{text}")?;
        return Ok(true);
    }
    Ok(false)
}
pub fn gain(args: &[OsString]) -> Result<()> {
    if help(args, GAIN_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    gain_at(&state_dir()?, args, &mut io::stdout().lock())
}
fn parse_time(value: &str) -> Result<u64> {
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse().context("invalid Unix milliseconds");
    }
    let parts: Vec<_> = value.split('-').collect();
    ensure!(
        parts.len() == 3 && parts[0].len() == 4 && parts[1].len() == 2 && parts[2].len() == 2,
        "time must be YYYY-MM-DD or Unix milliseconds"
    );
    let year: u64 = parts[0].parse().context("invalid year")?;
    let month: usize = parts[1].parse().context("invalid month")?;
    let day: u64 = parts[2].parse().context("invalid day")?;
    ensure!(
        (1970..=9999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day),
        "invalid UTC date"
    );
    let leap_days = |y: u64| y / 4 - y / 100 + y / 400;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let days = (year - 1970) * 365 + leap_days(year - 1) - leap_days(1969)
        + months[..month - 1].iter().sum::<u64>()
        + day
        - 1;
    ensure!(utc_date(days) == value, "invalid UTC date");
    Ok(days * 86_400_000)
}
fn period_start(millis: u64, period: &str) -> String {
    let days = millis / 86_400_000;
    match period {
        "weekly" if days < 4 => "1969-12-29".into(),
        "weekly" => utc_date(days - (days + 3) % 7),
        "monthly" => utc_date(days).rsplit_once('-').unwrap().0.to_owned(),
        _ => utc_date(days),
    }
}
fn csv_row(output: &mut impl Write, fields: &[String]) -> Result<()> {
    for (index, field) in fields.iter().enumerate() {
        if index != 0 {
            write!(output, ",")?;
        }
        if field.contains([',', '"', '\r', '\n']) {
            write!(output, "\"{}\"", field.replace('"', "\"\""))?;
        } else {
            write!(output, "{field}")?;
        }
    }
    write!(output, "\r\n")?;
    Ok(())
}
fn csv_totals(
    output: &mut impl Write,
    period: &str,
    start: &str,
    total: &Totals,
    malformed: u64,
) -> Result<()> {
    csv_row(
        output,
        &[
            period.into(),
            start.into(),
            total.events.to_string(),
            total.measured_events.to_string(),
            total.input_tokens.to_string(),
            total.output_tokens.to_string(),
            total.saved_tokens.to_string(),
            total.input_bytes.to_string(),
            total.output_bytes.to_string(),
            malformed.to_string(),
        ],
    )
}
pub fn gain_at(dir: &Path, args: &[OsString], output: &mut impl Write) -> Result<()> {
    if help(args, GAIN_HELP, output)? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    if args.contains(&"--reset") {
        ensure!(args.len() == 1, "--reset must be used alone");
        reset_at(dir)?;
        writeln!(output, "Usage metrics reset; saved originals retained.")?;
        return Ok(());
    }
    let mut query = UsageQuery::default();
    let mut format = None;
    let mut history = false;
    let mut graph = false;
    let mut periods = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg {
            "--json" | "--csv" | "--format" => {
                let value = match arg {
                    "--json" => "json",
                    "--csv" => "csv",
                    _ => args
                        .next()
                        .context("--format requires text, json, or csv")?,
                };
                ensure!(
                    ["text", "json", "csv"].contains(&value),
                    "unknown gain format {value}"
                );
                ensure!(
                    format.is_none_or(|f| f == value),
                    "choose one output format"
                );
                format = Some(value);
            }
            "--history" => history = true,
            "--graph" => graph = true,
            "--daily" | "--weekly" | "--monthly" => {
                if !periods.contains(&&arg[2..]) {
                    periods.push(&arg[2..]);
                }
            }
            "--project" => {
                let path = if args.peek().is_some_and(|s| !s.starts_with("--")) {
                    PathBuf::from(args.next().unwrap())
                } else {
                    std::env::current_dir()?
                };
                query.project = Some(project_at(&path)?);
            }
            "--since" => {
                query.since = Some(parse_time(args.next().context("--since requires a time")?)?)
            }
            "--until" => {
                query.until = Some(parse_time(args.next().context("--until requires a time")?)?)
            }
            "--command" => {
                query.command = Some(
                    args.next()
                        .context("--command requires a basename/category")?
                        .to_owned(),
                )
            }
            "--source" => {
                query.source = Some(
                    args.next()
                        .context("--source requires a category")?
                        .to_owned(),
                )
            }
            _ => bail!("gain: unknown option {arg}; see gain --help"),
        }
    }
    ensure!(
        query.command.as_deref().is_none_or(valid_label)
            && query.source.as_deref().is_none_or(valid_label),
        "command/source filters must be exact basenames/categories"
    );
    if graph && periods.is_empty() {
        periods.push("daily");
    }
    let format = format.unwrap_or("text");
    ensure!(
        format != "csv" || !history || periods.is_empty(),
        "CSV history and period summaries must be exported separately"
    );
    let usage = query_at(dir, &query)?;
    let totals = usage.totals();
    let grouped: Vec<_> = periods
        .iter()
        .map(|period| {
            let mut buckets = BTreeMap::<String, Totals>::new();
            for event in &usage.events {
                buckets
                    .entry(period_start(event.unix_millis, period))
                    .or_default()
                    .add(event);
            }
            (*period, buckets)
        })
        .collect();
    if format == "json" {
        let mut value =
            serde_json::json!({"totals": totals, "malformed_records": usage.malformed_records});
        if history {
            value["history"] = serde_json::to_value(&usage.events)?;
        }
        for (period, buckets) in &grouped {
            value[format!("{period}_utc")] = serde_json::to_value(buckets)?;
        }
        serde_json::to_writer_pretty(&mut *output, &value)?;
        writeln!(output)?;
    } else if format == "csv" {
        if history {
            writeln!(
                output,
                "unix_millis,project,command,source,measured,input_tokens,output_tokens,saved_tokens,input_bytes,output_bytes,duration_ms,exit_code,original_id\r"
            )?;
            for event in &usage.events {
                let saved = event
                    .input_tokens
                    .zip(event.output_tokens)
                    .map(|(a, b)| (a as i128 - b as i128).to_string());
                csv_row(
                    output,
                    &[
                        event.unix_millis.to_string(),
                        event.project.clone().unwrap_or_default(),
                        event.command.clone(),
                        event.source.clone().unwrap_or_default(),
                        saved.is_some().to_string(),
                        event
                            .input_tokens
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                        event
                            .output_tokens
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                        saved.unwrap_or_default(),
                        event.input_bytes.to_string(),
                        event.output_bytes.to_string(),
                        event.duration_ms.to_string(),
                        event.exit_code.map(|n| n.to_string()).unwrap_or_default(),
                        event.original_id.clone().unwrap_or_default(),
                    ],
                )?;
            }
        } else {
            writeln!(
                output,
                "period,start,events,measured_events,input_tokens,output_tokens,saved_tokens,input_bytes,output_bytes,log_malformed_records\r"
            )?;
            if grouped.is_empty() {
                csv_totals(output, "all", "", &totals, usage.malformed_records)?;
            }
            for (period, buckets) in &grouped {
                for (start, total) in buckets {
                    csv_totals(output, period, start, total, usage.malformed_records)?;
                }
            }
        }
    } else {
        writeln!(
            output,
            "{} events; {} measured; {} input / {} output tokens; {} saved tokens",
            totals.events,
            totals.measured_events,
            totals.input_tokens,
            totals.output_tokens,
            totals.saved_tokens
        )?;
        writeln!(
            output,
            "Unmeasured events excluded from token totals. {} malformed records skipped.",
            usage.malformed_records
        )?;
        if history {
            for event in &usage.events {
                let tokens = match (event.input_tokens, event.output_tokens) {
                    (Some(input), Some(result)) => format!(
                        "{input} -> {result} tokens ({} saved)",
                        input as i128 - result as i128
                    ),
                    _ => "tokens unmeasured".to_owned(),
                };
                let seconds = event.unix_millis / 1000 % 86400;
                let exit = event
                    .exit_code
                    .map_or_else(|| "unknown".to_owned(), |code| code.to_string());
                writeln!(
                    output,
                    "{} {:02}:{:02}:{:02} UTC  {}  {tokens}; {} -> {} bytes; {} ms; exit {exit}; project {}; source {}; original {}",
                    utc_date(event.unix_millis / 86_400_000),
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                    event.command,
                    event.input_bytes,
                    event.output_bytes,
                    event.duration_ms,
                    event.project.as_deref().unwrap_or("unknown"),
                    event.source.as_deref().unwrap_or("unknown"),
                    event.original_id.as_deref().unwrap_or("none")
                )?;
            }
        }
        for (period, buckets) in grouped {
            writeln!(output, "UTC {period}: saved tokens")?;
            let max = buckets
                .values()
                .map(|t| t.saved_tokens.max(0))
                .max()
                .unwrap_or(0)
                .max(1);
            for (start, total) in buckets {
                let graph = if graph {
                    "#".repeat((total.saved_tokens.max(0) * 40 / max) as usize)
                } else {
                    String::new()
                };
                writeln!(output, "{start}: {} {graph}", total.saved_tokens)?;
            }
        }
    }
    Ok(())
}
pub fn recall(args: &[OsString]) -> Result<()> {
    if help(args, RECALL_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    recall_command_at(&state_dir()?, args, &mut io::stdout().lock())
}
pub fn recall_command_at(dir: &Path, args: &[OsString], output: &mut impl Write) -> Result<()> {
    if help(args, RECALL_HELP, output)? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    if args == ["--list"] {
        for entry in list_originals_at(dir)? {
            writeln!(
                output,
                "{}\t{} bytes\t{} unix ms",
                entry.id, entry.bytes, entry.unix_millis
            )?;
        }
        return Ok(());
    }
    let id = args.first().context("recall: expected --list or ID")?;
    let mut options = RecallOptions::default();
    let mut args = args[1..].iter().copied();
    while let Some(arg) = args.next() {
        match arg {
            "--stderr" => options.stderr = true,
            "--from" => {
                options.from = Some(
                    args.next()
                        .context("--from requires a line")?
                        .parse()
                        .context("invalid --from line")?,
                )
            }
            "--lines" => {
                options.lines = Some(
                    args.next()
                        .context("--lines requires a count")?
                        .parse()
                        .context("invalid --lines count")?,
                )
            }
            "--grep" => {
                options.grep = Some(
                    args.next()
                        .context("--grep requires a literal pattern")?
                        .to_owned(),
                )
            }
            _ => bail!("recall: unknown option {arg}"),
        }
    }
    if options.from.is_none() && options.lines.is_none() && options.grep.is_none() {
        recall_at(dir, id, options.stderr, output)
    } else {
        recall_with_options_at(dir, id, &options, output)
    }
}
pub fn config(args: &[OsString]) -> Result<()> {
    if help(args, CONFIG_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    ensure!(
        args.is_empty() || args == ["show"] || args == ["--create"],
        "config: expected show or --create"
    );
    let path = config_path()?;
    if args == ["--create"] {
        Settings::create_default(&path)?;
    }
    serde_json::to_writer_pretty(io::stdout().lock(), &Settings::load_from(&path)?)?;
    writeln!(io::stdout().lock())?;
    Ok(())
}

const SEMANTIC_HELP: &str = "Usage: ctx output semantic enable [--project PATH]
       ctx output semantic shadow [--project PATH]
       ctx output semantic disable
       ctx output semantic status
Enable emits smaller recoverable semantic selections; shadow only records decisions.
Enable and shadow send the current task and eligible Pi grep result passages to TypeSafe.
The API key is read from TYPESAFE_API_KEY and is never stored.";

pub fn semantic(args: &[OsString]) -> Result<()> {
    if help(args, SEMANTIC_HELP, &mut io::stdout().lock())? {
        return Ok(());
    }
    let args = args_utf8(args)?;
    let action = args
        .first()
        .context("semantic: expected enable, shadow, disable, or status")?;
    ensure!(
        ["enable", "shadow", "disable", "status"].contains(action),
        "semantic: expected enable, shadow, disable, or status"
    );
    if *action == "disable" || *action == "status" {
        ensure!(args.len() == 1, "semantic {action} accepts no options");
    }
    let path = config_path()?;
    let mut settings = Settings::load_from(&path)?;
    if *action == "status" {
        let current = std::env::current_dir()
            .ok()
            .and_then(|path| project_at(&path).ok());
        let allowed = current
            .as_ref()
            .is_some_and(|path| settings.semantic_selection.allowed_projects.contains(path));
        writeln!(
            io::stdout().lock(),
            "mode: {}\ncurrent project: {}\ncurrent project allowed: {}\nTYPESAFE_API_KEY: {}",
            match settings.semantic_selection.mode {
                SemanticMode::Off => "off",
                SemanticMode::Shadow => "shadow",
                SemanticMode::Select => "select",
            },
            current.as_deref().unwrap_or("unavailable"),
            allowed,
            if std::env::var_os("TYPESAFE_API_KEY").is_some_and(|v| !v.is_empty()) {
                "set"
            } else {
                "not set"
            }
        )?;
        return Ok(());
    }
    if *action == "disable" {
        settings.semantic_selection.mode = SemanticMode::Off;
        settings.save_to(&path)?;
        writeln!(io::stdout().lock(), "Semantic selection disabled.")?;
        return Ok(());
    }
    let project = match args.as_slice() {
        [_] => project_at(&std::env::current_dir()?)?,
        [_, "--project", value] => project_at(Path::new(value))?,
        [_, value] if value.starts_with("--project=") => {
            project_at(Path::new(value.trim_start_matches("--project=")))?
        }
        _ => bail!("semantic {action}: expected [--project PATH]"),
    };
    if !settings
        .semantic_selection
        .allowed_projects
        .contains(&project)
    {
        settings
            .semantic_selection
            .allowed_projects
            .push(project.clone());
    }
    settings.semantic_selection.mode = if *action == "enable" {
        SemanticMode::Select
    } else {
        SemanticMode::Shadow
    };
    settings.save_to(&path)?;
    writeln!(
        io::stdout().lock(),
        "Semantic {} for {project}. Eligible Pi grep requests send the current task and eligible result passages to TypeSafe; TYPESAFE_API_KEY is read only from the environment.",
        if *action == "enable" {
            "selection enabled"
        } else {
            "shadow mode enabled"
        }
    )?;
    Ok(())
}
