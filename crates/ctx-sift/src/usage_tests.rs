// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use crate::usage;

use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sift-usage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn raw(&self, input: &[u8], format: &str) -> anyhow::Result<String> {
        let path = self.0.join("usage.json");
        fs::write(&path, input).unwrap();
        let mut args = vec![OsString::from("--import"), path.into_os_string()];
        if !format.is_empty() {
            args.push(OsString::from(format));
        }
        let mut out = Vec::new();
        let result = usage::run_to(&args, &mut out);
        assert_eq!(fs::read(self.0.join("usage.json")).unwrap(), input);
        assert_eq!(fs::read_dir(&self.0).unwrap().count(), 1);
        if result.is_err() {
            assert!(out.is_empty(), "failure emitted a partial report");
        }
        result.map(|()| String::from_utf8(out).unwrap())
    }
    fn report(&self, input: &Value) -> Value {
        serde_json::from_str(&self.raw(input.to_string().as_bytes(), "--json").unwrap()).unwrap()
    }
    fn rejects(&self, input: &Value) {
        assert!(
            self.raw(input.to_string().as_bytes(), "--json").is_err(),
            "accepted {input}"
        );
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Hand-written synthetic amounts, matching ccusage's public field names.
fn counts() -> Value {
    json!({"inputTokens":100,"outputTokens":20,"cacheCreationTokens":30,"cacheReadTokens":50,"totalTokens":200,"totalCost":0.125})
}
fn daily() -> Value {
    let mut row = counts();
    row["date"] = json!("2026-01-02");
    row["modelsUsed"] = json!(["synthetic-model"]);
    row["modelBreakdowns"] = json!([]);
    json!({"daily":[row],"totals":counts()})
}

#[test]
fn daily_reports_only_imported_values_in_all_formats() {
    let s = Sandbox::new();
    let input = daily();
    let report = s.report(&input);
    assert_eq!(report["source"], "ccusage");
    assert_eq!(report["kind"], "daily");
    assert_eq!(report["totals"], counts());
    assert_eq!(report["rows"][0]["inputTokens"], 100);
    assert_eq!(report["rows"][0]["period"], "2026-01-02");
    assert!(report.get("saved_tokens").is_none());
    assert!(report.get("money_saved").is_none());
    let text = s.raw(input.to_string().as_bytes(), "").unwrap();
    assert!(text.contains("100 | 20 | 30 | 50 | 200 | 0.125"));
    assert!(text.contains("not an invoice or savings"));
    let csv = s.raw(input.to_string().as_bytes(), "--csv").unwrap();
    assert!(csv.contains("reported_cost_usd"));
    assert!(csv.contains("100,20,30,50,200,0.125,false"));
    assert_eq!(csv.lines().count(), 3);
}

#[test]
fn sessions_projects_and_unified_shapes_are_supported() {
    let s = Sandbox::new();
    let mut session = counts();
    session["sessionId"] = json!("synthetic-session");
    session["projectPath"] = json!("synthetic-project");
    let report = s.report(&json!({"sessions":[session],"totals":counts()}));
    assert_eq!(report["kind"], "session");
    assert_eq!(report["rows"][0]["project"], "synthetic-project");
    let d = daily();
    let report = s.report(&json!({"projects":{"synthetic-project":d["daily"]},"totals":counts()}));
    assert_eq!(report["rows"][0]["project"], "synthetic-project");
    for kind in ["daily", "session"] {
        let mut row = counts();
        row["period"] = json!("synthetic-period");
        row["agent"] = json!("all");
        // Unified agents may include additional tokens not exposed in the four counters.
        row["totalTokens"] = json!(205);
        let mut totals = counts();
        totals["totalTokens"] = json!(205);
        let report = s.report(&json!({kind:[row],"totals":totals}));
        assert_eq!(report["rows"][0]["source"], "all");
        assert_eq!(report["totals"]["totalTokens"], 205);
    }
}

#[test]
fn missing_and_null_metrics_never_become_zero_or_derived_totals() {
    let s = Sandbox::new();
    let input = json!({"daily":[{"date":"2026-01-02","inputTokens":7,"outputTokens":null}]});
    let report = s.report(&input);
    assert_eq!(report["rows"][0]["inputTokens"], 7);
    for name in [
        "outputTokens",
        "cacheCreationTokens",
        "cacheReadTokens",
        "totalTokens",
        "totalCost",
    ] {
        assert!(report["rows"][0][name].is_null());
    }
    assert!(report["totals"]["inputTokens"].is_null());
    let text = s.raw(input.to_string().as_bytes(), "").unwrap();
    assert!(
        text.contains("7 | unavailable | unavailable | unavailable | unavailable | unavailable")
    );
    assert!(
        s.raw(input.to_string().as_bytes(), "--csv")
            .unwrap()
            .contains("7,unavailable,unavailable")
    );
    let empty: Value = serde_json::from_str(&s.raw(b" [ \n ] ", "--json").unwrap()).unwrap();
    assert_eq!(empty["kind"], "empty");
    assert!(empty["totals"]["totalCost"].is_null());
    assert_eq!(s.report(&json!({"daily":[]}))["rows"], json!([]));
}

#[test]
fn pricing_uncertainty_and_reported_zero_survive_import() {
    let s = Sandbox::new();
    let mut input = daily();
    input["daily"][0]["totalCost"] = json!(0);
    input["totals"]["totalCost"] = json!(0);
    input["daily"][0]["modelBreakdowns"] =
        json!([{"modelName":"synthetic-unknown","missingPricing":true}]);
    input["totals"]["unpricedModels"] = json!(["synthetic-unknown"]);
    let report = s.report(&input);
    assert_eq!(report["totals"]["totalCost"], 0);
    assert_eq!(report["pricing_incomplete"], true);
    assert_eq!(report["rows"][0]["pricing_incomplete"], true);
    assert!(
        s.raw(input.to_string().as_bytes(), "")
            .unwrap()
            .contains("cost is incomplete")
    );
}

#[test]
fn malformed_ambiguous_and_unrecognized_shapes_fail() {
    let s = Sandbox::new();
    for input in [
        json!({}),
        json!([counts()]),
        json!({"totals":counts()}),
        json!({"type":"session","data":[],"summary":{}}),
        json!({"daily":[],"sessions":[]}),
        json!({"daily":[],"monthly":[]}),
        json!({"daily":[counts()]}),
        json!({"daily":[{"date":"2026-01-02"}]}),
        json!({"daily":[{"date":"","inputTokens":0}]}),
        json!({"daily":[{"sessionId":"session","inputTokens":0}]}),
        json!({"daily":[{"date":"day","period":"day","inputTokens":0}]}),
        json!({"daily":[{"period":"day","inputTokens":0}]}),
        json!({"projects":{"a":[{"date":"day","project":"b","inputTokens":0}]}}),
    ] {
        s.rejects(&input);
    }
    for raw in [
        b"not json".as_slice(),
        b"{} {}",
        b"[\x0b]",
        b"{\"projects\":{\"a\":[{\"date\":\"day\",\"inputTokens\":1}],\"a\":[]}}",
        b"{\"daily\":[],\"daily\":[]}",
        b"{\"daily\":[{\"date\":\"day\",\"inputTokens\":1,\"inputTokens\":2}]}",
        b"{\"daily\":[],\"totals\":{\"inputTokens\":0,\"inputTokens\":1}}",
    ] {
        assert!(s.raw(raw, "--json").is_err());
    }
}

#[test]
fn numeric_limits_and_conflicting_totals_fail_without_partial_output() {
    let s = Sandbox::new();
    for field in [
        "inputTokens",
        "outputTokens",
        "cacheCreationTokens",
        "cacheReadTokens",
        "totalTokens",
    ] {
        for bad in [
            json!(-1),
            json!(1.5),
            json!("12"),
            json!(true),
            json!([]),
            json!({}),
        ] {
            let mut input = daily();
            input["daily"][0][field] = bad;
            s.rejects(&input);
        }
    }
    for n in ["18446744073709551616", "1e500", "-1", "0.5"] {
        assert!(
            s.raw(
                format!("{{\"daily\":[{{\"date\":\"day\",\"inputTokens\":{n}}}]}}").as_bytes(),
                "--json"
            )
            .is_err()
        );
    }
    for n in ["-1", "1e500", "1e-500", "\"0.1\"", "true", "NaN"] {
        assert!(
            s.raw(
                format!("{{\"daily\":[{{\"date\":\"day\",\"inputTokens\":1,\"totalCost\":{n}}}]}}")
                    .as_bytes(),
                "--json"
            )
            .is_err()
        );
    }
    for bad in [
        json!([]),
        json!({}),
        json!({"$serde_json::private::Number":"0.125"}),
    ] {
        let mut input = daily();
        input["daily"][0]["totalCost"] = bad;
        s.rejects(&input);
    }
    for (field, bad) in [
        ("inputTokens", json!(101)),
        ("totalTokens", json!(201)),
        ("totalCost", json!(0.13)),
    ] {
        let mut input = daily();
        input["totals"][field] = bad;
        s.rejects(&input);
    }
    let mut input = daily();
    input["daily"][0]["totalTokens"] = json!(201);
    s.rejects(&input);
    s.rejects(&json!({"daily":[{"date":"day","inputTokens":u64::MAX,"outputTokens":1}]}));
    s.rejects(
        &json!({"daily":[{"date":"one","inputTokens":u64::MAX},{"date":"two","inputTokens":1}]}),
    );
    s.rejects(&json!({"daily":[{"date":"one","inputTokens":1,"totalCost":1e308},{"date":"two","inputTokens":1,"totalCost":1e308}]}));
    assert_eq!(
        s.report(&json!({"daily":[{"date":"day","inputTokens":u64::MAX}]}))["rows"][0]["inputTokens"],
        u64::MAX
    );
}

#[test]
fn incomplete_rows_cannot_contradict_totals_and_rounding_is_allowed() {
    let s = Sandbox::new();
    s.rejects(&json!({"daily":[{"date":"one","inputTokens":5},{"date":"two","outputTokens":1}],"totals":{"inputTokens":4}}));
    let input = json!({"daily":[{"date":"one","inputTokens":1,"totalCost":0.1},{"date":"two","inputTokens":1,"totalCost":0.2}],"totals":{"inputTokens":2,"totalCost":0.3}});
    assert_eq!(s.report(&input)["totals"]["totalCost"], json!(0.3));
    s.rejects(&json!({"daily":[],"totals":{"inputTokens":1}}));
    s.rejects(&json!({"daily":[{"date":"one","inputTokens":1,"totalCost":1e-10}],"totals":{"totalCost":0}}));
}

#[test]
fn identifiers_are_escaped_in_text_and_csv_and_extra_payload_is_omitted() {
    let s = Sandbox::new();
    let input = json!({"sessions":[{"sessionId":"synthetic,\"quoted\"\n\u{1b}","inputTokens":1,"prompt":"SYNTHETIC_PROMPT_NOT_FOR_OUTPUT"}]});
    let text = s.raw(input.to_string().as_bytes(), "").unwrap();
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains("SYNTHETIC_PROMPT"));
    let csv = s.raw(input.to_string().as_bytes(), "--csv").unwrap();
    assert!(csv.contains("\"synthetic,\"\"quoted\"\"\n"));
    assert!(!s.report(&input).to_string().contains("SYNTHETIC_PROMPT"));
}

