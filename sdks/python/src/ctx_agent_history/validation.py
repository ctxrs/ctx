"""SDK input validation helpers."""

from __future__ import annotations

import json
from collections.abc import Mapping
from typing import Optional, cast

from .errors import CtxAgentHistoryValidationError
from .types import SearchQueryV1

SEARCH_QUERY_VERSION = "ctx-search-v1"
SEARCH_MAX_CLAUSES = 32
SEARCH_MAX_CLAUSE_BYTES = 1_024
SEARCH_MAX_TOTAL_CLAUSE_BYTES = 8_192
SEARCH_MAX_QUERY_JSON_BYTES = 64 * 1_024
SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE = 32
SEARCH_MIN_LITERAL_BYTES = 3
SEARCH_MAX_LITERAL_BYTES = 256
SEARCH_MAX_RESULTS = 200

_QUERY_FIELDS = frozenset({"version", "any", "must", "must_not"})
_LEXICAL_MATCHERS = frozenset({"all", "phrase", "literal"})
_ANY_MATCHERS = frozenset({*_LEXICAL_MATCHERS, "semantic"})


def serialize_search_query(query: SearchQueryV1) -> str:
    """Validate and serialize one canonical ctx-search-v1 text expression."""

    canonical = validate_search_query(query)
    serialized = json.dumps(canonical, ensure_ascii=False, separators=(",", ":"))
    encoded_bytes = len(serialized.encode("utf-8"))
    if encoded_bytes > SEARCH_MAX_QUERY_JSON_BYTES:
        _invalid(
            "search query JSON exceeds the 65536-byte limit",
            {"actualBytes": encoded_bytes, "maximumBytes": SEARCH_MAX_QUERY_JSON_BYTES},
        )
    return serialized


