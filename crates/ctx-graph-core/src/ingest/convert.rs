use super::CommandAdapter;
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use std::{
    io::{Cursor, Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

impl CommandAdapter {
    /// `tesseract INPUT OUTPUT_STEM`; requires Tesseract on PATH.
    pub fn tesseract() -> Self {
        Self {
            program: "tesseract".into(),
            args: vec!["{input}".into(), "{output_stem}".into()],
            output_file: true,
        }
    }
    /// OpenAI's open-source Whisper CLI accepts audio/video through FFmpeg.
    /// The user installs Whisper/FFmpeg and its model separately; Graf never installs them.
    pub fn whisper(model: &str) -> Self {
        Self {
            program: "whisper".into(),
            args: vec![
                "{input}".into(),
                "--model".into(),
                model.into(),
                "--output_format".into(),
                "txt".into(),
                "--output_dir".into(),
                "{output_dir}".into(),
            ],
            output_file: true,
        }
    }
    /// Whisper initial context is an explicit argument and participates in cache keys.
    pub fn whisper_with_prompt(model: &str, prompt: &str) -> Self {
        let mut adapter = Self::whisper(model);
        adapter
            .args
            .extend(["--initial_prompt".into(), prompt.into()]);
        adapter
    }
    /// Poppler text extraction; useful as an explicit override for native PDFs.
    pub fn pdftotext() -> Self {
        Self {
            program: "pdftotext".into(),
            args: vec!["-layout".into(), "{input}".into(), "-".into()],
            output_file: false,
        }
    }
    /// Pandoc DOCX/RST/HTML to Markdown. Native DOCX needs no executable.
    pub fn pandoc() -> Self {
        Self {
            program: "pandoc".into(),
            args: vec!["{input}".into(), "--to".into(), "markdown".into()],
            output_file: false,
        }
    }
    /// Explicit Google Workspace CLI export. Docs/slides: plain text;
    /// sheets: XLSX, read by the native workbook parser (all worksheets).
    pub fn google_workspace() -> Self {
        Self {
            program: "gws".into(),
            args: vec![
                "drive".into(),
                "files".into(),
                "export".into(),
                "--params".into(),
                "{google_params}".into(),
                "-o".into(),
                "{output}".into(),
            ],
            output_file: true,
        }
    }

    /// Explicit remote media download, used only by `extract_url` when installed
    /// under converters["url"]. Pair with a local transcription recipe.
    pub fn yt_dlp() -> Self {
        Self {
            program: "yt-dlp".into(),
            args: vec![
                "--no-playlist".into(),
                "--no-progress".into(),
                "--max-filesize".into(),
                "32M".into(),
                "-f".into(),
                "bestaudio".into(),
                "--output".into(),
                "{output}".into(),
                "--".into(),
                "{url}".into(),
            ],
            output_file: true,
        }
    }

    /// AWS CLI v2 Converse; credentials/profile stay in the CLI environment.
    pub fn bedrock() -> Self {
        Self {
            program: "aws".into(),
            args: vec![
                "bedrock-runtime".into(),
                "converse".into(),
                "--cli-input-json".into(),
                "file://{request_file}".into(),
                "--cli-binary-format".into(),
                "base64".into(),
                "--output".into(),
                "json".into(),
                "--no-cli-pager".into(),
            ],
            output_file: false,
        }
    }

    /// Claude Code print-only inference, with tool use and session persistence disabled.
    /// One text generation; native vision explicitly raises the turn limit and
    /// reserves every permitted turn before adding its per-file Read allowlist.
    /// Arbitrary custom executables remain responsible for honoring requested
    /// limits: Graf cannot bound model work hidden inside a user adapter.
    pub fn claude_cli() -> Self {
        Self {
            program: if cfg!(windows) {
                "claude.cmd"
            } else {
                "claude"
            }
            .into(),
            args: vec![
                "--print".into(),
                "--model".into(),
                "{model}".into(),
                "--output-format".into(),
                "json".into(),
                "--tools".into(),
                String::new(),
                "--no-session-persistence".into(),
                "--max-turns".into(),
                "1".into(),
                "--setting-sources".into(),
                String::new(),
                "--strict-mcp-config".into(),
                "--mcp-config".into(),
                "{\"mcpServers\":{}}".into(),
            ],
            output_file: false,
        }
    }
}

/// TempDir defaults to 0777 masked by umask on Unix. Provider inputs need an
/// owner-only directory from creation, including before request files exist.
pub(super) fn private_tempdir() -> Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("graf-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    Ok(builder.tempdir()?)
}

/// No shell, bounded wall-clock and output, no inherited terminal/stdin. Temp
/// files avoid pipe deadlocks (including descendants keeping stdout open).
pub(crate) fn run(
    adapter: &CommandAdapter,
    input: Option<&Path>,
    stdin: Option<&[u8]>,
    timeout: u64,
    limit: usize,
) -> Result<String> {
    String::from_utf8(run_bytes(adapter, input, stdin, timeout, limit)?)
        .context("converter/provider output is not UTF-8")
}

pub(super) fn run_bytes(
    adapter: &CommandAdapter,
    input: Option<&Path>,
    stdin: Option<&[u8]>,
    timeout: u64,
    limit: usize,
) -> Result<Vec<u8>> {
    run_bytes_env(adapter, input, stdin, timeout, limit, &[])
}

pub(super) fn run_bytes_env(
    adapter: &CommandAdapter,
    input: Option<&Path>,
    stdin: Option<&[u8]>,
    timeout: u64,
    limit: usize,
    env: &[(&str, String)],
) -> Result<Vec<u8>> {
    run_bytes_until(
        adapter,
        input,
        stdin,
        Instant::now() + Duration::from_secs(timeout),
        limit,
        env,
    )
}

pub(super) fn run_bytes_until(
    adapter: &CommandAdapter,
    input: Option<&Path>,
    stdin: Option<&[u8]>,
    deadline: Instant,
    limit: usize,
    env: &[(&str, String)],
) -> Result<Vec<u8>> {
    let (bytes, success) = run_provider_until(adapter, input, stdin, deadline, limit, env)?;
    ensure!(
        success,
        "converter/provider exited unsuccessfully (output omitted to protect credentials)"
    );
    Ok(bytes)
}

#[derive(Debug)]
pub(super) struct CommandTimeout;
impl std::fmt::Display for CommandTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("converter/provider timed out")
    }
}
impl std::error::Error for CommandTimeout {}

