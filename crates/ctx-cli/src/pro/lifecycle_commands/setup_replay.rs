use super::*;

pub(super) fn reusable_setup_access_state(
    helper: &ProStatus,
    trial_only: bool,
    referral_codename: Option<&str>,
) -> Option<String> {
    if trial_only
        || referral_codename.is_some()
        || !helper.installed
        || !helper.ready
        || !helper.materialized
    {
        return None;
    }
    let account_state = helper.access_state.as_deref()?;
    validate_account_state(account_state).ok()?;
    Some(account_state.to_owned())
}

pub(super) fn write_setup_result(
    data_root: &Path,
    account_state: &str,
    helper_updated: bool,
    report: crate::pro::client::MaterializeReport,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    let value = json!({
        "schema_version": 1,
        "payload_type": "pro_setup",
        "ok": true,
        "account_state": account_state,
        "helper_updated": helper_updated,
        "graph": report,
        "status": lifecycle_status_json(data_root),
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let document = render_setup_human(ui.stdout_context(), account_state);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

pub(super) fn record_helper_smoke(
    smoke: Result<crate::pro::client::HelperSmoke>,
    telemetry: &mut ProLifecycleTelemetryV1,
) -> Result<crate::pro::client::HelperSmoke> {
    match smoke {
        Ok(smoke) => {
            telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
            Ok(smoke)
        }
        Err(error) => {
            telemetry.helper_connection =
                crate::analytics::pro_helper_connection_outcome(stable_error_code(&error));
            Err(error)
        }
    }
}
