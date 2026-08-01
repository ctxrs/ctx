#!/usr/bin/env python3
"""Keep the public SQL guide aligned with the shipped relational views."""

from __future__ import annotations

import re
import sqlite3
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCHEMA_PATH = (
    ROOT
    / "crates"
    / "ctx-history-relational"
    / "src"
    / "source_backed_relational"
    / "schema.rs"
)
DOC_PATH = ROOT / "docs" / "sql.md"
SCHEMA_START = 'pub(super) const SCHEMA_SQL: &str = r#"\n'
SCHEMA_END = '"#;'


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def relational_schema_sql() -> str:
    source = SCHEMA_PATH.read_text()
    start = source.find(SCHEMA_START)
    require(start >= 0, f"could not find SCHEMA_SQL in {SCHEMA_PATH}")
    start += len(SCHEMA_START)
    end = source.find(SCHEMA_END, start)
    require(end >= 0, f"could not find the end of SCHEMA_SQL in {SCHEMA_PATH}")
    return source[start:end]


def stable_views(conn: sqlite3.Connection) -> dict[str, list[str]]:
    names = [
        row[0]
        for row in conn.execute(
            "SELECT name FROM sqlite_schema "
            "WHERE type = 'view' AND name LIKE 'ctx_%' ORDER BY name"
        )
    ]
    require(names, "relational schema defines no stable ctx_* views")
    return {
        name: [row[1] for row in conn.execute(f"PRAGMA table_info('{name}')")]
        for name in names
    }


def documented_views(markdown: str) -> dict[str, list[str]]:
    views: dict[str, list[str]] = {}
    current: str | None = None
    heading = re.compile(r"^### `(?P<name>ctx_[a-z_]+)`$")
    column = re.compile(r"^\| `(?P<name>[a-z0-9_]+)` \|")
    for line in markdown.splitlines():
        match = heading.match(line)
        if match:
            current = match.group("name")
            require(current not in views, f"duplicate SQL view heading {current}")
            views[current] = []
            continue
        if line.startswith("### "):
            current = None
            continue
        match = column.match(line)
        if current is not None and match:
            views[current].append(match.group("name"))
    return views


def validate_examples(conn: sqlite3.Connection, markdown: str) -> int:
    examples = re.findall(r"```sql\n(.*?)\n```", markdown, flags=re.DOTALL)
    require(len(examples) >= 5, "docs/sql.md must contain runnable SQL examples")
    for index, sql in enumerate(examples, start=1):
        statement = sql.strip()
        require(
            statement.lower().startswith(("select ", "with ")),
            f"SQL example {index} is not a read-only query",
        )
        conn.execute(statement)
    return len(examples)


def main() -> None:
    markdown = DOC_PATH.read_text()
    conn = sqlite3.connect(":memory:")
    conn.executescript(relational_schema_sql())

    actual = stable_views(conn)
    documented = documented_views(markdown)
    require(
        documented.keys() == actual.keys(),
        "documented stable views differ from schema: "
        f"documented={sorted(documented)} actual={sorted(actual)}",
    )
    for view, columns in actual.items():
        require(
            documented[view] == columns,
            f"{view} columns differ: documented={documented[view]} actual={columns}",
        )

    example_count = validate_examples(conn, markdown)
    print(f"validated {len(actual)} stable SQL views and {example_count} runnable examples")


if __name__ == "__main__":
    main()
