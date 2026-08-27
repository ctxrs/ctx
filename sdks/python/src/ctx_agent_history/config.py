"""SDK configuration objects."""

from __future__ import annotations

import warnings
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Optional


_HOSTED_DEPRECATION_MESSAGE = (
    "Hosted SDK placeholders are deprecated and will be removed in the next "
    "breaking SDK revision; hosted operations remain unsupported."
)


@dataclass(frozen=True)
class LocalConfig:
    """Configuration for the local CLI adapter."""

    ctx_binary: str = "ctx"
    data_root: Optional[Path] = None
    env: Optional[Mapping[str, str]] = None
    cwd: Optional[Path] = None
    timeout: Optional[float] = None


@dataclass(frozen=True)
class HostedConfig:
    """Deprecated placeholder configuration for a hosted agent-history-v1 transport.

    Hosted SDK placeholders are deprecated and will be removed in the next
    breaking SDK revision; hosted operations remain unsupported.
    """

    base_url: str
    api_key: Optional[str] = None
    timeout: Optional[float] = None

    def __post_init__(self) -> None:
        warnings.warn(
            _HOSTED_DEPRECATION_MESSAGE,
            DeprecationWarning,
            stacklevel=3,
        )
