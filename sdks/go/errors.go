package ctxagenthistory

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

// ErrorKind classifies SDK and adapter failures.
type ErrorKind string

const (
	ErrorKindInvalidArgument      ErrorKind = "invalid_request"
	ErrorKindNotFound             ErrorKind = "not_found"
	ErrorKindNotInitialized       ErrorKind = "not_initialized"
	ErrorKindCommandFailed        ErrorKind = "adapter_error"
	ErrorKindDecode               ErrorKind = "decode_error"
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
	Retryable bool
	Details   map[string]any
	Kind      ErrorKind
	Message   string
	Command   []string
	ExitCode  int
	Stdout    string
	Stderr    string
	Err       error
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
	var producer map[string]any
	var details map[string]any
	retryable := false
	decoder := json.NewDecoder(strings.NewReader(stderr))
	decoder.UseNumber()
	if rejectDuplicateJSONMembers([]byte(stderr)) == nil && decoder.Decode(&producer) == nil {
		code, codeOK := producer["error_code"].(string)
		retry, retryOK := producer["retryable"].(bool)
		_, hasRetry := producer["retryable"]
		if codeOK && code != "" && (!hasRetry || retryOK) {
			details = map[string]any{"producerError": producer}
			retryable = retry
		}
	}
	return &Error{
		Retryable: retryable, Details: details,
		Kind:     ErrorKindCommandFailed,
		Message:  message,
		Command:  append([]string(nil), command...),
		ExitCode: exitCode,
		Stdout:   stdout,
		Stderr:   stderr,
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
