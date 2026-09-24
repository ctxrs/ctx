use crate::{Diagnostic, FileFacts};
use anyhow::{Context, Result};
use tree_sitter::{Language, Node as Syntax, Parser, Tree};

pub fn children(node: Syntax<'_>) -> Vec<Syntax<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}
pub fn line(node: Syntax<'_>) -> u32 {
    node.start_position().row as u32 + 1
}
pub fn end_line(node: Syntax<'_>) -> u32 {
    let end = node.end_position();
    (end.row + usize::from(end.column != 0 || end.row == 0)) as u32
}
pub fn module_path(path: &str) -> String {
    path.rsplit_once('.')
        .filter(|(stem, suffix)| !stem.is_empty() && !stem.ends_with('/') && !suffix.contains('/'))
        .map_or(path, |(stem, _)| stem)
        .into()
}
pub fn relative_path(base: &str, import: &str) -> Option<String> {
    let mut parts: Vec<_> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in import.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}
pub fn tree(language: Language, source: &str, facts: &mut FileFacts) -> Result<Option<Tree>> {
    if source.len() > crate::MAX_SOURCE_BYTES {
        diagnostic(facts, None, "Source exceeds the 4 MiB indexing limit");
        return Ok(None);
    }
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .context("language parser did not return a tree")?;
    let mut pending = vec![(tree.root_node(), 0)];
    while let Some((node, depth)) = pending.pop() {
        if node.is_error() || node.is_missing() || depth > 256 {
            diagnostic(
                facts,
                Some(line(node)),
                if depth > 256 {
                    "Syntax nesting exceeds the indexing limit; no facts indexed"
                } else {
                    "Syntax error; no facts indexed"
                },
            );
            return Ok(None);
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor).map(|n| (n, depth + 1)));
    }
    Ok(Some(tree))
}
pub fn diagnostic(facts: &mut FileFacts, line: Option<u32>, message: &str) {
    facts.diagnostics.push(Diagnostic {
        file: facts.path.clone(),
        line,
        message: message.into(),
    });
}
