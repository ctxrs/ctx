use super::*;

pub(super) fn setup(args: &SetupArgs, remove: bool) -> Result<SetupReport> {
    let action = if remove { "remove" } else { "install" };
    if args.mcp || args.skill || !args.tool_hooks {
        let target = if args.mcp { "mcp" } else { "skill" };
        bail!(
            "ctx owns managed skills and the default MCP server; run ctx integrations {action} {target} --help"
        );
    }
    ensure!(
        !args.global && args.config_root.is_none() && args.profile.is_none(),
        "graph tool hooks require a project; global/config-root/profile selections belong to ctx integrations"
    );
    let host = args.platform.as_str();
    ensure!(
        matches!(host, "claude" | "codebuddy" | "gemini"),
        "graph tool hooks support claude, codebuddy, and gemini projects"
    );
    let scope = root(args.project.as_deref())?;
    let destination = scope.join(format!(".{host}/settings.json"));
    let path = receipt_path(&scope, host, "tool-hooks");
    let mut report = SetupReport {
        status: "not-installed".into(),
        platform: host.into(),
        scope: scope.clone(),
        files: vec![],
        notes: vec![
            "Tool hooks add optional graph context and never deny source access. The host must have ctx on PATH.".into(),
        ],
    };
    if remove && read(&path)?.is_none() {
        return Ok(report);
    }
    let _lock = lock(&scope, !remove)?;
    let receipt = match load(&path, &scope, std::slice::from_ref(&destination))? {
        Some(receipt) => receipt,
        None => {
            let old = read(&destination)?;
            let after = tool_hook_bytes(old.as_deref().unwrap_or(b"{}\n"), host, &scope)?;
            Receipt {
                version: 1,
                scope,
                changes: vec![edited(destination, old, after)?],
                guidance_version: None,
            }
        }
    };
    report.files = receipt.changes.iter().map(|c| c.path.clone()).collect();
    report.status = if remove {
        undo(&path, &receipt)?;
        "uninstalled"
    } else if apply(&path, &receipt)? {
        "installed"
    } else {
        "unchanged"
    }
    .into();
    Ok(report)
}

fn object_fields(bytes: &[u8], host: &str) -> Result<(BTreeMap<String, Range<usize>>, usize)> {
    use jsonc_parser::{CollectOptions, ParseOptions, common::Ranged};
    // VS Code's schema accepts JSONC; Gemini strips comments before JSON.parse.
    // Other hosts retain strict JSON until their contract establishes extensions.
    let parsed = jsonc_parser::parse_to_ast(
        std::str::from_utf8(bytes)?,
        &CollectOptions::default(),
        &ParseOptions {
            allow_comments: matches!(host, "vscode" | "gemini"),
            allow_trailing_commas: host == "vscode",
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
        },
    )
    .context("invalid MCP configuration for this host")?;
    let object = parsed
        .value
        .as_ref()
        .and_then(|v| v.as_object())
        .context("MCP configuration section must be an object")?;
    let mut fields = BTreeMap::new();
    for property in &object.properties {
        let range = property.value.range();
        ensure!(
            fields
                .insert(property.name.as_str().to_owned(), range.start..range.end)
                .is_none(),
            "duplicate JSON key; configuration unchanged"
        );
    }
    // Insert the new first member, before all existing content, avoiding any
    // need to move comments or interpret an existing trailing comma ourselves.
    Ok((fields, object.range.start + 1))
}

fn tool_hook_bytes(old: &[u8], host: &str, scope: &Path) -> Result<Vec<u8>> {
    // One fixed executable on PATH; quote only the pinned project argument.
    let project = if cfg!(windows) {
        let value = scope.to_str().context("project path must be UTF-8")?;
        ensure!(
            !value.contains(['"', '%', '!', '$', '`', '\n', '\r', '\0']),
            "project path cannot be quoted safely for this host shell"
        );
        format!("\"{value}\"")
    } else {
        quote(scope)?
    };
    let command = format!("ctx graph hook-guard --platform {host} --project {project}");
    let (event, matcher, timeout) = if host == "gemini" {
        ("BeforeTool", "read_file|list_directory", 10_000)
    } else {
        ("PreToolUse", "Read|Glob|Grep|Bash", 10)
    };
    let entry =
        json!({"matcher":matcher,"hooks":[{"type":"command","command":command,"timeout":timeout}]});
    let (fields, root_start) = object_fields(old, host)?;
    let (position, comma, member) = if let Some(range) = fields.get("hooks") {
        let (events, start) = object_fields(&old[range.clone()], host)?;
        if let Some(array) = events.get(event) {
            let offset = range.start + array.start;
            let bytes = &old[offset..range.start + array.end];
            // The complete configuration was already parsed with host-specific
            // strictness; inspect this array without rewriting its existing bytes.
            let parsed = jsonc_parser::parse_to_ast(
                std::str::from_utf8(bytes)?,
                &Default::default(),
                &Default::default(),
            )?;
            let array = parsed
                .value
                .as_ref()
                .and_then(|v| v.as_array())
                .context("tool hook event must be an array")?;
            ensure!(
                !String::from_utf8_lossy(bytes).contains("ctx graph hook-guard"),
                "unowned ctx graph tool hook already exists"
            );
            (
                offset + array.range.start + 1,
                !array.elements.is_empty(),
                entry.to_string(),
            )
        } else {
            (
                range.start + start,
                !events.is_empty(),
                format!("{}: [{entry}]", serde_json::to_string(event)?),
            )
        }
    } else {
        (
            root_start,
            !fields.is_empty(),
            format!(
                "\"hooks\": {{{}: [{entry}]}}",
                serde_json::to_string(event)?
            ),
        )
    };
    let mut out = old[..position].to_vec();
    out.extend_from_slice(format!("\n  {member}{}\n", if comma { "," } else { "" }).as_bytes());
    out.extend_from_slice(&old[position..]);
    Ok(out)
}
