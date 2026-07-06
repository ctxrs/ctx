#[allow(unused_imports)]
use super::*;

#[derive(Debug, Args, Clone)]
pub(crate) struct DoctorArgs {
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub(crate) progress: ProgressArg,
}

pub(crate) fn run_doctor(
    args: DoctorArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let progress = ProgressReporter::new(args.progress, args.json, "doctor", 0);
    progress.message("opening", "opening ctx store");
    let db_path = database_path(data_root.clone());
    let mut findings = Vec::new();
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
    if !db_path.exists() {
        findings.push(format!(
            "ctx store is not initialized at {}; run `ctx setup` or `ctx import` first",
            db_path.display()
        ));
    } else {
        let store = open_existing_store_read_only(&db_path, "ctx doctor")?;
        progress.message(
            "checking",
            "running sqlite integrity and foreign key checks",
        );
        findings.extend(store.validate()?);
    }
    analytics::insert_count_bucket(
        analytics_properties,
        "finding_count_bucket",
        findings.len() as u64,
    );
    progress.done(
        "done",
        if findings.is_empty() {
            "ctx doctor passed"
        } else {
            "ctx doctor found issues"
        },
        0,
    );
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "ok": findings.is_empty(),
            "progress": progress_mode_name(args.progress),
            "findings": findings,
        }))?;
    } else if findings.is_empty() {
        println!("ok");
    } else {
        for finding in findings {
            println!("{finding}");
        }
    }
    Ok(())
}