#[test]
fn cli_requires_explicit_file_and_unambiguous_output_option() {
    let _entrypoint: fn(&[OsString]) -> anyhow::Result<()> = usage::run;
    for args in [
        vec![],
        vec!["--import"],
        vec!["--import", "-"],
        vec!["--json", "--csv"],
        vec!["--json", "--json"],
        vec!["--import", "a", "--import", "b"],
        vec!["--unknown"],
        vec!["--help", "--import", "a"],
    ] {
        let mut out = Vec::new();
        assert!(
            usage::run_to(
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                &mut out
            )
            .is_err()
        );
        assert!(out.is_empty());
    }
    let mut out = Vec::new();
    usage::run_to(&["--help".into()], &mut out).unwrap();
    let help = String::from_utf8(out).unwrap();
    assert!(help.contains("--import FILE [--json|--csv]"));
    assert!(help.contains("Missing or null metrics are unavailable"));
    let s = Sandbox::new();
    assert!(
        usage::run_to(
            &["--import".into(), s.0.as_os_str().into()],
            &mut Vec::new()
        )
        .is_err()
    );
    assert!(
        usage::run_to(
            &["--import".into(), s.0.join("absent").into_os_string()],
            &mut Vec::new()
        )
        .is_err()
    );
}

#[test]
fn oversized_import_is_rejected() {
    let s = Sandbox::new();
    let path = s.0.join("oversized.json");
    fs::File::create(&path)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    let error =
        usage::run_to(&["--import".into(), path.into_os_string()], &mut Vec::new()).unwrap_err();
    assert!(error.to_string().contains("exceeds 32 MiB"));
}
