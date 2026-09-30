#!/usr/bin/env python3
"""Re-encode pinned tree-sitter dense rows in the native sparse format.

Only build output is written. State IDs, action IDs and lookahead order stay fixed.
"""
import itertools
import re


def section(text, name):
    pattern = rf"static const uint(?:16|32)_t {name}\[[^;=]+? = \{{\n(.*?)\n\}};"
    matches = list(re.finditer(pattern, text, re.S))
    if len(matches) != 1:
        raise ValueError(f"expected one declaration of {name}")
    return matches[0]


def compact(text):
    keep = 2  # Release bridge only: keep the error and initial states dense.
    constants = {k: int(v) for k, v in re.findall(r"^#define (\w+) (\d+)$", text, re.M)}
    old_large = constants["LARGE_STATE_COUNT"]
    if not 2 <= keep <= old_large:
        raise ValueError("dense prefix must retain state 0 and 1")
    enum = re.search(r"enum ts_symbol_identifiers \{(.*?)\};", text, re.S)
    symbols = {"ts_builtin_sym_end": 0}
    symbols.update({k: int(v) for k, v in re.findall(r"(\w+) = (\d+),", enum[1])})
    dense = section(text, "ts_parse_table")
    small = section(text, "ts_small_parse_table")
    mapping = section(text, "ts_small_parse_table_map")
    row_pattern = re.compile(r"  \[(?:STATE\()?(\d+)\)?\] = \{\n(.*?)  \},", re.S)
    rows = list(row_pattern.finditer(dense[1]))
    if row_pattern.sub("", dense[1]).strip() or [int(m[1]) for m in rows] != list(range(old_large)):
        raise ValueError("unrecognized dense row layout")
    # Count existing sparse initializer elements, including designated offsets.
    item = re.compile(r"(?:\[(\d+)\]\s*=\s*)?(\w+(?:\(\d+\))?),")
    cursor = 0
    for m in item.finditer(small[1]):
        if m[1] is not None:
            if int(m[1]) != cursor:
                raise ValueError("non-contiguous original sparse initializer")
        cursor += 1
    if item.sub("", small[1]).strip():
        raise ValueError("unrecognized sparse initializer")
    original_words = cursor
    converted, new_map = [], []
    entry = re.compile(r"\[(\w+)\] = (ACTIONS|STATE)\((\d+)\),")
    for row in rows[keep:]:
        entries = list(entry.finditer(row[2]))
        if entry.sub("", row[2]).strip():
            raise ValueError("unrecognized dense entry")
        values = [(symbols[m[1]], m[2], int(m[3])) for m in entries]
        if len({s for s, _, _ in values}) != len(values):
            raise ValueError("duplicate symbol")
        for symbol, kind, value in values:
            if not 0 <= symbol < constants["SYMBOL_COUNT"] or not 0 <= value <= 65535:
                raise ValueError("entry out of range")
            if (symbol < constants["TOKEN_COUNT"]) != (kind == "ACTIONS"):
                raise ValueError("action/state category mismatch")
        values = sorted(v for v in values if v[2])
        groups = [(key, list(items)) for key, items in itertools.groupby(values, lambda v: (v[1], v[2]))]
        words = [len(groups)]
        for (_, value), members in groups:
            words.extend([value, len(members), *(s for s, _, _ in members)])
        if any(w > 65535 for w in words) or cursor + len(words) > 2**32 - 1:
            raise ValueError("sparse integer overflow")
        converted.append(f"  [{cursor}] = " + ", ".join(map(str, words)) + ",")
        new_map.append(f"  [SMALL_STATE({row[1]})] = {cursor},")
        cursor += len(words)
    replacements = [
        (dense.start(1), dense.end(1), "\n".join(m[0] for m in rows[:keep])),
        (small.start(1), small.end(1), small[1] + "\n" + "\n".join(converted)),
        (mapping.start(1), mapping.end(1), mapping[1] + "\n" + "\n".join(new_map)),
    ]
    count = re.search(r"^#define LARGE_STATE_COUNT (\d+)$", text, re.M)
    replacements.append((count.start(1), count.end(1), str(keep)))
    for start, end, replacement in sorted(replacements, reverse=True):
        text = text[:start] + replacement + text[end:]
    saving = (old_large - keep) * constants["SYMBOL_COUNT"] * 2 - (cursor - original_words) * 2 - (old_large - keep) * 4
    return text, {"converted_rows": old_large - keep, "saved_bytes": saving, "new_sparse_words": cursor - original_words}
