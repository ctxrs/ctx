use super::*;

pub(super) fn robot_dynamic(name: &str) -> bool {
    ["${", "@{", "&{", "%{"]
        .iter()
        .any(|marker| name.contains(marker))
}

pub(super) fn robot_dynamic_import(name: &str) -> bool {
    // robot_import handles the three allowed ${...} path anchors itself.
    ["@{", "&{", "%{"]
        .iter()
        .any(|marker| name.contains(marker))
}

pub(super) fn blank(bytes: &mut [u8], range: Range<usize>) {
    for b in &mut bytes[range] {
        if !matches!(*b, b'\n' | b'\r') {
            *b = b' ';
        }
    }
}

pub(super) fn mask_ranges(source: &str, ranges: &[Range<usize>]) -> String {
    let mut out = source.as_bytes().to_vec();
    blank(&mut out, 0..source.len());
    for range in ranges {
        out[range.clone()].copy_from_slice(&source.as_bytes()[range.clone()]);
    }
    String::from_utf8(out).expect("ranges end at character boundaries")
}

pub(super) fn mask_comments(bytes: &mut [u8], source: &str, open: &str, close: &str) {
    let mut pos = 0;
    while let Some(at) = source[pos..].find(open) {
        let start = pos + at;
        let end = source[start + open.len()..]
            .find(close)
            .map(|p| start + open.len() + p + close.len())
            .unwrap_or(source.len());
        blank(bytes, start..end);
        pos = end;
    }
}

pub(super) fn tag_at(text: &str, start: usize) -> Option<Tag> {
    let b = text.as_bytes();
    let mut pos = start + 1;
    if !b.get(pos)?.is_ascii_alphabetic() {
        return None;
    }
    while b
        .get(pos)
        .is_some_and(|c| c.is_ascii_alphanumeric() || b":._-".contains(c))
    {
        pos += 1;
    }
    let name = text[start + 1..pos].to_owned();
    let mut attrs = vec![];
    loop {
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if *b.get(pos)? == b'>' {
            return Some(Tag {
                name,
                range: start..pos + 1,
                attrs,
            });
        }
        if b.get(pos..pos + 2) == Some(b"/>") {
            return Some(Tag {
                name,
                range: start..pos + 2,
                attrs,
            });
        }
        if b[pos] == b'{' {
            pos = balanced(text, pos, b'{', b'}')?;
            continue;
        }
        let key_start = pos;
        while b
            .get(pos)
            .is_some_and(|c| !c.is_ascii_whitespace() && !b"=>/".contains(c))
        {
            pos += 1;
        }
        if pos == key_start {
            return None;
        }
        let key = text[key_start..pos].to_owned();
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if b.get(pos) != Some(&b'=') {
            attrs.push(Attribute {
                name: key,
                value: String::new(),
                range: key_start..pos,
                braced: false,
            });
            continue;
        }
        pos += 1;
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let start_value = pos;
        let mut braced = false;
        let value = match *b.get(pos)? {
            quote @ (b'\'' | b'"') => {
                pos += 1;
                let from = pos;
                while *b.get(pos)? != quote {
                    pos += 1;
                }
                let s = text[from..pos].to_owned();
                pos += 1;
                s
            }
            b'{' => {
                braced = true;
                let end = balanced(text, pos, b'{', b'}')?;
                let s = text[pos + 1..end - 1].to_owned();
                pos = end;
                s
            }
            _ => {
                while b
                    .get(pos)
                    .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>')
                {
                    pos += 1;
                }
                text[start_value..pos].trim_end_matches('/').into()
            }
        };
        attrs.push(Attribute {
            name: key,
            value,
            range: start_value..pos,
            braced,
        });
    }
}

pub(super) fn find_close_tag(text: &str, start: usize, needle: &str) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let n = needle.as_bytes();
    let mut pos = start;
    while pos + n.len() < b.len() {
        if b[pos..pos + n.len()].eq_ignore_ascii_case(n)
            && (b[pos + n.len()].is_ascii_whitespace() || b[pos + n.len()] == b'>')
        {
            let end = text[pos + n.len()..].find('>')? + pos + n.len() + 1;
            return Some((pos, end));
        }
        pos += 1;
    }
    None
}

