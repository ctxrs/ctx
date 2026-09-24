use super::{base, edge};
use crate::model::{FileFacts, Node, Reference};
use anyhow::{Context, Result, ensure};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::path::Path;

const MAX_FACTS: usize = 20_000;
const MAX_WIKI_PATH_BYTES: usize = 4096;
const MAX_WIKI_PATH_SEGMENTS: usize = 256;

pub(super) fn parse_as(path: &str, text: &str, hash: &str, format: &str) -> Result<FileFacts> {
    match format {
        "md" | "markdown" | "mdx" | "qmd" | "skill" => markdown(path, text, hash),
        "html" | "htm" => html(path, text, hash),
        "yaml" | "yml" => yaml(path, text, hash),
        "gdoc" | "gsheet" | "gslides" => {
            let mut facts = base(path, hash, "document_pointer");
            let pointer = google_pointer(text)?;
            if let Some(url) = pointer["url"].as_str().filter(|url| !url.is_empty()) {
                link(&mut facts, url, url, 1)?;
            }
            facts.nodes[0].metadata["pointer"] = pointer;
            Ok(facts)
        }
        "url" | "webloc" => {
            let mut facts = base(path, hash, "document_pointer");
            for line in text.lines() {
                if let Some(url) = line.trim().strip_prefix("URL=") {
                    link(&mut facts, url, url, 1)?;
                }
            }
            if path.ends_with(".webloc") {
                for value in super::convert::xml_values(text.as_bytes(), b"string")? {
                    if value.starts_with("https://") || value.starts_with("http://") {
                        link(&mut facts, &value, &value, 1)?;
                    }
                }
            }
            Ok(facts)
        }
        _ => plain(path, text, hash),
    }
}

fn section(
    facts: &mut FileFacts,
    label: &str,
    line: u32,
    kind: &str,
    metadata: Value,
) -> Result<String> {
    ensure!(
        facts.nodes.len() + facts.references.len() < MAX_FACTS,
        "document fact limit exceeded"
    );
    let id = format!("document:{}:{kind}:{}", facts.path, facts.nodes.len());
    facts.nodes.push(Node {
        id: id.clone(),
        label: label.chars().take(512).collect(),
        kind: kind.into(),
        file: facts.path.clone(),
        line: Some(line),
        end_line: Some(line),
        qualified_name: None,
        binding_key: None,
        metadata,
    });
    let root = facts.nodes[0].id.clone();
    edge(
        facts,
        &root,
        &id,
        if kind == "rationale" {
            "explains"
        } else {
            "contains"
        },
        line,
        json!({"provenance":"document_syntax"}),
    );
    Ok(id)
}

fn reference(
    facts: &mut FileFacts,
    label: &str,
    relation: &str,
    line: u32,
    keys: Vec<String>,
    reason: &str,
) -> Result<()> {
    ensure!(
        facts.nodes.len() + facts.references.len() < MAX_FACTS,
        "document fact limit exceeded"
    );
    facts.references.push(Reference {
        id: format!("document-ref:{}:{}", facts.path, facts.references.len()),
        source: facts.nodes[0].id.clone(),
        label: label.into(),
        relation: relation.into(),
        file: facts.path.clone(),
        line,
        candidate_keys: keys,
        reason: reason.into(),
    });
    Ok(())
}

