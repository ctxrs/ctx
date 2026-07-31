"""Normalization helpers for the agent-history-v1 contract."""

from __future__ import annotations

from typing import Any, Mapping, Optional, cast

from .config import HostedConfig, LocalConfig
from .types import (
    Backend,
    EventResult,
    ImportResult,
    JsonObject,
    ProviderSource,
    SearchResult,
    SessionResult,
    Status,
)
from .version import API_VERSION

SCHEMA_VERSION = 1


def local_backend(config: LocalConfig, raw: Optional[Mapping[str, Any]] = None) -> Backend:
    data_root = str(config.data_root) if config.data_root is not None else None
    if data_root is None and raw is not None:
        data_root = raw.get("data_root") or raw.get("dataRoot")
    return cast(Backend, _drop_none({"kind": "local", "dataRoot": data_root}))


def hosted_backend(config: HostedConfig) -> Backend:
    return cast(Backend, _drop_none({"kind": "hosted", "baseUrl": config.base_url}))


def envelope(operation: str, backend: Mapping[str, Any], **payload: Any) -> JsonObject:
    result: JsonObject = {
        "contractVersion": API_VERSION,
        "schemaVersion": SCHEMA_VERSION,
        "operation": operation,
        "backend": dict(backend),
    }
    result.update(
        _drop_none(
            {
                key[:-1] if key.endswith("_") else key: value
                for key, value in payload.items()
            }
        )
    )
    return result


def normalize_status(raw: Mapping[str, Any]) -> Status:
    status = _camelize_public(raw)
    status.setdefault("initialized", False)
    status.setdefault("localOnly", True)
    status.setdefault("indexedItems", 0)
    status.setdefault("indexedSources", 0)
    status.setdefault("pendingCatalogSessions", 0)
    status.setdefault("failedCatalogSessions", 0)
    status.setdefault("staleCatalogSessions", 0)
    return cast(
        Status,
        _drop_none(
            {
                key: value
                for key, value in status.items()
            }
        ),
    )


def normalize_init(raw: Mapping[str, Any]) -> JsonObject:
    return cast(JsonObject, _camelize_public(raw))


def normalize_sources(raw: Mapping[str, Any]) -> list[ProviderSource]:
    return cast(
        list[ProviderSource],
        [_camelize_public(source) for source in raw.get("sources", [])],
    )


def normalize_import(raw: Mapping[str, Any]) -> ImportResult:
    return cast(
        ImportResult,
        _drop_none(
            {
                "resume": raw.get("resume", False),
                "resumeMode": raw.get("resume_mode", raw.get("resumeMode")),
                "totals": _camelize_public(raw.get("totals", {})),
                "sources": [_camelize_public(source) for source in raw.get("sources", [])],
            }
        ),
    )


def normalize_search(raw: Mapping[str, Any]) -> SearchResult:
    result_window = raw.get("result_window", raw.get("resultWindow"))
    pagination = raw.get("pagination")
    if pagination is None and isinstance(result_window, Mapping):
        pagination = _result_window_pagination(result_window)
    return cast(
        SearchResult,
        _drop_none(
            {
                "query": raw.get("query"),
                "filters": _camelize_public(raw.get("filters", {})),
                "freshness": _camelize_public(raw.get("freshness", {})),
                "retrieval": _camelize_public(raw.get("retrieval")),
                "generatedAt": raw.get("generated_at", raw.get("generatedAt")),
                "results": [_camelize_public(result) for result in raw.get("results", [])],
                "resultWindow": _camelize_public(result_window),
                "pagination": _camelize_public(pagination if pagination is not None else {}),
                "truncation": _camelize_public(raw.get("truncation", {})),
            }
        ),
    )


def _result_window_pagination(result_window: Mapping[str, Any]) -> JsonObject:
    pagination: JsonObject = {}
    if "limit" in result_window:
        pagination["limit"] = result_window["limit"]
    if "more_available" in result_window:
        pagination["hasMore"] = result_window["more_available"]
    elif "moreAvailable" in result_window:
        pagination["hasMore"] = result_window["moreAvailable"]
    return pagination


def normalize_event(raw: Mapping[str, Any]) -> EventResult:
    event = _camelize_public(raw.get("event"))
    events = [_camelize_public(item) for item in raw.get("events", [])]
    return cast(
        EventResult,
        _drop_none(
            {
                "event": event,
                "events": events,
            }
        ),
    )


def normalize_session(raw: Mapping[str, Any]) -> SessionResult:
    session = _camelize_public(raw.get("session", {}))
    if isinstance(session, dict):
        _copy_if_absent(session, "ctxSessionId", raw.get("ctx_session_id"))
        _copy_if_absent(session, "providerSessionId", raw.get("provider_session_id"))
    return cast(
        SessionResult,
        _drop_none(
            {
                "session": session,
                "events": [_camelize_public(item) for item in raw.get("events", [])],
                "mode": raw.get("mode"),
                "format": raw.get("format"),
            }
        ),
    )


def _camelize_public(value: Any) -> Any:
    if isinstance(value, list):
        return [_camelize_public(item) for item in value]
    if isinstance(value, dict):
        result: JsonObject = {}
        for key, nested in value.items():
            if key in {
                "schema_version",
                "target",
                "item_type",
                "itemType",
                "payload_type",
                "payloadType",
                "record_type",
                "recordType",
            }:
                continue
            result[_snake_to_camel(key)] = _camelize_public(nested)
        return _drop_none(result)
    return value


def _snake_to_camel(value: str) -> str:
    parts = value.split("_")
    if len(parts) == 1:
        return value
    return parts[0] + "".join(part[:1].upper() + part[1:] for part in parts[1:])


def _copy_if_absent(target: JsonObject, key: str, value: Any) -> None:
    if key not in target and value is not None:
        target[key] = value


def _drop_none(value: Mapping[str, Any]) -> JsonObject:
    return {key: nested for key, nested in value.items() if nested is not None}