/// Native provider protocols can report typed errors and usage on nonzero exit.
/// Never expose stdout/stderr in an error; ordinary converters still reject it.
pub(super) fn run_provider_until(
    adapter: &CommandAdapter,
    input: Option<&Path>,
    stdin: Option<&[u8]>,
    deadline: Instant,
    limit: usize,
    env: &[(&str, String)],
) -> Result<(Vec<u8>, bool)> {
    ensure!(
        !adapter.program.is_empty(),
        "converter executable must be configured"
    );
    let temp = private_tempdir()?;
    let input = input.map(Path::canonicalize).transpose()?;
    let stem = input
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("response");
    let output = temp.path().join(format!("{stem}.txt"));
    let output_stem = output.with_extension("");
    let request_file = temp.path().join("request.json");
    if let Some(bytes) = stdin {
        std::fs::write(&request_file, bytes)?;
    }
    let mut command = Command::new(&adapter.program);
    command.current_dir(temp.path());
    for (name, value) in env {
        command.env(name, value);
    }
    for arg in &adapter.args {
        command.arg(
            arg.replace(
                "{input}",
                input.as_ref().and_then(|p| p.to_str()).unwrap_or(""),
            )
            .replace("{output}", &output.to_string_lossy())
            .replace("{output_stem}", &output_stem.to_string_lossy())
            .replace("{output_dir}", &temp.path().to_string_lossy())
            .replace("{request_file}", &request_file.to_string_lossy()),
        );
    }
    let mut in_file = tempfile::tempfile()?;
    if let Some(bytes) = stdin {
        in_file.write_all(bytes)?;
        std::io::Seek::rewind(&mut in_file)?;
    }
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::from(in_file))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if Instant::now() >= deadline {
        return Err(CommandTimeout.into());
    }
    let mut child = command
        .spawn()
        .context("cannot start configured converter/provider executable")?;
    let result = (|| -> Result<bool> {
        loop {
            if Instant::now() >= deadline {
                return Err(CommandTimeout.into());
            }
            ensure!(
                stdout.metadata()?.len() <= limit as u64
                    && stderr.metadata()?.len() <= limit as u64,
                "converter/provider output exceeds byte limit"
            );
            if let Ok(metadata) = std::fs::metadata(&output) {
                ensure!(
                    metadata.len() <= limit as u64,
                    "converted file exceeds byte limit"
                );
            }
            #[cfg(test)]
            tests::after_output_sample(&mut child);
            if let Some(status) = child.try_wait()? {
                break Ok(status.success());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    // Terminate the process group even after successful parent exit; detached
    // descendants cannot continue writing the result while it is consumed.
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let success = result?;
    // The child can write after the last polling sample and exit before
    // try_wait. Recheck both captures even when a declared file is the result.
    ensure!(
        stdout.metadata()?.len() <= limit as u64 && stderr.metadata()?.len() <= limit as u64,
        "converter/provider output exceeds byte limit"
    );
    if Instant::now() >= deadline {
        return Err(CommandTimeout.into());
    }
    if !success && adapter.output_file {
        return Ok((Vec::new(), false));
    }
    let mut bytes = Vec::new();
    if adapter.output_file {
        bytes = super::read_bounded(&output, limit as u64)
            .context("converter did not produce its declared output file")?;
    } else {
        let mut stdout = stdout;
        std::io::Seek::rewind(&mut stdout)?;
        stdout.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    }
    ensure!(
        bytes.len() <= limit,
        "converter/provider output exceeds byte limit"
    );
    Ok((bytes, success))
}

fn xml_text(bytes: &[u8], limit: usize) -> Result<String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut text = String::new();
    let mut depth = 0usize;
    loop {
        match reader.read_event()? {
            Event::Start(_) => {
                depth += 1;
                ensure!(depth <= 128, "Office XML nesting exceeds limit");
            }
            Event::End(tag) => {
                depth = depth.saturating_sub(1);
                if matches!(tag.local_name().as_ref(), b"p" | b"row" | b"si") {
                    text.push('\n');
                } else if matches!(tag.local_name().as_ref(), b"tc" | b"c") {
                    text.push('\t');
                }
            }
            Event::Text(value) => text.push_str(&value.decode()?),
            Event::GeneralRef(value) => text.push_str(&quick_xml::escape::unescape(&format!(
                "&{};",
                value.decode()?
            ))?),
            Event::CData(value) => text.push_str(&value.decode()?),
            Event::Empty(e) if e.local_name().as_ref() == b"tab" => text.push('\t'),
            Event::Empty(e) if matches!(e.local_name().as_ref(), b"br" | b"cr") => text.push('\n'),
            Event::DocType(_) => bail!("Office XML DTDs are prohibited"),
            Event::Eof => {
                ensure!(depth == 0, "incomplete Office XML");
                break;
            }
            _ => {}
        }
        ensure!(text.len() <= limit, "Office text exceeds byte limit");
    }
    Ok(text)
}

pub(super) fn xml_values(bytes: &[u8], tag: &[u8]) -> Result<Vec<String>> {
    let mut reader = Reader::from_reader(bytes);
    let mut values = vec![];
    let mut depth = 0usize;
    let mut capture = None;
    let mut text = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                depth += 1;
                ensure!(depth <= 128, "XML nesting exceeds limit");
                if e.local_name().as_ref() == tag {
                    capture = Some(depth);
                    text.clear();
                }
            }
            Event::End(_) => {
                if capture == Some(depth) {
                    values.push(std::mem::take(&mut text));
                    capture = None;
                }
                depth = depth.saturating_sub(1);
            }
            Event::Text(e) if capture.is_some() => text.push_str(&e.decode()?),
            Event::GeneralRef(e) if capture.is_some() => {
                text.push_str(&quick_xml::escape::unescape(&format!("&{};", e.decode()?))?)
            }
            Event::CData(e) if capture.is_some() => text.push_str(&e.decode()?),
            Event::DocType(_) => bail!("XML DTDs are prohibited"),
            Event::Eof => {
                ensure!(depth == 0, "incomplete XML");
                break;
            }
            _ => {}
        }
    }
    Ok(values)
}

