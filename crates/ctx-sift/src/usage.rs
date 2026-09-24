// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Read-only reporting of explicitly imported ccusage exports.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Number;

const HELP: &str = "Usage: ctx sift ccusage --import FILE [--json|--csv]

Report an existing local ccusage daily/session JSON export (including daily
--instances). Accepts daily, sessions, or session arrays, or a projects object.
Each row uses date, sessionId, or unified period identifiers and camelCase
inputTokens/outputTokens/cacheCreationTokens/cacheReadTokens/totalTokens/totalCost.
Missing or null metrics are unavailable (JSON null), never assumed to be zero.
Totals are reported only when supplied; conflicting totals are rejected.
An empty [] export is accepted. Combined --sections exports are unsupported.

Cost is USD as reported by ccusage; it may be calculated or incompletely priced,
not an invoice or money saved. No prices or savings are calculated by ctx sift.
This command reads only FILE; it does not run ccusage, fetch packages, scan
history, or read/write ctx sift's measured compaction statistics.
";
const COST_NOTE: &str =
    "USD reported by ccusage; may be calculated or incompletely priced; not an invoice or savings";
const MAX_IMPORT: u64 = 32 * 1024 * 1024;

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    total_tokens: Option<u64>,
    #[serde(skip_deserializing)]
    total_cost: Option<Number>,
}

fn reported_cost(raw: Option<Box<serde_json::value::RawValue>>) -> Result<Option<Number>> {
    raw.map(|raw| {
        // Check the JSON token itself, including with serde's arbitrary_precision feature.
        ensure!(
            raw.get()
                .starts_with(|c: char| c.is_ascii_digit() || c == '-'),
            "totalCost must be a JSON number or null"
        );
        serde_json::from_str(raw.get()).context("invalid totalCost number")
    })
    .transpose()
}

