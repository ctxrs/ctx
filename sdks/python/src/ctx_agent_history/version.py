"""SDK and API version metadata."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

SDK_VERSION = "0.1.0a0"
API_VERSION = "agent-history-v2"


@dataclass(frozen=True)
class VersionInfo:
    sdk_version: str
    api_version: str
    transport: str
    ctx_version: Optional[str] = None

    def as_dict(self) -> dict[str, Optional[str]]:
        return {
            "sdk_version": self.sdk_version,
            "api_version": self.api_version,
            "transport": self.transport,
            "ctx_version": self.ctx_version,
        }