pub(super) struct OfficeContent {
    pub text: String,
    pub elements: Vec<(String, &'static str, Option<usize>)>,
}

pub(super) fn office(bytes: &[u8], ext: &str, limit: usize) -> Result<OfficeContent> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("invalid Office ZIP archive")?;
    ensure!(archive.len() <= 4096, "Office archive entry limit exceeded");
    let mut expanded = 0u64;
    let mut members = std::collections::BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        // Never extract an archive member to the filesystem.
        let name = file.name().to_owned();
        ensure!(
            file.enclosed_name().is_some(),
            "unsafe Office archive member path"
        );
        expanded = expanded
            .checked_add(file.size())
            .context("Office expanded size overflow")?;
        ensure!(
            expanded <= 64 * 1024 * 1024,
            "Office archive expanded byte limit exceeded"
        );
        if name == "word/document.xml"
            || name.starts_with("word/header")
            || name.starts_with("word/footer")
            || name == "word/styles.xml"
            || name == "word/numbering.xml"
            || name == "xl/sharedStrings.xml"
            || name == "xl/workbook.xml"
            || (name.starts_with("xl/") && name.ends_with(".rels"))
            || (name.starts_with("xl/tables/") && name.ends_with(".xml"))
            || (name.starts_with("xl/worksheets/") && name.ends_with(".xml"))
        {
            let mut data = Vec::new();
            file.by_ref()
                .take(64 * 1024 * 1024 + 1)
                .read_to_end(&mut data)?;
            ensure!(
                data.len() as u64 == file.size(),
                "Office archive size mismatch"
            );
            ensure!(
                members.insert(name, data).is_none(),
                "duplicate Office archive member"
            );
        }
    }
    let mut out = String::new();
    let mut elements = vec![];
    if ext == "docx" {
        ensure!(
            members.contains_key("word/document.xml"),
            "DOCX has no document.xml"
        );
        for (name, data) in &members {
            if name == "word/document.xml"
                || name.starts_with("word/header")
                || name.starts_with("word/footer")
            {
                out.push_str(&docx_markdown(
                    data,
                    members.get("word/styles.xml"),
                    members.get("word/numbering.xml"),
                    limit,
                )?);
                out.push('\n');
            }
        }
    } else {
        let strings = members
            .get("xl/sharedStrings.xml")
            .map(|b| xml_values(b, b"si"))
            .transpose()?
            .unwrap_or_default();
        ensure!(
            members.keys().any(|n| n.starts_with("xl/worksheets/")),
            "XLSX has no worksheets"
        );
        let mut sheet_names = std::collections::BTreeMap::new();
        if let (Some(workbook), Some(rels)) = (
            members.get("xl/workbook.xml"),
            members.get("xl/_rels/workbook.xml.rels"),
        ) {
            let relationships = attributes(rels, b"Relationship")?;
            for sheet in attributes(workbook, b"sheet")? {
                if let (Some(id), Some(label)) = (sheet.get("id"), sheet.get("name"))
                    && let Some(target) = relationships
                        .iter()
                        .find(|r| r.get("Id") == Some(id))
                        .and_then(|r| r.get("Target"))
                {
                    sheet_names.insert(member_target("xl/workbook.xml", target)?, label.clone());
                }
            }
        }
        for (name, data) in &members {
            if !name.starts_with("xl/worksheets/") || !name.ends_with(".xml") {
                continue;
            }
            let label = sheet_names.get(name).unwrap_or(name);
            let sheet_index = elements.len();
            elements.push((label.clone(), "sheet", None));
            let rel_name = format!(
                "xl/worksheets/_rels/{}.rels",
                name.rsplit('/').next().unwrap()
            );
            if let Some(rels) = members.get(&rel_name) {
                for relation in attributes(rels, b"Relationship")? {
                    if relation.get("TargetMode").is_some_and(|v| v == "External") {
                        continue;
                    }
                    if let Some(target) = relation.get("Target") {
                        let target = member_target(name, target)?;
                        if let Some(table) = members
                            .get(&target)
                            .filter(|_| target.starts_with("xl/tables/"))
                        {
                            for attrs in attributes(table, b"table")? {
                                if let Some(name) =
                                    attrs.get("displayName").or_else(|| attrs.get("name"))
                                {
                                    let table_index = elements.len();
                                    elements.push((name.clone(), "table", Some(sheet_index)));
                                    for column in attributes(table, b"tableColumn")? {
                                        if let Some(name) = column.get("name") {
                                            elements.push((
                                                name.clone(),
                                                "column",
                                                Some(table_index),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ensure!(
                elements.len() <= 20_000,
                "Office structure fact limit exceeded"
            );
            out.push_str(&format!("\nSheet: {label}\n"));
            let mut first_row = true;
            let mut reader = Reader::from_reader(data.as_slice());
            let mut shared = false;
            let mut cell = false;
            let mut value = String::new();
            let mut depth = 0usize;
            loop {
                match reader.read_event()? {
                    Event::Start(e) => {
                        depth += 1;
                        ensure!(depth <= 128, "XLSX nesting exceeds limit");
                        if e.local_name().as_ref() == b"c" {
                            cell = true;
                            value.clear();
                            shared = false;
                            for attr in e.attributes() {
                                let attr = attr?;
                                if attr.key.as_ref() == b"t" {
                                    shared = attr.value.as_ref() == b"s";
                                }
                            }
                        }
                        // Cell formulas are never executed. Only cached values/inline strings are read.
                        if cell && matches!(e.local_name().as_ref(), b"v" | b"t") {
                            value.push_str(&reader.read_text(e.name())?.decode()?);
                            depth -= 1;
                        }
                    }
                    Event::End(e) => {
                        depth = depth.saturating_sub(1);
                        if e.local_name().as_ref() == b"c" {
                            let resolved = if shared {
                                let index: usize =
                                    value.parse().context("invalid XLSX shared string index")?;
                                strings
                                    .get(index)
                                    .context("missing XLSX shared string")?
                                    .clone()
                            } else {
                                quick_xml::escape::unescape(&value)?.into_owned()
                            };
                            out.push_str(&resolved);
                            if first_row && !resolved.is_empty() {
                                elements.push((resolved, "column", Some(sheet_index)));
                            }
                            ensure!(
                                elements.len() <= 20_000,
                                "Office structure fact limit exceeded"
                            );
                            out.push('\t');
                            cell = false;
                        } else if e.local_name().as_ref() == b"row" {
                            first_row = false;
                            out.push('\n');
                        }
                    }
                    Event::DocType(_) => bail!("XLSX DTDs are prohibited"),
                    Event::Eof => {
                        ensure!(depth == 0, "incomplete XLSX XML");
                        break;
                    }
                    _ => {}
                }
                ensure!(out.len() <= limit, "Office text exceeds byte limit");
            }
        }
    }
    ensure!(out.len() <= limit, "Office text exceeds byte limit");
    Ok(OfficeContent {
        text: out,
        elements,
    })
}

fn member_target(source: &str, target: &str) -> Result<String> {
    if target.starts_with('/') {
        return Ok(target.trim_start_matches('/').into());
    }
    let mut parts: Vec<_> = source.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                ensure!(parts.pop().is_some(), "Office relationship escapes archive");
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn attributes(bytes: &[u8], tag: &[u8]) -> Result<Vec<std::collections::BTreeMap<String, String>>> {
    let mut reader = Reader::from_reader(bytes);
    let mut result = vec![];
    let mut depth = 0usize;
    loop {
        let event = reader.read_event()?;
        if matches!(event, Event::Start(_)) {
            depth += 1;
            ensure!(depth <= 128, "Office XML nesting exceeds limit");
        }
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == tag => {
                let mut map = std::collections::BTreeMap::new();
                for attr in e.attributes() {
                    let attr = attr?;
                    map.insert(
                        String::from_utf8(attr.key.local_name().as_ref().to_vec())?,
                        attr.decoded_and_normalized_value(
                            quick_xml::XmlVersion::Implicit1_0,
                            reader.decoder(),
                        )?
                        .into_owned(),
                    );
                }
                result.push(map);
                ensure!(result.len() <= 20_000, "Office structure limit exceeded");
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::DocType(_) => bail!("Office XML DTDs are prohibited"),
            Event::Eof => {
                ensure!(depth == 0, "incomplete Office XML");
                break;
            }
            _ => {}
        }
    }
    Ok(result)
}

#[derive(Default, Clone)]
struct WordStyle {
    parent: Option<String>,
    heading: Option<u32>,
    number: Option<String>,
    bullet: bool,
}

fn property(xml: &[u8], tag: &[u8]) -> Result<Option<String>> {
    Ok(attributes(xml, tag)?
        .into_iter()
        .next()
        .and_then(|m| m.get("val").cloned()))
}

fn heading_style(name: &str) -> Option<u32> {
    let name = name.to_ascii_lowercase().replace(' ', "");
    if name == "title" {
        return Some(1);
    }
    name.strip_prefix("heading")
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|n| (1..=6).contains(n))
}

// Small OOXML paragraph-style reader. XML/ZIP boundaries remain shared with
// native Office extraction; no Office installation or macros are involved.
fn docx_markdown(
    document: &[u8],
    styles: Option<&Vec<u8>>,
    numbering: Option<&Vec<u8>>,
    limit: usize,
) -> Result<String> {
    xml_text(document, limit)?; // Validate complete document/DTD/depth before reading subtrees.
    let mut style_map = std::collections::BTreeMap::new();
    if let Some(styles) = styles {
        for (attrs, xml) in elements(styles, b"style")? {
            if let Some(id) = attrs.get("styleId") {
                let name = property(xml.as_bytes(), b"name")?.unwrap_or_else(|| id.clone());
                let heading = property(xml.as_bytes(), b"outlineLvl")?
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|v| *v < 6)
                    .map(|v| v + 1)
                    .or_else(|| heading_style(&name));
                style_map.insert(
                    id.clone(),
                    WordStyle {
                        parent: property(xml.as_bytes(), b"basedOn")?,
                        heading,
                        number: property(xml.as_bytes(), b"numId")?,
                        bullet: name.to_ascii_lowercase().contains("bullet"),
                    },
                );
            }
        }
    }
    let mut formats = std::collections::BTreeMap::new();
    let mut nums = std::collections::BTreeMap::new();
    if let Some(numbering) = numbering {
        for (attrs, xml) in elements(numbering, b"abstractNum")? {
            if let Some(id) = attrs.get("abstractNumId") {
                for (attrs, level) in elements(xml.as_bytes(), b"lvl")? {
                    let level_id = attrs.get("ilvl").cloned().unwrap_or_else(|| "0".into());
                    let fmt =
                        property(level.as_bytes(), b"numFmt")?.unwrap_or_else(|| "decimal".into());
                    let start = property(level.as_bytes(), b"start")?
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(1);
                    formats.insert((id.clone(), level_id), (fmt != "bullet", start));
                }
            }
        }
        for (attrs, xml) in elements(numbering, b"num")? {
            if let (Some(id), Some(abstract_id)) = (
                attrs.get("numId"),
                property(xml.as_bytes(), b"abstractNumId")?,
            ) {
                nums.insert(id.clone(), abstract_id);
            }
        }
    }
    let mut out = String::new();
    let mut counters = std::collections::BTreeMap::<(String, u32), u32>::new();
    let mut reader = Reader::from_reader(document);
    loop {
        let (table, xml) = match reader.read_event()? {
            Event::Start(e) if matches!(e.local_name().as_ref(), b"p" | b"tbl") => (
                e.local_name().as_ref() == b"tbl",
                reader.read_text(e.name())?.decode()?.into_owned(),
            ),
            Event::Eof => break,
            _ => continue,
        };
        if table {
            out.push_str(&docx_table(xml.as_bytes(), limit)?);
            ensure!(out.len() <= limit, "DOCX text exceeds byte limit");
            continue;
        }
        let raw = xml.as_bytes();
        let text = xml_text(raw, limit)?;
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let style = property(raw, b"pStyle")?;
        let mut resolved = WordStyle::default();
        let mut current = style.clone();
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = current {
            ensure!(
                seen.len() < 64 && seen.insert(id.clone()),
                "cyclic/deep DOCX paragraph style"
            );
            let Some(style) = style_map.get(&id) else {
                if resolved.heading.is_none() {
                    resolved.heading = heading_style(&id);
                }
                resolved.bullet |= id.to_ascii_lowercase().contains("bullet");
                break;
            };
            if resolved.heading.is_none() {
                resolved.heading = style.heading;
            }
            if resolved.number.is_none() {
                resolved.number = style.number.clone();
            }
            resolved.bullet |= style.bullet;
            current = style.parent.clone();
        }
        let heading = property(raw, b"outlineLvl")?
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|v| *v < 6)
            .map(|v| v + 1)
            .or(resolved.heading);
        let number = property(raw, b"numId")?
            .or(resolved.number)
            .filter(|n| n != "0");
        let level = property(raw, b"ilvl")?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0)
            .min(8);
        if let Some(level) = heading {
            out.push_str(&"#".repeat(level as usize));
            out.push(' ');
        } else if let Some(number) = number {
            let (ordered, start) = nums
                .get(&number)
                .and_then(|id| formats.get(&(id.clone(), level.to_string())))
                .copied()
                .unwrap_or((!resolved.bullet, 1));
            out.push_str(&"  ".repeat(level as usize));
            if ordered {
                counters.retain(|(id, l), _| id != &number || *l <= level);
                let count = counters.entry((number, level)).or_insert(start);
                out.push_str(&format!("{count}. "));
                *count = count.saturating_add(1);
            } else {
                out.push_str("- ");
            }
        } else if resolved.bullet
            || style
                .as_ref()
                .is_some_and(|s| s.to_ascii_lowercase().contains("list"))
        {
            out.push_str("- ");
        }
        out.push_str(text);
        out.push_str("\n\n");
        ensure!(out.len() <= limit, "DOCX text exceeds byte limit");
    }
    Ok(out)
}

/// Direct children only, so a nested table cannot contribute rows/cells twice.
fn word_children(xml: &[u8], tag: &[u8]) -> Result<Vec<String>> {
    let mut reader = Reader::from_reader(xml);
    let mut depth = 0usize;
    let mut children = vec![];
    loop {
        match reader.read_event()? {
            Event::Start(e) if depth == 0 && e.local_name().as_ref() == tag => {
                children.push(reader.read_text(e.name())?.decode()?.into_owned());
                ensure!(
                    children.len() <= 20_000,
                    "DOCX table structure exceeds limit"
                );
            }
            Event::Empty(e) if depth == 0 && e.local_name().as_ref() == tag => {
                children.push(String::new());
                ensure!(
                    children.len() <= 20_000,
                    "DOCX table structure exceeds limit"
                );
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(children)
}

fn docx_table(xml: &[u8], limit: usize) -> Result<String> {
    let mut rows = vec![];
    let mut header = false;
    let mut size = 0usize;
    for row in word_children(xml, b"tr")? {
        let properties = word_children(row.as_bytes(), b"trPr")?
            .into_iter()
            .next()
            .unwrap_or_default();
        if rows.is_empty() {
            header = attributes(properties.as_bytes(), b"tblHeader")?
                .first()
                .is_some_and(|p| {
                    p.get("val")
                        .is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off"))
                });
        }
        let before = property(properties.as_bytes(), b"gridBefore")?
            .unwrap_or_else(|| "0".into())
            .parse::<usize>()?;
        ensure!(before <= 256, "DOCX table column limit exceeded");
        let mut cells = vec![String::new(); before];
        for cell in word_children(row.as_bytes(), b"tc")? {
            let properties = word_children(cell.as_bytes(), b"tcPr")?
                .into_iter()
                .next()
                .unwrap_or_default();
            let span = property(properties.as_bytes(), b"gridSpan")?
                .unwrap_or_else(|| "1".into())
                .parse::<usize>()?;
            ensure!(
                span > 0 && cells.len() + span <= 256,
                "DOCX table column limit exceeded"
            );
            let text = xml_text(cell.as_bytes(), limit)?;
            let text = text
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                        .replace('\\', "\\\\")
                        .replace('|', "\\|")
                })
                .collect::<Vec<_>>()
                .join("<br>");
            size = size
                .checked_add(text.len())
                .context("DOCX table size overflow")?;
            ensure!(size <= limit, "DOCX table exceeds text byte limit");
            cells.push(text);
            cells.extend((1..span).map(|_| String::new()));
        }
        rows.push(cells);
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Ok(String::new());
    }
    let mut out = String::new();
    // A synthetic empty header preserves data when Word has no marked header.
    if !header {
        rows.insert(0, vec![]);
    }
    for (index, mut row) in rows.into_iter().enumerate() {
        row.resize(columns, String::new());
        out.push_str(&format!("| {} |\n", row.join(" | ")));
        if index == 0 {
            out.push_str(&format!("| {} |\n", vec!["---"; columns].join(" | ")));
        }
        ensure!(out.len() <= limit, "DOCX table exceeds text byte limit");
    }
    out.push('\n');
    Ok(out)
}

fn elements(
    bytes: &[u8],
    tag: &[u8],
) -> Result<Vec<(std::collections::BTreeMap<String, String>, String)>> {
    // The shared XML pass checks entities, nesting, DTDs and complete closing tags.
    xml_text(bytes, 64 * 1024 * 1024)?;
    let mut reader = Reader::from_reader(bytes);
    let mut out = vec![];
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.local_name().as_ref() == tag => {
                let mut attrs = std::collections::BTreeMap::new();
                for attr in e.attributes() {
                    let attr = attr?;
                    attrs.insert(
                        String::from_utf8(attr.key.local_name().as_ref().to_vec())?,
                        attr.decoded_and_normalized_value(
                            quick_xml::XmlVersion::Implicit1_0,
                            reader.decoder(),
                        )?
                        .into_owned(),
                    );
                }
                let xml = reader.read_text(e.name())?.decode()?.into_owned();
                out.push((attrs, xml));
                ensure!(out.len() <= 20_000, "DOCX element limit exceeded");
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, process::Child};

    type SampleHook = Box<dyn FnOnce(&mut Child)>;
    thread_local! {
        static AFTER_SAMPLE: RefCell<Option<SampleHook>> = const { RefCell::new(None) };
    }

    pub(super) fn after_output_sample(child: &mut Child) {
        AFTER_SAMPLE.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook(child);
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn final_capture_limits_reject_writes_between_sample_and_successful_exit() {
        let temp = tempfile::tempdir().unwrap();
        for (stream, output_file) in [("stderr", false), ("stdout", true), ("quiet", true)] {
            let gate = temp.path().join(stream);
            let script = temp.path().join(format!("{stream}.py"));
            std::fs::write(
                &script,
                r#"
import pathlib, sys, time
gate, output, stream = sys.argv[1:]
deadline = time.monotonic() + 5
while not pathlib.Path(gate).exists():
    assert time.monotonic() < deadline
    time.sleep(0.001)
pathlib.Path(output).write_text('bounded output')
if stream != 'quiet':
    getattr(sys, stream).write('x' * 4097)
    getattr(sys, stream).flush()
"#,
            )
            .unwrap();
            let adapter = CommandAdapter {
                program: "python3".into(),
                args: vec![
                    script.to_string_lossy().into_owned(),
                    gate.to_string_lossy().into_owned(),
                    "{output}".into(),
                    stream.into(),
                ],
                output_file,
            };
            AFTER_SAMPLE.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |child| {
                    // The first size sample sees empty streams. Only then let
                    // the child write, and observe success before try_wait.
                    std::fs::write(&gate, b"release").unwrap();
                    assert!(child.wait().unwrap().success());
                }));
            });
            let result = run_bytes(&adapter, None, None, 5, 4096);
            if stream == "quiet" {
                assert_eq!(result.unwrap(), b"bounded output");
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("output exceeds byte limit")
                );
            }
        }
    }
}
