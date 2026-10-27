use crate::{
    records::{copy, meta, path, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};

pub(super) fn result(
    r: &mut Value,
    spool: &Spool,
    id: &str,
    output: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    let call = spool.entity(&format!("call:{id}"))?.unwrap_or(Value::Null);
    if matches!(call["name"].as_str(), Some("Bash" | "PowerShell")) {
        let mut exec = json!({});
        for key in ["stdout", "stderr"] {
            copy(&mut exec, key, output, key);
        }
        copy(&mut exec, "command", &call["input"], "command");
        if let Some(p) = output["backgroundCwdHint"].as_str().or(cwd) {
            exec["cwd"] = path(p, cwd);
        }
        copy(&mut exec, "terminalId", output, "backgroundTaskId");
        r["exec"] = exec;
    }
    Ok(())
}
pub(super) fn audit(
    writer: &mut Writer,
    source: &Value,
    id: &str,
    output: &Value,
    cwd: Option<&str>,
) -> Result<()> {
    if let Some(diff) = output.get("bashEditDiff") {
        for file in diff["files"].as_array().into_iter().flatten() {
            let Some(name) = file["filePath"].as_str() else {
                continue;
            };
            let mut c = path(name, cwd);
            c["op"] = json!(if file["created"] == true {
                "add"
            } else if file["deleted"] == true {
                "delete"
            } else {
                "modify"
            });
            let mut r = json!({"type":"file_change","cause":{"callId":id},"status":"applied","precision":"unknown","changes":[c]});
            if let Some(flag) = diff["moreFiles"].as_bool() {
                r["moreFiles"] = json!(flag);
            } else if let Some(count) = diff["moreFiles"].as_u64() {
                r["moreFiles"] = json!(count > 0);
                meta(&mut r, Provider::Claude, "moreFilesCount", &json!(count));
            } else if let Some(native) = diff.get("moreFiles") {
                meta(&mut r, Provider::Claude, "moreFilesCount", native);
            }
            if let Some(patch) = hunks(&file["hunks"]) {
                r["precision"] = json!("hunks");
                r["patch"] = json!({"format":"unified","text":patch});
            }
            writer.emit(r, source)?;
        }
    }
    if let Some(git) = output.get("gitOperation") {
        for kind in ["commit", "push", "branch", "pr"] {
            let Some(detail) = git.get(kind) else {
                continue;
            };
            let action = match (
                kind,
                detail["kind"]
                    .as_str()
                    .or_else(|| detail["action"].as_str())
                    .unwrap_or(""),
            ) {
                ("commit", "amended") => "amend",
                ("commit", "cherry-picked") => "cherry_pick",
                ("commit", _) => "commit",
                ("push", _) => "push",
                ("branch", "merged") => "merge",
                ("branch", "rebased") => "rebase",
                ("pr", "created") => "pr_open",
                ("pr", "edited") => "pr_update",
                ("pr", "merged") => "pr_merge",
                ("pr", "commented") => "pr_comment",
                ("pr", "closed") => "pr_close",
                ("pr", "reopened") => "pr_reopen",
                ("pr", "ready") => "pr_ready",
                ("pr", "draft") => "pr_draft",
                ("pr", "auto-merge-enabled") => "_com.anthropic.claude-code/pr_auto_merge_enabled",
                ("pr", "auto-merge-disabled") => {
                    "_com.anthropic.claude-code/pr_auto_merge_disabled"
                }
                _ => continue,
            };
            let mut r = json!({"type":"vcs","action":action,"evidence":"parsed_output","cause":{"callId":id}});
            if let Some(sha) = detail["sha"].as_str() {
                r[if sha.len() == 40 || sha.len() == 64 {
                    "commit"
                } else {
                    "commitPrefix"
                }] = json!(sha);
            }
            copy(&mut r, "branch", detail, "branch");
            if kind == "branch" {
                copy(&mut r, "branch", detail, "ref");
            }
            if kind == "pr" {
                let mut pr = json!({});
                for key in ["number", "url", "repository"] {
                    copy(&mut pr, key, detail, key);
                }
                r["pr"] = pr;
            }
            writer.emit(r, source)?;
        }
    }
    Ok(())
}
pub(crate) fn hunks(value: &Value) -> Option<String> {
    let mut patch = String::new();
    for h in value.as_array()? {
        patch.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h["oldStart"].as_u64()?,
            h["oldLines"].as_u64()?,
            h["newStart"].as_u64()?,
            h["newLines"].as_u64()?
        ));
        for l in h["lines"].as_array()? {
            patch.push_str(l.as_str()?);
            patch.push('\n');
        }
    }
    (!patch.is_empty()).then_some(patch)
}
pub(super) fn checkpoint(writer: &mut Writer, v: &Value, cwd: Option<&str>) -> Result<()> {
    let snapshot = &v["snapshot"];
    let Some(id) = snapshot["messageId"]
        .as_str()
        .or_else(|| v["messageId"].as_str())
    else {
        return writer.vendor(v, "file-history-snapshot");
    };
    let mut files = Vec::new();
    for (name, details) in snapshot["trackedFileBackups"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let mut f = path(name, cwd);
        if let Some(version) = details.get("version") {
            f["version"] = json!(version
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| version.to_string()));
        }
        meta(&mut f, Provider::Claude, "backup", details);
        files.push(f);
    }
    writer.emit(
        json!({"type":"checkpoint","kind":"snapshot","ref":id,"files":files}),
        v,
    )?;
    Ok(())
}
