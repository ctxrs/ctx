package cli

import "errors"

type ErrorCode string

const (
	CodeUsage         ErrorCode = "usage"
	CodeUnimplemented ErrorCode = "unimplemented"
)

type Error struct {
	Code    ErrorCode
	Command string
	Message string
	Err     error
}

func (e *Error) Error() string {
	if e == nil {
		return ""
	}
	if e.Command == "" {
		return e.Message
	}
	return e.Command + ": " + e.Message
}

func (e *Error) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.Err
}

func ExitCode(err error) int {
	if err == nil {
		return 0
	}
	var cliErr *Error
	if errors.As(err, &cliErr) && cliErr.Code == CodeUsage {
		return 2
	}
	return 1
}
