"""Structured SDK errors."""

from __future__ import annotations

from typing import Any, Mapping, Optional, Sequence


class CtxAgentHistoryError(Exception):
    """Base class for ctx agent history SDK errors."""

    def __init__(
        self,
        message: str,
        *,
        code: str = "unknown",
        details: Optional[Mapping[str, Any]] = None,
        retryable: bool = False,
        cause: Optional[BaseException] = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.code = code
        self.details = dict(details or {})
        self.retryable = retryable
        self.cause = cause

    def as_dict(self) -> dict[str, Any]:
        return {
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
            "details": self.details,
            "cause": str(self.cause) if self.cause is not None else None,
        }


class CtxAgentHistoryCliError(CtxAgentHistoryError):
    """Raised when the local ctx CLI exits unsuccessfully."""

    def __init__(
        self,
        message: str,
        *,
        command: Sequence[str],
        exit_code: int,
        stderr: str,
        stdout: str = "",
        cause: Optional[BaseException] = None,
    ) -> None:
        self.command = list(command)
        self.exit_code = exit_code
        self.stderr = stderr
        self.stdout = stdout
        super().__init__(
            message,
            code="adapter_error",
            details={
                "command": self.command,
                "exit_code": exit_code,
                "stderr": stderr,
                "stdout": stdout,
            },
            retryable=False,
            cause=cause,
        )


class CtxAgentHistoryProtocolError(CtxAgentHistoryError):
    """Raised when ctx does not return the expected agent-history-v1 JSON."""

    def __init__(
        self,
        message: str,
        *,
        details: Optional[Mapping[str, Any]] = None,
        cause: Optional[BaseException] = None,
    ) -> None:
        super().__init__(
            message,
            code="decode_error",
            details=details,
            retryable=False,
            cause=cause,
        )


class CtxAgentHistoryValidationError(CtxAgentHistoryError):
    """Raised before invoking ctx for invalid SDK input."""

    def __init__(
        self,
        message: str,
        *,
        details: Optional[Mapping[str, Any]] = None,
        cause: Optional[BaseException] = None,
    ) -> None:
        super().__init__(
            message,
            code="invalid_request",
            details=details,
            retryable=False,
            cause=cause,
        )


class CtxAgentHistoryTimeoutError(CtxAgentHistoryError):
    """Raised when the local ctx CLI exceeds the configured timeout."""

    def __init__(
        self,
        message: str,
        *,
        details: Optional[Mapping[str, Any]] = None,
        cause: Optional[BaseException] = None,
    ) -> None:
        super().__init__(
            message,
            code="timeout",
            details=details,
            retryable=True,
            cause=cause,
        )


class HostedTransportNotImplementedError(CtxAgentHistoryError):
    """Raised by the hosted placeholder transport."""

    def __init__(self, method: str) -> None:
        super().__init__(
            "hosted ctx agent history backend is not available in this in-repo SDK",
            code="not_supported",
            details={"backend": "hosted", "method": method},
            retryable=False,
        )