impl Counts {
    fn tokens(&self) -> [Option<u64>; 5] {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_creation_tokens,
            self.cache_read_tokens,
            self.total_tokens,
        ]
    }

    fn cost(&self) -> Result<Option<f64>> {
        self.total_cost
            .as_ref()
            .map(|number| {
                let cost = number
                    .as_f64()
                    .context("totalCost is outside the finite numeric range")?;
                ensure!(
                    cost.is_finite() && cost >= 0.0,
                    "totalCost must be finite and nonnegative"
                );
                // Do not silently validate a nonzero decimal as zero after underflow.
                let decimal = number.to_string();
                let mantissa = decimal.split(['e', 'E']).next().unwrap();
                ensure!(
                    cost != 0.0
                        || !mantissa.contains(['1', '2', '3', '4', '5', '6', '7', '8', '9']),
                    "totalCost is below the supported numeric range"
                );
                Ok(cost)
            })
            .transpose()
    }

    fn validate(&self, exact_components: bool) -> Result<()> {
        self.cost()?;
        let tokens = self.tokens();
        let sum = tokens[..4]
            .iter()
            .flatten()
            .try_fold(0u64, |sum, n| sum.checked_add(*n))
            .context("token counters overflow u64")?;
        if let Some(total) = self.total_tokens {
            ensure!(
                sum <= total,
                "totalTokens is smaller than reported token components"
            );
            if exact_components && tokens[..4].iter().all(Option::is_some) {
                ensure!(
                    sum == total,
                    "totalTokens conflicts with reported token components"
                );
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pricing {
    #[serde(default)]
    missing_pricing: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportedRow {
    date: Option<String>,
    session_id: Option<String>,
    period: Option<String>,
    agent: Option<String>,
    project: Option<String>,
    project_path: Option<String>,
    total_cost: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    model_breakdowns: Vec<Pricing>,
    #[serde(flatten)]
    counts: Counts,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportedTotals {
    total_cost: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    unpriced_models: Vec<String>,
    #[serde(flatten)]
    counts: Counts,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    daily: Option<Vec<ImportedRow>>,
    sessions: Option<Vec<ImportedRow>>,
    session: Option<Vec<ImportedRow>>,
    #[serde(default, deserialize_with = "projects")]
    projects: Option<std::collections::BTreeMap<String, Vec<ImportedRow>>>,
    totals: Option<ImportedTotals>,
}

fn projects<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<std::collections::BTreeMap<String, Vec<ImportedRow>>>, D::Error> {
    // A duplicate project key must not silently discard a group's counters.
    struct UniqueProjects;
    impl<'de> serde::de::Visitor<'de> for UniqueProjects {
        type Value = std::collections::BTreeMap<String, Vec<ImportedRow>>;
        fn expecting(&self, out: &mut std::fmt::Formatter) -> std::fmt::Result {
            out.write_str("an object with unique project keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut projects = Self::Value::new();
            while let Some((key, rows)) = map.next_entry()? {
                if projects.insert(key, rows).is_some() {
                    return Err(serde::de::Error::custom("duplicate project key"));
                }
            }
            Ok(projects)
        }
    }
    deserializer.deserialize_map(UniqueProjects).map(Some)
}

#[derive(Serialize)]
struct Row {
    period: String,
    source: Option<String>,
    project: Option<String>,
    pricing_incomplete: bool,
    #[serde(flatten)]
    counts: Counts,
}

#[derive(Serialize)]
struct Report {
    schema_version: u8,
    source: &'static str,
    kind: &'static str,
    cost_note: &'static str,
    pricing_incomplete: bool,
    rows: Vec<Row>,
    totals: Counts,
}

fn parse(bytes: &[u8]) -> Result<Report> {
    // Legacy ccusage emits [] for no data, without a report type or totals.
    let empty = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .eq(b"[]".iter().copied());
    if empty {
        return Ok(Report {
            schema_version: 1,
            source: "ccusage",
            kind: "empty",
            cost_note: COST_NOTE,
            pricing_incomplete: false,
            rows: vec![],
            totals: Counts::default(),
        });
    }
    let import: Import = serde_json::from_slice(bytes).map_err(|error| {
        anyhow::anyhow!(
            "invalid ccusage daily/session schema at line {}, column {}; use 'ctx sift ccusage --help'",
            error.line(),
            error.column()
        )
    })?;
    ensure!(
        [
            import.daily.is_some(),
            import.sessions.is_some(),
            import.session.is_some(),
            import.projects.is_some()
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
            == 1,
        "expected exactly one ccusage daily, sessions, session, or projects collection"
    );
    let is_daily = import.daily.is_some() || import.projects.is_some();
    let mut entries = Vec::new();
    if let Some(projects) = import.projects {
        for (project, rows) in projects {
            ensure!(!project.is_empty(), "project identifier must not be empty");
            entries.extend(rows.into_iter().map(|row| (Some(project.clone()), row)));
        }
    } else {
        entries.extend(
            import
                .daily
                .or(import.sessions)
                .or(import.session)
                .unwrap()
                .into_iter()
                .map(|row| (None, row)),
        );
    }
    let mut rows = Vec::with_capacity(entries.len());
    let mut exact_components = true;
    for (project, mut row) in entries {
        row.counts.total_cost = reported_cost(row.total_cost)?;
        let unified = row.period.is_some();
        exact_components &= !unified;
        ensure!(
            if is_daily {
                row.session_id.is_none()
            } else {
                row.date.is_none()
            },
            "row identifier does not match report type"
        );
        ensure!(
            !(unified && (row.date.is_some() || row.session_id.is_some())),
            "ambiguous row identifier"
        );
        let period = row
            .period
            .or(if is_daily { row.date } else { row.session_id })
            .context("missing date, sessionId, or period in ccusage row")?;
        ensure!(!period.is_empty(), "row identifier must not be empty");
        ensure!(
            !unified || row.agent.as_ref().is_some_and(|s| !s.is_empty()),
            "unified rows require agent"
        );
        ensure!(
            row.counts.tokens().iter().any(Option::is_some),
            "ccusage row has no reported token counters"
        );
        row.counts.validate(!unified)?;
        if let (Some(group), Some(row_project)) = (&project, &row.project) {
            ensure!(
                group == row_project,
                "row project conflicts with project group"
            );
        }
        rows.push(Row {
            period,
            source: row.agent,
            project: project.or(row.project).or(row.project_path),
            pricing_incomplete: row.model_breakdowns.iter().any(|m| m.missing_pricing),
            counts: row.counts,
        });
    }
    let mut totals = import.totals.unwrap_or_default();
    totals.counts.total_cost = reported_cost(totals.total_cost)?;
    totals.counts.validate(exact_components)?;
    validate_totals(&rows, &totals.counts)?;
    Ok(Report {
        schema_version: 1,
        source: "ccusage",
        kind: if is_daily { "daily" } else { "session" },
        cost_note: COST_NOTE,
        pricing_incomplete: !totals.unpriced_models.is_empty()
            || rows.iter().any(|r| r.pricing_incomplete),
        rows,
        totals: totals.counts,
    })
}

fn validate_totals(rows: &[Row], totals: &Counts) -> Result<()> {
    for (index, total) in totals.tokens().into_iter().enumerate() {
        let mut complete = true;
        let mut sum = 0u64;
        for row in rows {
            if let Some(n) = row.counts.tokens()[index] {
                sum = sum
                    .checked_add(n)
                    .context("aggregate token counters overflow u64")?;
            } else {
                complete = false;
            }
        }
        if let Some(total) = total {
            ensure!(
                sum <= total && (!complete || sum == total),
                "totals conflict with reported row token counters"
            );
        }
    }
    let mut sum = 0.0;
    let mut complete = true;
    for row in rows {
        if let Some(cost) = row.counts.cost()? {
            sum += cost;
        } else {
            complete = false;
        }
    }
    ensure!(
        sum.is_finite(),
        "aggregate totalCost exceeds the finite numeric range"
    );
    if let Some(total) = totals.cost()? {
        // ccusage sums floating-point costs; tolerate rounding, not conflicting amounts.
        let tolerance = sum.abs().max(total.abs()) * 1e-12;
        ensure!(
            sum - total <= tolerance && (!complete || (sum - total).abs() <= tolerance),
            "totals.totalCost conflicts with reported row costs"
        );
    }
    Ok(())
}

fn cells(counts: &Counts) -> Vec<String> {
    counts
        .tokens()
        .into_iter()
        .map(|n| n.map_or_else(|| "unavailable".into(), |n| n.to_string()))
        .chain(std::iter::once(
            counts
                .total_cost
                .as_ref()
                .map_or_else(|| "unavailable".into(), ToString::to_string),
        ))
        .collect()
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn write_report(report: &Report, format: &str, out: &mut impl Write) -> Result<()> {
    if format == "json" {
        serde_json::to_writer_pretty(&mut *out, report)?;
        writeln!(out)?;
    } else if format == "csv" {
        writeln!(
            out,
            "kind,period,source,project,input_tokens,output_tokens,cache_creation_tokens,cache_read_tokens,total_tokens,reported_cost_usd,pricing_incomplete,cost_note"
        )?;
        for row in &report.rows {
            writeln!(
                out,
                "{},{},{},{},{},{},{}",
                report.kind,
                csv_cell(&row.period),
                csv_cell(row.source.as_deref().unwrap_or("unavailable")),
                csv_cell(row.project.as_deref().unwrap_or("unavailable")),
                cells(&row.counts).join(","),
                row.pricing_incomplete,
                csv_cell(COST_NOTE)
            )?;
        }
        writeln!(
            out,
            "totals,unavailable,ccusage,unavailable,{},{},{}",
            cells(&report.totals).join(","),
            report.pricing_incomplete,
            csv_cell(COST_NOTE)
        )?;
    } else {
        writeln!(
            out,
            "ccusage reported usage ({})\n{}",
            report.kind, COST_NOTE
        )?;
        writeln!(
            out,
            "period | input | output | cache creation | cache read | total tokens | reported cost USD"
        )?;
        for row in &report.rows {
            writeln!(
                out,
                "{} (source: {}, project: {}) | {}",
                serde_json::to_string(&row.period)?,
                row.source
                    .as_deref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .unwrap_or_else(|| "unavailable".into()),
                row.project
                    .as_deref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .unwrap_or_else(|| "unavailable".into()),
                cells(&row.counts).join(" | ")
            )?;
        }
        writeln!(
            out,
            "Reported totals | {}",
            cells(&report.totals).join(" | ")
        )?;
        if report.pricing_incomplete {
            writeln!(
                out,
                "ccusage flagged missing pricing; reported cost is incomplete."
            )?;
        }
    }
    Ok(())
}

pub fn run(args: &[OsString]) -> Result<()> {
    run_to(args, &mut io::stdout().lock())
}

pub(crate) fn run_to(args: &[OsString], out: &mut impl Write) -> Result<()> {
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        out.write_all(HELP.as_bytes())?;
        return Ok(());
    }
    let mut path = None;
    let mut format = "text";
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--import" {
            ensure!(path.is_none(), "--import may be specified only once");
            path = Some(args.next().context("--import requires FILE")?);
        } else if arg == "--json" || arg == "--csv" {
            ensure!(format == "text", "choose only one of --json or --csv");
            format = if arg == "--json" { "json" } else { "csv" };
        } else {
            bail!("unknown ccusage option; use 'ctx sift ccusage --help'");
        }
    }
    let path = path.context("ccusage requires --import FILE; use 'ctx sift ccusage --help'")?;
    ensure!(path != "-", "--import requires a local file, not stdin");
    ensure!(
        std::fs::metadata(path)
            .context("cannot inspect ccusage import file")?
            .is_file(),
        "ccusage import must be a regular file"
    );
    let file = File::open(path).context("cannot open ccusage import file")?;
    ensure!(
        file.metadata()?.is_file(),
        "ccusage import must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT + 1)
        .read_to_end(&mut bytes)
        .context("cannot read ccusage import file")?;
    ensure!(
        bytes.len() as u64 <= MAX_IMPORT,
        "ccusage import exceeds 32 MiB"
    );
    write_report(&parse(&bytes)?, format, out)
}
