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
SEARCH_MIN_LITERAL_BYTES = 3
SEARCH_MAX_LITERAL_BYTES = 256

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
    total_clauses = 0
    total_clause_bytes = 0
    semantic_clauses = 0
    positive_clauses = 0
    for placement in ("any", "must", "must_not"):
        raw_clauses = query.get(placement, [])
        if not isinstance(raw_clauses, list):
            _invalid(f"search query {placement} must be an array", {"placement": placement})
        clauses: list[dict[str, str]] = []
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
            if not isinstance(value, str) or not value.strip():
                _invalid(
                    "search clause value must be a non-empty string",
                    {"placement": placement, "matcher": matcher},
                )
            value_bytes = len(value.encode("utf-8"))
            if value_bytes > SEARCH_MAX_CLAUSE_BYTES:
                _invalid(
                    "search clause exceeds the 1024-byte limit",
                    {"matcher": matcher, "actualBytes": value_bytes},
                )
            if matcher == "literal" and not SEARCH_MIN_LITERAL_BYTES <= value_bytes <= SEARCH_MAX_LITERAL_BYTES:
                _invalid(
                    "literal search clause must be between 3 and 256 bytes",
                    {"actualBytes": value_bytes},
                )
            if matcher == "semantic":
                semantic_clauses += 1
            clauses.append({matcher: value})
            total_clauses += 1
            total_clause_bytes += value_bytes
            if total_clauses > SEARCH_MAX_CLAUSES:
                _invalid(
                    "search query exceeds the 32-clause limit",
                    {"actualClauses": total_clauses, "maximumClauses": SEARCH_MAX_CLAUSES},
                )
            if total_clause_bytes > SEARCH_MAX_TOTAL_CLAUSE_BYTES:
                _invalid(
                    "search query exceeds the 8192-byte clause limit",
                    {
                        "actualBytes": total_clause_bytes,
                        "maximumBytes": SEARCH_MAX_TOTAL_CLAUSE_BYTES,
                    },
                )
            if semantic_clauses > 1:
                _invalid("search query allows at most one semantic clause in any", {})
        if placement in query or clauses:
            canonical[placement] = clauses
        if placement in {"any", "must"}:
            positive_clauses += len(clauses)

    if positive_clauses == 0:
        _invalid("search query needs a positive any or must clause", {})
    return cast(SearchQueryV1, canonical)


def validate_search_intent(
    *,
    query: Optional[SearchQueryV1],
    file: Optional[str],
) -> None:
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
