use super::*;

#[test]
fn code_mode_commit_reaches_blame_without_literal_command_facts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let repository = temp.path().join("repository");
    let (receipt, oid) = real_commit(&repository)?;
    let output = format!("{receipt}{oid}\n");
    let args = serde_json::json!({
        "cmd":"git commit -m 'primary witness' && git rev-parse HEAD", "workdir":repository
    });
    for (mode, script, body, expected) in [
        (
            "stdout",
            format!("const r = await tools.exec_command({args}); text(r.output);"),
            output.clone(),
            true,
        ),
        (
            "machine-oid",
            format!("const r = await tools.exec_command({args}); text(r.output);"),
            format!("{oid}\n"),
            true,
        ),
        (
            "envelope",
            format!("text(await tools.exec_command({args}));"),
            serde_json::json!({"exit_code":0,"output":output}).to_string(),
            true,
        ),
        (
            "conditional",
            format!("if (false) {{ text(await tools.exec_command({args})); }}"),
            output.clone(),
            false,
        ),
        (
            "multiple",
            format!(
                "text(await tools.exec_command({args})); text(await tools.exec_command({args}));"
            ),
            output.clone(),
            false,
        ),
        (
            "failed",
            format!("text(await tools.exec_command({args}));"),
            serde_json::json!({"exit_code":1,"output":output}).to_string(),
            false,
        ),
    ] {
        let mut records = primary_records(&repository, &output, "primary")?;
        for record in &mut records {
            record.parser_revision =
                "codex-nativepath-core-activity-v17-code-mode-output".to_owned();
            let activity = record.content.activity.as_mut().unwrap();
            activity
                .facts
                .retain(|fact| fact.kind == LiteralFactKind::File);
            activity.facts.push(ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: repository.to_string_lossy().into_owned(),
            });
            if let Some(invocation) = &mut activity.invocation {
                invocation.tool = "exec".to_owned();
                invocation.arguments = ActivityJsonCapture::Present {
                    value: serde_json::Value::String(script.clone()),
                };
            }
            if let Some(result) = &mut activity.result {
                result.text = ActivityTextCapture::Present {
                    value: body.clone(),
                };
                result.structured_content = ActivityJsonCapture::Present {
                    value: serde_json::json!([
                        {"type":"input_text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},
                        {"type":"input_text","text":body}
                    ]),
                };
            }
        }
        let root = temp.path().join(mode);
        materialize(&root, &records)?;
        assert_eq!(
            query(&root, &records, &oid)?,
            if expected {
                vec![AttributionOutcome::Possible]
            } else {
                vec![]
            },
            "{mode}"
        );
    }
    Ok(())
}
