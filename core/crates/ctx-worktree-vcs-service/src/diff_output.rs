use anyhow::{bail, Context, Result};

pub const WORKTREE_VCS_CONTAINER_DIFF_SCRIPT: &str = r#"
set -euo pipefail
base="$1"
git diff "$base"
max_bytes=$((512 * 1024))
while IFS= read -r f; do
  [ -z "$f" ] && continue
  size="$(stat -c %s -- "$f" 2>/dev/null || echo 0)"
  case "$size" in
    ''|*[!0-9]*) size=0 ;;
  esac
  if [ "$size" -gt "$max_bytes" ]; then
    printf '\n# untracked: %s (%s bytes; omitted)\n' "$f" "$size"
    continue
  fi
  patch="$(git diff --no-index -- /dev/null "$f" || true)"
  if [ -n "$patch" ]; then
    printf '\n%s\n' "$patch"
  fi
done < <(git ls-files --others --exclude-standard)
"#;

pub const WORKTREE_VCS_CONTAINER_DIFF_SUMMARY_SCRIPT: &str = r#"
set -euo pipefail
base="$1"
file_count=0
additions=0
deletions=0
while IFS=$'\t' read -r add del path; do
  [ -z "$path" ] && continue
  file_count=$((file_count+1))
  if [ "$add" != "-" ]; then
    additions=$((additions+add))
  fi
  if [ "$del" != "-" ]; then
    deletions=$((deletions+del))
  fi
done < <(git diff --numstat "$base")

max_bytes=$((512 * 1024))
while IFS= read -r f; do
  [ -z "$f" ] && continue
  file_count=$((file_count+1))
  size="$(stat -c %s -- "$f" 2>/dev/null || echo 0)"
  case "$size" in
    ''|*[!0-9]*) size=0 ;;
  esac
  if [ "$size" -gt "$max_bytes" ]; then
    continue
  fi
  lines="$(awk 'END{print NR}' -- "$f" 2>/dev/null || echo 0)"
  case "$lines" in
    ''|*[!0-9]*) lines=0 ;;
  esac
  additions=$((additions+lines))
done < <(git ls-files --others --exclude-standard)

printf '%s %s %s\n' "$file_count" "$additions" "$deletions"
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorktreeVcsDiffSummaryCounts {
    pub file_count: i64,
    pub line_additions: i64,
    pub line_deletions: i64,
}

pub fn parse_worktree_vcs_diff_summary_counts(
    output: &[u8],
) -> Result<WorktreeVcsDiffSummaryCounts> {
    let text = std::str::from_utf8(output).context("diff summary output was not utf-8")?;
    let mut parts = text.split_whitespace();
    let file_count = parse_non_negative_count(&mut parts, "file_count")?;
    let line_additions = parse_non_negative_count(&mut parts, "line_additions")?;
    let line_deletions = parse_non_negative_count(&mut parts, "line_deletions")?;
    if let Some(extra) = parts.next() {
        bail!("diff summary output had unexpected extra field `{extra}`");
    }
    Ok(WorktreeVcsDiffSummaryCounts {
        file_count,
        line_additions,
        line_deletions,
    })
}

fn parse_non_negative_count<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    field: &str,
) -> Result<i64> {
    let raw = parts
        .next()
        .with_context(|| format!("diff summary output missing {field}"))?;
    let value = raw
        .parse::<i64>()
        .with_context(|| format!("diff summary output has invalid {field} `{raw}`"))?;
    if value < 0 {
        bail!("diff summary output has negative {field} `{raw}`");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diff_summary_counts() {
        let counts = parse_worktree_vcs_diff_summary_counts(b"3 12 4\n").unwrap();

        assert_eq!(
            counts,
            WorktreeVcsDiffSummaryCounts {
                file_count: 3,
                line_additions: 12,
                line_deletions: 4,
            }
        );
    }

    #[test]
    fn rejects_malformed_diff_summary_counts() {
        assert!(parse_worktree_vcs_diff_summary_counts(b"3 12").is_err());
        assert!(parse_worktree_vcs_diff_summary_counts(b"3 nope 4").is_err());
        assert!(parse_worktree_vcs_diff_summary_counts(b"3 12 4 extra").is_err());
        assert!(parse_worktree_vcs_diff_summary_counts(b"3 -1 4").is_err());
    }
}
