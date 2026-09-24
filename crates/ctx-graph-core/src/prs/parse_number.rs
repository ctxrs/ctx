use super::*;

pub(super) fn parse_number(value: &str) -> Result<u64, String> {
    let value = value.strip_prefix('#').unwrap_or(value);
    let number = value
        .parse::<u64>()
        .map_err(|_| "PR number must be a positive integer".to_owned())?;
    if number == 0 || number > i32::MAX as u64 {
        return Err("PR number is outside the supported range".into());
    }
    Ok(number)
}

pub(super) fn validate_args(args: &PrsArgs) -> Result<()> {
    if let Some(repo) = &args.repo {
        validate_repo(repo)?;
    }
    if let Some(number) = args.number {
        parse_number(&number.to_string()).map_err(anyhow::Error::msg)?;
    }
    if let Some(base) = &args.base {
        ensure!(
            !base.is_empty() && base.len() <= 1024 && !base.chars().any(char::is_control),
            "invalid expected base"
        );
    }
    Ok(())
}

pub(super) fn timestamp(text: &str) -> Option<u64> {
    if text.len() != 20
        || !text.is_ascii()
        || &text[4..5] != "-"
        || &text[7..8] != "-"
        || &text[10..11] != "T"
        || &text[13..14] != ":"
        || &text[16..17] != ":"
        || &text[19..] != "Z"
    {
        return None;
    }
    let n = |a, b| {
        let part = &text[a..b];
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse::<u64>().ok()
        } else {
            None
        }
    };
    let (y, m, d, h, min, s) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || h > 23 || min > 59 || s > 59 {
        return None;
    }
    let leap = |y: u64| y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let months = [
        31,
        28 + u64::from(leap(y)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if d == 0 || d > months[(m - 1) as usize] {
        return None;
    }
    let before = |y: u64| (y - 1) / 4 - (y - 1) / 100 + (y - 1) / 400;
    let days = (y - 1970) * 365 + before(y) - before(1970)
        + months[..(m - 1) as usize].iter().sum::<u64>()
        + d
        - 1;
    Some(days * 86400 + h * 3600 + min * 60 + s)
}

pub(super) fn path_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.contains('\0')
        && !path
            .split('/')
            .any(|p| p == "." || p == ".." || p.is_empty())
}

pub(super) fn boundary_match(a: &str, b: &str) -> bool {
    a == b
        || a.strip_suffix(b)
            .is_some_and(|prefix| prefix.ends_with('/'))
        || b.strip_suffix(a)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

pub(super) fn namespace(node: &Node) -> String {
    let mut value = &node.metadata;
    let mut projects = Vec::new();
    while let (Some(project), Some(_), Some(original)) = (
        value.get("project").and_then(Value::as_str),
        value.get("original_id").and_then(Value::as_str),
        value.get("original_metadata"),
    ) {
        projects.push(project);
        value = original;
    }
    serde_json::to_string(&projects).expect("string vector serializes")
}

pub(super) fn community_limit_notice(graph: &GraphSnapshot) -> Result<Option<&'static str>> {
    let references = graph
        .metadata
        .get("graf_unresolved_references")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if graph.nodes.len() > 5_000 || graph.edges.len() > 20_000 || references > 20_000 {
        return Ok(Some(
            "Computed communities omitted: unlabeled snapshot exceeds the analysis limit of 5000 nodes, 20000 edges or 20000 unresolved references. File/node impact remains available; empty communities do not mean no community overlap.",
        ));
    }
    struct ByteLimit {
        bytes: usize,
        exceeded: bool,
    }
    impl std::io::Write for ByteLimit {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.len() > MAX_BYTES - self.bytes {
                self.exceeded = true;
                return Err(std::io::Error::other("analysis byte limit"));
            }
            self.bytes += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut limit = ByteLimit {
        bytes: 0,
        exceeded: false,
    };
    let serialized = serde_json::to_writer(&mut limit, graph);
    if limit.exceeded {
        return Ok(Some(
            "Computed communities omitted: unlabeled snapshot exceeds the 8 MiB serialized analysis limit. File/node impact remains available; empty communities do not mean no community overlap.",
        ));
    }
    serialized.context("cannot measure PR analysis snapshot")?;
    Ok(None)
}
