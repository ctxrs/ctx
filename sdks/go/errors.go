package ctxagenthistory

import (
	"errors"
	"fmt"
)

// ErrorKind classifies SDK and adapter failures.
type ErrorKind string

const (
	ErrorKindInvalidArgument      ErrorKind = "invalid_request"
	ErrorKindNotFound             ErrorKind = "not_found"
	ErrorKindNotInitialized       ErrorKind = "not_initialized"
	ErrorKindCommandFailed        ErrorKind = "adapter_error"
	ErrorKindDecode               ErrorKind = "decode_error"
	ErrorKindCaptureLimit         ErrorKind = "capture_limit"
	ErrorKindCaptureFailure       ErrorKind = "capture_failure"
	ErrorKindTimeout              ErrorKind = "timeout"
	ErrorKindCancelled            ErrorKind = "cancelled"
	ErrorKindUnavailable          ErrorKind = "backend_unavailable"
	ErrorKindHostedNotImplemented ErrorKind = "not_supported"
	ErrorKindUnknown              ErrorKind = "unknown"
	ErrorKindUnsupportedSchema    ErrorKind = "unsupported_schema"
	ErrorKindTransportUnavailable ErrorKind = "transport_unavailable"
)

// Error is the structured error returned by the SDK.
type Error struct {
	Kind     ErrorKind
	Message  string
	Command  []string
	ExitCode int
	Stdout   string
	Stderr   string
	Stream   string
	CapBytes int
	Err      error
}

func (e *Error) Error() string {
	if e == nil {
		return "<nil>"
	}
	if e.Message != "" {
		return e.Message
	}
	if e.Err != nil {
		return e.Err.Error()
	}
	return string(e.Kind)
}

func (e *Error) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.Err
}

func (e *AgentHistoryError) Error() string {
	if e == nil {
		return "<nil>"
	}
	return e.Message
}

// IsErrorKind reports whether err or any wrapped error is an SDK Error with kind.
func IsErrorKind(err error, kind ErrorKind) bool {
	var sdkErr *Error
	if errors.As(err, &sdkErr) {
		return sdkErr.Kind == kind
	}
	return false
}

func sdkError(kind ErrorKind, message string, err error) *Error {
	return &Error{Kind: kind, Message: message, Err: err}
}

func commandError(command []string, exitCode int, stdout, stderr string, err error) *Error {
	message := "ctx command failed"
	if stderr != "" {
		message = fmt.Sprintf("%s: %s", message, firstLine(stderr))
	} else if err != nil {
		message = fmt.Sprintf("%s: %s", message, err.Error())
	}
	return &Error{
		Kind:     ErrorKindCommandFailed,
		Message:  message,
		Command:  append([]string(nil), command...),
		ExitCode: exitCode,
		Stdout:   stdout,
		Stderr:   stderr,
		Err:      err,
	}
}

func captureLimitSDKError(command []string, stream string, capBytes int) *Error {
	return &Error{
		Kind:     ErrorKindCaptureLimit,
		Message:  fmt.Sprintf("ctx CLI %s exceeded its %d-byte capture limit", stream, capBytes),
		Command:  append([]string(nil), command...),
		ExitCode: -1,
		Stream:   stream,
		CapBytes: capBytes,
	}
}

func captureFailureSDKError(command []string, stream string, err error) *Error {
	return &Error{
		Kind:     ErrorKindCaptureFailure,
		Message:  fmt.Sprintf("ctx CLI %s capture failed", stream),
		Command:  append([]string(nil), command...),
		ExitCode: -1,
		Stream:   stream,
		Err:      err,
	}
}

func firstLine(value string) string {
	for i, r := range value {
		if r == '\n' || r == '\r' {
			return value[:i]
		}
	}
	return value
}