pub(super) fn balanced(text: &str, start: usize, open: u8, close: u8) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0;
    let mut pos = start;
    while pos < b.len() {
        match b[pos] {
            quote @ (b'\'' | b'"' | b'`') => {
                pos += 1;
                while pos < b.len() {
                    if b[pos] == b'\\' {
                        pos += 2;
                    } else if b[pos] == quote {
                        pos += 1;
                        break;
                    } else {
                        pos += 1;
                    }
                }
            }
            b'/' if b.get(pos + 1) == Some(&b'/') => {
                while pos < b.len() && b[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'/' if b.get(pos + 1) == Some(&b'*') => {
                pos += 2;
                while pos + 1 < b.len() && &b[pos..pos + 2] != b"*/" {
                    pos += 1;
                }
                pos += 2;
            }
            c if c == open => {
                depth += 1;
                if depth > 256 {
                    return None;
                }
                pos += 1;
            }
            c if c == close => {
                depth -= 1;
                pos += 1;
                if depth == 0 {
                    return Some(pos);
                }
            }
            _ => pos += 1,
        }
    }
    None
}

pub(super) fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c == '$' || c.is_alphabetic() || (i > 0 && c.is_numeric()))
}

pub(super) fn pascal(name: &str) -> String {
    name.split('-')
        .map(|s| {
            let mut chars = s.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

pub(super) fn js_modules(path: &str, import: &str) -> Vec<String> {
    if import.starts_with('.') {
        relative_path(path.rsplit_once('/').map_or("", |(p, _)| p), import)
            .map(|p| vec![format!("javascript:module:{p}")])
            .unwrap_or_default()
    } else {
        vec![format!("javascript:import-module:{import}")]
    }
}

pub(super) fn robot_normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn robot_import(path: &str, raw: &str) -> Option<String> {
    let base = path.rsplit_once('/').map_or("", |(p, _)| p);
    let mut result = String::new();
    let mut rest = raw;
    let mut execution_root = false;
    while let Some(at) = rest.find("${") {
        result.push_str(&rest[..at]);
        let end = rest[at + 2..].find('}')? + at + 2;
        match robot_normalize(&rest[at + 2..end]).as_str() {
            "curdir" if result.is_empty() => result.push('.'),
            "execdir" if result.is_empty() => {
                execution_root = true;
                result.push('.');
            }
            "/" => result.push('/'),
            _ => return None,
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    if result.starts_with('/') || result.contains(['%', '\\']) {
        return None;
    }
    relative_path(if execution_root { "" } else { base }, &result)
}

pub(super) fn robot_cells(line: &str) -> Vec<(usize, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim_start().starts_with('|') {
        let mut pos = line.find('|').unwrap() + 1;
        let mut cells = vec![];
        for s in line[pos..].split('|') {
            let leading = s.len() - s.trim_start().len();
            let value = s.trim();
            if !value.is_empty() {
                cells.push((pos + leading, value));
            }
            pos += s.len() + 1;
        }
        return cells;
    }
    let b = line.as_bytes();
    let mut pos = 0;
    let mut cells = vec![];
    while pos < b.len() {
        while b.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        let start = pos;
        while pos < b.len() && b[pos] != b'\t' && !(b[pos] == b' ' && b.get(pos + 1) == Some(&b' '))
        {
            pos += 1;
        }
        if pos > start {
            let value = line[start..pos].trim_end();
            if value.starts_with('#') {
                break;
            }
            cells.push((start, value));
        }
    }
    cells
}

pub(super) fn toolkit_using(
    root: tree_sitter::Node<'_>,
    member: tree_sitter::Node<'_>,
    source: &str,
    namespace: &str,
) -> bool {
    let has = |node| {
        children(node).into_iter().any(|n| {
            n.kind() == "using_directive"
                && source[n.byte_range()]
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .strip_prefix("using ")
                    .or_else(|| {
                        source[n.byte_range()]
                            .trim()
                            .trim_end_matches(';')
                            .trim()
                            .strip_prefix("global using ")
                    })
                    .is_some_and(|s| s.trim() == namespace)
        })
    };
    if has(root) {
        return true;
    }
    let mut parent = member.parent();
    while let Some(node) = parent {
        if has(node) {
            return true;
        }
        parent = node.parent();
    }
    false
}

pub(super) fn event_signature(node: tree_sitter::Node<'_>, source: &str) -> bool {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return false;
    };
    let parameters: Vec<_> = children(parameters)
        .into_iter()
        .filter(|n| n.kind() == "parameter")
        .collect();
    if parameters.len() != 2 {
        return false;
    }
    let types: Vec<_> = parameters
        .iter()
        .filter_map(|n| n.child_by_field_name("type"))
        .map(|n| source[n.byte_range()].replace(' ', ""))
        .collect();
    types.len() == 2
        && matches!(types[0].trim_end_matches('?'), "object" | "System.Object")
        && types[1]
            .split('<')
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("")
            .ends_with("EventArgs")
}

pub(super) fn qualify(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.into()
    } else {
        format!("{prefix}.{name}")
    }
}

pub(super) fn relative_source(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && !path.split('/').any(|s| matches!(s, "" | "." | ".."))
}

pub(super) fn in_project(path: &str, root: &str) -> bool {
    relative_source(path)
        && (root.is_empty()
            || (relative_source(root)
                && path.strip_prefix(root).is_some_and(|s| s.starts_with('/'))))
}

pub(super) fn project_key(project: &TemplateProject<'_>, category: &str, name: &str) -> String {
    format!("xaml:project:{}:{category}:{name}", project.root)
}

pub(super) fn viewmodel_names(view: &str) -> Vec<String> {
    if view == "MainWindow" {
        return vec!["MainWindowViewModel".into(), "MainViewModel".into()];
    }
    for suffix in ["UserControl", "View", "Page", "Control"] {
        if let Some(stem) = view.strip_suffix(suffix).filter(|s| !s.is_empty()) {
            return vec![format!("{stem}ViewModel")];
        }
    }
    vec![]
}

pub(super) fn robot_file_key(path: &str) -> String {
    if let Some(stem) = path.strip_suffix(".py") {
        let stem = stem
            .strip_prefix("src/")
            .unwrap_or(stem)
            .trim_end_matches("/__init__");
        format!(
            "module:{}",
            if stem == "__init__" {
                String::new()
            } else {
                stem.replace('/', ".")
            }
        )
    } else {
        format!("template:file:{path}")
    }
}

pub(super) fn robot_variables(source: &str) -> HashMap<String, Option<String>> {
    let mut variables = HashMap::new();
    let mut in_variables = false;
    for line in source.lines() {
        let cells = robot_cells(line);
        let Some((_, first)) = cells.first() else {
            continue;
        };
        if first.starts_with("***") {
            in_variables = robot_normalize(first.trim_matches('*')) == "variables";
            continue;
        }
        if !in_variables || !first.starts_with("${") || !first.ends_with('}') {
            continue;
        }
        let name = robot_normalize(&first[2..first.len() - 1]);
        let value = (cells.len() == 2 && !cells[1].1.contains(['\\', '@', '&', '%']))
            .then(|| cells[1].1.to_owned());
        variables
            .entry(name)
            .and_modify(|v| *v = None)
            .or_insert(value);
    }
    variables
}

pub(super) fn robot_expand(
    raw: &str,
    variables: &HashMap<String, Option<String>>,
) -> Option<String> {
    let mut result = raw.to_owned();
    for _ in 0..8 {
        let mut next = String::new();
        let mut rest = result.as_str();
        let mut replaced = false;
        while let Some(at) = rest.find("${") {
            next.push_str(&rest[..at]);
            let end = rest[at + 2..].find('}')? + at + 2;
            let name = robot_normalize(&rest[at + 2..end]);
            if matches!(name.as_str(), "curdir" | "execdir" | "/") {
                next.push_str(&rest[at..=end]);
            } else {
                next.push_str(variables.get(&name)?.as_deref()?);
                replaced = true;
            }
            rest = &rest[end + 1..];
        }
        next.push_str(rest);
        if next.len() > 4096 {
            return None;
        }
        if !replaced {
            return Some(next);
        }
        result = next;
    }
    None
}