def validate_search_query(query: SearchQueryV1) -> SearchQueryV1:
    if not isinstance(query, Mapping):
        _invalid("search query must be an object", {"queryType": type(query).__name__})

    unknown = next((str(field) for field in query if field not in _QUERY_FIELDS), None)
    if unknown is not None:
        _invalid("search query contains an unknown field", {"field": unknown})
    if query.get("version") != SEARCH_QUERY_VERSION:
        _invalid(
            "search query version must be ctx-search-v1",
            {"version": query.get("version")},
        )

    canonical: dict[str, object] = {"version": SEARCH_QUERY_VERSION}
    canonical_placements: dict[str, list[dict[str, str]]] = {}
    for placement in ("any", "must", "must_not"):
        raw_clauses = query.get(placement, [])
        if not isinstance(raw_clauses, list):
            _invalid(f"search query {placement} must be an array", {"placement": placement})
        clauses: list[dict[str, str]] = []
        seen: set[tuple[str, str]] = set()
        allowed = _ANY_MATCHERS if placement == "any" else _LEXICAL_MATCHERS
        for raw_clause in raw_clauses:
            if not isinstance(raw_clause, Mapping):
                _invalid("search clause must be an object", {"placement": placement})
            keys = []
            for key in raw_clause:
                keys.append(key)
                if len(keys) > 1:
                    break
            if len(keys) != 1 or keys[0] not in allowed:
                _invalid(
                    "search clause must contain exactly one allowed matcher",
                    {"placement": placement, "matchers": keys},
                )
            matcher = keys[0]
            value = raw_clause[matcher]
            if not isinstance(value, str):
                _invalid(
                    "search clause value must be a string",
                    {"placement": placement, "matcher": matcher},
                )
            canonical_value = value.strip() if matcher == "literal" else " ".join(value.split())
            identity = (matcher, canonical_value)
            if identity in seen:
                continue
            seen.add(identity)
            clauses.append({matcher: canonical_value})
        if clauses:
            canonical[placement] = clauses
            canonical_placements[placement] = clauses

    positive_clauses = len(canonical_placements.get("any", [])) + len(
        canonical_placements.get("must", [])
    )
    if positive_clauses == 0:
        _invalid("search query needs a positive any or must clause", {})

    all_clauses = [
        clause
        for placement in ("any", "must", "must_not")
        for clause in canonical_placements.get(placement, [])
    ]
    if len(all_clauses) > SEARCH_MAX_CLAUSES:
        _invalid(
            "search query exceeds the 32-clause limit",
            {"actualClauses": len(all_clauses), "maximumClauses": SEARCH_MAX_CLAUSES},
        )

    semantic_clauses = sum("semantic" in clause for clause in canonical_placements.get("any", []))
    if semantic_clauses > 1:
        _invalid("search query allows at most one semantic clause in any", {})

    total_clause_bytes = 0
    for clause in all_clauses:
        matcher, value = next(iter(clause.items()))
        value_bytes = len(value.encode("utf-8"))
        if value_bytes == 0:
            _invalid("search clause cannot be empty", {"matcher": matcher})
        if value_bytes > SEARCH_MAX_CLAUSE_BYTES:
            _invalid(
                "search clause exceeds the 1024-byte limit",
                {"matcher": matcher, "actualBytes": value_bytes},
            )
        if matcher == "literal" and not (
            SEARCH_MIN_LITERAL_BYTES <= value_bytes <= SEARCH_MAX_LITERAL_BYTES
        ):
            _invalid(
                "literal search clause must be between 3 and 256 bytes",
                {"actualBytes": value_bytes},
            )
        analyzed_tokens = _search_analyzed_token_count(value)
        if analyzed_tokens == 0:
            _invalid("search clause has no searchable tokens", {"matcher": matcher})
        if analyzed_tokens > SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE:
            _invalid(
                "search clause exceeds the 32 analyzed-token limit",
                {
                    "matcher": matcher,
                    "actualTokens": analyzed_tokens,
                    "maximumTokens": SEARCH_MAX_ANALYZED_TOKENS_PER_CLAUSE,
                },
            )
        total_clause_bytes += value_bytes

    if total_clause_bytes > SEARCH_MAX_TOTAL_CLAUSE_BYTES:
        _invalid(
            "search query exceeds the 8192-byte clause limit",
            {
                "actualBytes": total_clause_bytes,
                "maximumBytes": SEARCH_MAX_TOTAL_CLAUSE_BYTES,
            },
        )
    return cast(SearchQueryV1, canonical)


def validate_search_limit(limit: Optional[int]) -> None:
    if limit is None:
        return
    if (
        isinstance(limit, bool)
        or not isinstance(limit, int)
        or not 1 <= limit <= SEARCH_MAX_RESULTS
    ):
        _invalid(
            "search limit must be an integer between 1 and 200",
            {"limit": limit, "minimum": 1, "maximum": SEARCH_MAX_RESULTS},
        )


def _search_analyzed_token_count(value: str) -> int:
    count = 0
    in_token = False
    for char in value:
        continues_token = char.isalnum() or (in_token and _is_search_continuation_mark(char))
        if continues_token:
            if not in_token:
                count += 1
            in_token = True
        else:
            in_token = False
    return count


def _is_search_continuation_mark(char: str) -> bool:
    codepoint = ord(char)
    return (
        0x0300 <= codepoint <= 0x036F
        or 0x1AB0 <= codepoint <= 0x1AFF
        or 0x1DC0 <= codepoint <= 0x1DFF
        or 0x20D0 <= codepoint <= 0x20FF
        or 0xFE20 <= codepoint <= 0xFE2F
        or codepoint in {0x200C, 0x200D}
    )


def validate_search_intent(
    *,
    query: Optional[SearchQueryV1],
    file: Optional[str],
    limit: Optional[int],
) -> None:
    validate_search_limit(limit)
    if query is not None:
        validate_search_query(query)
        return
    if _has_text(file):
        return
    raise CtxAgentHistoryValidationError(
        "search requires a ctx-search-v1 query or file option",
        details={"query": query, "file": file},
    )


def _has_text(value: object) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _invalid(message: str, details: dict[str, object]) -> None:
    raise CtxAgentHistoryValidationError(message, details=details)