fn local_target(file: &str, target: &str) -> Option<String> {
    let target = target.split(['#', '?']).next()?;
    if target.is_empty() {
        return Some(file.into());
    }
    if target.contains([':', '\\', '\0']) || target.starts_with('/') {
        return None;
    }
    let mut parts: Vec<_> = file.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn bounded_wiki_path(path: &str) -> bool {
    path.len() <= MAX_WIKI_PATH_BYTES
        && path.split('/').count() <= MAX_WIKI_PATH_SEGMENTS
        && !path.chars().any(char::is_control)
}

fn wiki_target(file: &str, target: &str) -> Option<String> {
    if !bounded_wiki_path(target) || matches!(target.rsplit('/').next(), Some("" | "." | "..")) {
        return None;
    }
    let mut target = local_target(file, target)?;
    if target.is_empty() {
        return None;
    }
    if Path::new(&target).extension().is_none() {
        target.push_str(".md");
    }
    bounded_wiki_path(&target).then_some(target)
}

fn wiki_link(facts: &mut FileFacts, text: &str, line: u32) -> Result<()> {
    if !bounded_wiki_path(text) {
        return Ok(());
    }
    let (target, label) = text.split_once('|').unwrap_or((text, text));
    let target = target.trim();
    let label = label.trim();
    let path = target.split(['#', '?']).next().unwrap_or("");
    let mut candidates = Vec::new();
    if path.is_empty() && target.starts_with('#') {
        candidates.push(format!("file:{}", facts.path));
    } else if let Some(sibling) = wiki_target(&facts.path, path) {
        candidates.push(format!("file:{sibling}"));
        // An empty base normalizes against the corpus root. Outward paths have
        // no fallback; the sibling normalization above must also have succeeded.
        if let Some(root) = wiki_target("", path) {
            let key = format!("file:{root}");
            if !candidates.contains(&key) {
                candidates.push(key);
            }
            candidates.push(format!("document-wiki:{root}"));
        }
    }
    if !candidates.is_empty() {
        reference(
            facts,
            if label.is_empty() { target } else { label },
            "references",
            line,
            candidates,
            "literal document wikilink",
        )?;
    }
    Ok(())
}

fn wiki_aliases(facts: &mut FileFacts) {
    if !bounded_wiki_path(&facts.path) {
        return;
    }
    let mut aliases = facts.nodes[0].metadata["binding_aliases"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut suffix = facts.path.as_str();
    loop {
        let alias = json!(format!("document-wiki:{suffix}"));
        if !aliases.contains(&alias) {
            aliases.push(alias);
        }
        let Some((_, tail)) = suffix.split_once('/') else {
            break;
        };
        suffix = tail;
    }
    facts.nodes[0].metadata["binding_aliases"] = json!(aliases);
}

fn link(facts: &mut FileFacts, target: &str, label: &str, line: u32) -> Result<()> {
    if target.starts_with("http://") || target.starts_with("https://") {
        // Preserve evidence only. This function never sends a request.
        // URL templates and incomplete examples are ordinary prose. Do not turn
        // rejected URLs (including embedded credentials) into link evidence.
        let Ok(parsed) = super::safe_url(target, false) else {
            return Ok(());
        };
        let id = section(
            facts,
            if label.is_empty() { target } else { label },
            line,
            "url",
            json!({"url":parsed.as_str(),"provenance":"literal_link"}),
        )?;
        let root = facts.nodes[0].id.clone();
        edge(
            facts,
            &root,
            &id,
            "references",
            line,
            json!({"target":target}),
        );
    } else if let Some(target) = local_target(&facts.path, target) {
        let mut candidates = vec![format!("file:{target}")];
        if Path::new(&target).extension().is_none() {
            candidates.push(format!("file:{target}.md"));
        }
        reference(
            facts,
            label,
            "references",
            line,
            candidates,
            "literal document link",
        )?;
    }
    Ok(())
}

fn prose(facts: &mut FileFacts, text: &str, line: u32) -> Result<()> {
    let lower = text.to_ascii_lowercase();
    if [
        "because ",
        "rationale:",
        "decision:",
        "trade-off",
        "tradeoff",
        "we chose ",
        "we use ",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        section(
            facts,
            text.trim(),
            line,
            "rationale",
            json!({"evidence":text,"provenance":"rationale_marker"}),
        )?;
    }
    for token in text.split_whitespace() {
        let token = token
            .trim_matches(|c: char| matches!(c, '(' | ')' | '<' | '>' | '"' | '\'' | ',' | ';'));
        if token.starts_with("https://") || token.starts_with("http://") {
            link(facts, token, token, line)?;
        }
    }
    // Only wikilinks may fall back to unique indexed Markdown path suffixes.
    for tail in text.split("[[").skip(1) {
        if let Some((target, _)) = tail.split_once("]]") {
            wiki_link(facts, target, line)?;
        }
    }
    Ok(())
}

fn markdown(path: &str, text: &str, hash: &str) -> Result<FileFacts> {
    let mut facts = base(path, hash, "document");
    wiki_aliases(&mut facts);
    facts.nodes[0].end_line = Some(text.lines().count().max(1) as u32);
    let mut start = 0;
    if text.starts_with("---\n") || text.starts_with("---\r\n") {
        let first = text.find('\n').unwrap() + 1;
        let mut offset = first;
        for line in text[first..].split_inclusive('\n').take(200) {
            if matches!(line.trim(), "---" | "...") {
                let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text[first..offset])
                    .context("invalid Markdown frontmatter")?;
                facts.nodes[0].metadata["frontmatter"] = serde_json::to_value(value)?;
                start = offset + line.len();
                break;
            }
            offset += line.len();
        }
    }
    let mut heading: Option<(String, u32, u32, usize)> = None;
    let mut headings: Vec<(u32, String)> = vec![];
    let mut paragraph = String::new();
    let mut paragraph_line = 1;
    let mut in_code = false;
    let mut links: Vec<(String, String, u32)> = vec![];
    let newline_offsets: Vec<usize> = text.match_indices('\n').map(|(i, _)| i).collect();
    for (event, range) in Parser::new_ext(
        &text[start..],
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    )
    .into_offset_iter()
    {
        let line = (newline_offsets.partition_point(|n| *n < range.start + start) + 1) as u32;
        let reference_start = facts.references.len();
        match event {
            Event::Start(Tag::Paragraph) => {
                paragraph.clear();
                paragraph_line = line;
            }
            Event::End(TagEnd::Paragraph) => {
                prose(&mut facts, &paragraph, paragraph_line)?;
            }
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((String::new(), line, level as u32, facts.references.len()))
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((label, line, level, first_reference)) = heading.take() {
                    let id = section(&mut facts, &label, line, "heading", json!({"level":level}))?;
                    while headings.last().is_some_and(|(parent, _)| *parent >= level) {
                        headings.pop();
                    }
                    if let Some((_, parent)) = headings.last() {
                        facts.edges.last_mut().unwrap().source = parent.clone();
                    }
                    for reference in &mut facts.references[first_reference..] {
                        reference.source = id.clone();
                    }
                    headings.push((level, id));
                }
            }
            Event::Start(Tag::Link { dest_url, .. })
            | Event::Start(Tag::Image { dest_url, .. }) => {
                links.push((dest_url.into_string(), String::new(), line))
            }
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                if let Some((url, label, line)) = links.pop() {
                    link(&mut facts, &url, &label, line)?;
                }
            }
            Event::Text(value) if !in_code => {
                if let Some((label, _, _, _)) = &mut heading {
                    label.push_str(&value);
                }
                if let Some((_, label, _)) = links.last_mut() {
                    label.push_str(&value);
                }
                paragraph.push_str(&value);
            }
            Event::SoftBreak | Event::HardBreak => paragraph.push('\n'),
            Event::Code(value) if !in_code => {
                if let Some((label, _, _, _)) = &mut heading {
                    label.push_str(&value);
                }
                let name = value.trim_end_matches("()");
                if let Some((file, symbol)) =
                    name.split_once("::").filter(|(file, _)| file.contains('/'))
                {
                    if super::validate_relative(file).is_ok() && !symbol.is_empty() {
                        reference(
                            &mut facts,
                            &value,
                            "documents",
                            line,
                            vec![format!("symbol:{file}::{}", symbol.replace("::", "."))],
                            "path-qualified inline code mention",
                        )?;
                    }
                } else if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))
                {
                    let bare = name;
                    reference(
                        &mut facts,
                        &value,
                        "documents",
                        line,
                        vec![format!("symbol:{bare}")],
                        "inline code mention; resolve only an unambiguous definition",
                    )?;
                }
            }
            _ => {}
        }
        if heading.is_none()
            && let Some((_, owner)) = headings.last()
        {
            for reference in &mut facts.references[reference_start..] {
                reference.source = owner.clone();
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    facts.references.retain(|r| {
        seen.insert((
            r.source.clone(),
            r.relation.clone(),
            r.candidate_keys.clone(),
        ))
    });
    Ok(facts)
}

pub(super) fn plain(path: &str, text: &str, hash: &str) -> Result<FileFacts> {
    let mut facts = base(path, hash, "document");
    facts.nodes[0].end_line = Some(text.lines().count().max(1) as u32);
    let lines: Vec<_> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(next) = lines.get(i + 1) {
            let next = next.trim();
            if !line.trim().is_empty()
                && next.len() >= 3
                && next.chars().all(|c| c == next.chars().next().unwrap())
                && next.starts_with(['=', '-', '~', '^', '"'])
            {
                section(
                    &mut facts,
                    line.trim(),
                    i as u32 + 1,
                    "heading",
                    json!({"provenance":"underlined_heading"}),
                )?;
            }
        }
        prose(&mut facts, line, i as u32 + 1)?;
        // RST explicit hyperlink and document/code roles.
        for tail in line.split('`').skip(1).step_by(2) {
            if let Some((label, target)) = tail.rsplit_once(" <") {
                link(
                    &mut facts,
                    target.trim_end_matches('>'),
                    label,
                    i as u32 + 1,
                )?;
            }
        }
    }
    Ok(facts)
}

