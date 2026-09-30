"""Independent C initializer decoder. Does not import the compactor.

Decode original and output tables, including C zero defaults, then compare every
row and ordered lookahead. Source outside four permitted regions must match.
"""
from array import array
import re


def declarations(text):
    result = {}
    for name in ("ts_parse_table", "ts_small_parse_table", "ts_small_parse_table_map"):
        start = text.index("static const uint", text.index(name) - 30)
        opening = text.index("{", start)
        closing = text.index("\n};", opening)
        result[name] = (opening + 1, closing)
    return result


def decode(text):
    constants = {k: int(v) for k, v in re.findall(r"#define\s+(\w+)\s+(\d+)", text)}
    symbol_text = text.split("enum ts_symbol_identifiers {", 1)[1].split("};", 1)[0]
    symbols = {"ts_builtin_sym_end": 0}
    for assignment in symbol_text.split(","):
        if assignment.strip():
            name, value = assignment.split("=")
            symbols[name.strip()] = int(value.strip())
    spans = declarations(text)

    def tokens(name):
        a, b = spans[name]
        body = text[a:b]
        pattern = r"[A-Za-z_]\w*|\d+|[\[\]{}(),=]"
        if re.sub(pattern, "", body).strip():
            raise ValueError("unknown C initializer token")
        return re.findall(pattern, body)

    def value(ts, pos):
        token = ts[pos]
        if token in ("STATE", "ACTIONS", "SMALL_STATE"):
            if ts[pos + 1] != "(" or ts[pos + 3] != ")":
                raise ValueError("invalid C table initializer")
            number = int(ts[pos + 2])
            return number - (constants["LARGE_STATE_COUNT"] if token == "SMALL_STATE" else 0), pos + 4
        return (int(token) if token.isdecimal() else symbols[token]), pos + 1

    def initializer(ts, pos=0, end=None):
        data, index = {}, 0
        while pos < len(ts) and ts[pos] != end:
            if ts[pos] == "[":
                index, pos = value(ts, pos + 1)
                if ts[pos:pos + 2] != ["]", "="]:
                    raise ValueError("invalid C table initializer")
                pos += 2
            if index in data:
                raise ValueError("duplicate C initializer index")
            if ts[pos] == "{":
                item, pos = initializer(ts, pos + 1, "}")
                if ts[pos] != "}":
                    raise ValueError("invalid C table initializer")
                pos += 1
            else:
                item, pos = value(ts, pos)
            data[index] = item
            index += 1
            if ts[pos] != ",":
                raise ValueError("invalid C table initializer")
            pos += 1
        return data, pos

    arrays = {}
    for name in spans:
        ts = tokens(name)
        data, consumed = initializer(ts)
        if consumed != len(ts):
            raise ValueError("invalid C table initializer")
        arrays[name] = data
    return constants, arrays, spans


def compare(original, transformed):
    before = decode(original)
    after = decode(transformed)
    c, old, _ = before
    d, new, _ = after
    if ({k: v for k, v in c.items() if k != "LARGE_STATE_COUNT"}
            != {k: v for k, v in d.items() if k != "LARGE_STATE_COUNT"}):
        raise ValueError("non-table constants changed")
    skeleton = None
    for text, (_, _, spans) in ((original, before), (transformed, after)):
        for a, b in sorted(spans.values(), reverse=True):
            text = text[:a] + "<TABLE>" + text[b:]
        text = re.sub(r"(#define LARGE_STATE_COUNT )\d+", r"\g<1><COUNT>", text)
        if skeleton is None:
            skeleton = text
        elif text != skeleton:
            raise ValueError("non-table source changed (actions/aliases/lexer/etc)")

    def row(constants, arrays, state):
        if state < constants["LARGE_STATE_COUNT"]:
            values = arrays["ts_parse_table"][state]
            return [(s, values[s]) for s in sorted(values) if values[s]]
        mapping = arrays["ts_small_parse_table_map"]
        data = arrays["ts_small_parse_table"]
        pos = mapping[state - constants["LARGE_STATE_COUNT"]]
        groups = data[pos]
        pos += 1
        ordered = []
        for _ in range(groups):
            v, n = data[pos], data[pos + 1]
            if n <= 0:
                raise ValueError("invalid C table initializer")
            pos += 2
            group = [data[i] for i in range(pos, pos + n)]
            if not all((s < constants["TOKEN_COUNT"]) == (group[0] < constants["TOKEN_COUNT"]) for s in group):
                raise ValueError("mixed action/goto group")
            ordered.extend((s, v) for s in group)
            pos += n
        if len(set(s for s, _ in ordered)) != len(ordered):
            raise ValueError("duplicate sparse symbol")
        if not all(0 <= s < constants["SYMBOL_COUNT"] and 0 < v < 65536 for s, v in ordered):
            raise ValueError("invalid C table initializer")
        return ordered

    for state in range(c["STATE_COUNT"]):
        a, b = row(c, old, state), row(d, new, state)
        if a != b:
            raise ValueError(("ordered lookahead differs", state))
        dense_a, dense_b = array("H", [0]) * c["SYMBOL_COUNT"], array("H", [0]) * c["SYMBOL_COUNT"]
        for s, v in a:
            dense_a[s] = v
        for s, v in b:
            dense_b[s] = v
        if dense_a != dense_b:
            raise ValueError(("state/symbol lookup differs", state))
    return {"states": c["STATE_COUNT"], "symbols": c["SYMBOL_COUNT"], "cells_checked": c["STATE_COUNT"] * c["SYMBOL_COUNT"]}