fn html(path: &str, text: &str, hash: &str) -> Result<FileFacts> {
    let mut facts = base(path, hash, "document");
    let html = Html::parse_document(text);
    let selector = Selector::parse("h1,h2,h3,h4,h5,h6,a[href],p").expect("constant selector");
    for el in html.select(&selector) {
        if el
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|a| matches!(a.value().name(), "script" | "style" | "template"))
        {
            continue;
        }
        let label = visible_text(el);
        // HTML5 repair changes source positions: do not invent source lines.
        let before = facts.nodes.len();
        match el.value().name() {
            "a" => link(&mut facts, el.value().attr("href").unwrap_or(""), &label, 1)?,
            "p" => prose(&mut facts, &label, 1)?,
            _ => {
                section(
                    &mut facts,
                    &label,
                    1,
                    "heading",
                    json!({"tag":el.value().name()}),
                )?;
            }
        }
        for n in &mut facts.nodes[before..] {
            n.line = None;
            n.end_line = None;
        }
    }
    for e in &mut facts.edges {
        e.line = None;
    }
    facts.nodes[0].metadata["line_basis"] =
        json!("html_structure; reference line 1 denotes document");
    let title = Selector::parse("title").expect("constant selector");
    if let Some(title) = html.select(&title).next() {
        facts.nodes[0].metadata["title"] = json!(visible_text(title));
    }
    facts.nodes[0].metadata["text"] = json!(html_text(text));
    Ok(facts)
}

fn yaml(path: &str, text: &str, hash: &str) -> Result<FileFacts> {
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).context("invalid YAML document")?;
    let mut facts = base(path, hash, "document");
    let mut pending = vec![(String::new(), &value, 0)];
    let mut visited = 0;
    while let Some((name, value, depth)) = pending.pop() {
        visited += 1;
        ensure!(
            depth <= 64 && visited < MAX_FACTS,
            "YAML nesting/fact limit exceeded"
        );
        match value {
            serde_yaml_ng::Value::Mapping(map) => {
                for (key, value) in map {
                    if let Some(key) = key.as_str() {
                        pending.push((
                            if name.is_empty() {
                                key.into()
                            } else {
                                format!("{name}.{key}")
                            },
                            value,
                            depth + 1,
                        ));
                    }
                }
            }
            serde_yaml_ng::Value::Sequence(values) => {
                for (i, value) in values.iter().enumerate() {
                    pending.push((format!("{name}[{i}]"), value, depth + 1));
                }
            }
            serde_yaml_ng::Value::String(value) => {
                section(
                    &mut facts,
                    &name,
                    1,
                    "document_field",
                    json!({"value":value,"provenance":"yaml_scalar"}),
                )?;
                prose(&mut facts, value, 1)?;
            }
            _ => {}
        }
    }
    for n in &mut facts.nodes[1..] {
        n.line = None;
        n.end_line = None;
    }
    for e in &mut facts.edges {
        e.line = None;
    }
    Ok(facts)
}

pub(super) fn google_pointer(text: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(text).context("invalid Google pointer JSON")?;
    let url = value["url"].as_str().unwrap_or("");
    let mut id = ["doc_id", "file_id", "fileId", "id"]
        .iter()
        .find_map(|key| value[*key].as_str())
        .map(str::to_owned);
    if id.is_none() {
        id = value["resource_id"]
            .as_str()
            .and_then(|s| s.split_once(':'))
            .map(|(_, id)| id.to_owned());
    }
    if !url.is_empty() {
        let parsed = super::safe_url(url, false)?;
        ensure!(
            matches!(
                parsed.host_str(),
                Some("docs.google.com" | "drive.google.com")
            ),
            "Google pointer must use a Google document/Drive URL"
        );
        if id.is_none() {
            let parts: Vec<_> = parsed.path_segments().into_iter().flatten().collect();
            id = parts
                .windows(2)
                .find(|p| p[0] == "d")
                .map(|p| p[1].to_owned())
                .or_else(|| {
                    parsed
                        .query_pairs()
                        .find(|(k, _)| k == "id")
                        .map(|(_, v)| v.into_owned())
                });
        }
    }
    let id = id.context("Google pointer has no file ID")?;
    ensure!(
        !id.is_empty()
            && id.len() <= 256
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')),
        "invalid Google file ID"
    );
    // Deliberately omit account email, resource keys, and other pointer metadata.
    let url = if url.is_empty() {
        String::new()
    } else {
        let mut parsed = super::safe_url(url, false)?;
        parsed.set_query(None);
        parsed.set_fragment(None);
        parsed.to_string()
    };
    Ok(json!({"file_id":id,"url":url}))
}

pub(super) fn html_text(text: &str) -> String {
    visible_text(Html::parse_document(text).root_element())
}

fn visible_text(element: scraper::ElementRef<'_>) -> String {
    element
        .descendants()
        .filter(|node| {
            !node
                .ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .any(|a| matches!(a.value().name(), "script" | "style" | "template"))
        })
        .filter_map(|node| node.value().as_text())
        .map(|text| text.text.as_ref())
        .collect::<Vec<&str>>()
        .join(" ")
}
