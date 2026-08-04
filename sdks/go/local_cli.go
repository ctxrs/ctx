package ctxagenthistory

import (
	"bytes"
	"context"
	"errors"
	"os/exec"
	"unicode/utf8"
)

const analyticsEnabledEnvironment = "CTX_ANALYTICS_ENABLED"

// LocalCLIAdapter executes agent-history-v1 operations through the local ctx binary.
type LocalCLIAdapter struct {
	path     string
	dataRoot string
	env      []string
	runner   commandRunner
}

type commandRunner interface {
	Run(ctx context.Context, path string, args []string, env []string) commandResult
}

type commandResult struct {
	Stdout   []byte
	Stderr   []byte
	ExitCode int
	Err      error
}

// LocalCLIOption configures a LocalCLIAdapter.
type LocalCLIOption func(*LocalCLIAdapter)

// WithCLIPath sets the ctx executable path. The default is "ctx".
func WithCLIPath(path string) LocalCLIOption {
	return func(adapter *LocalCLIAdapter) {
		adapter.path = path
	}
}

// WithDataRoot sets CTX_DATA_ROOT for local CLI commands.
func WithDataRoot(dataRoot string) LocalCLIOption {
	return func(adapter *LocalCLIAdapter) {
		adapter.dataRoot = dataRoot
	}
}

// WithEnv appends environment entries for local CLI commands.
func WithEnv(env []string) LocalCLIOption {
	return func(adapter *LocalCLIAdapter) {
		adapter.env = append(adapter.env, env...)
	}
}

// NewLocalCLIAdapter creates a local CLI transport.
func NewLocalCLIAdapter(options ...LocalCLIOption) *LocalCLIAdapter {
	adapter := &LocalCLIAdapter{
		path:   "ctx",
		runner: execCommandRunner{},
	}
	for _, option := range options {
		option(adapter)
	}
	return adapter
}

func (a *LocalCLIAdapter) Do(ctx context.Context, op Operation) ([]byte, error) {
	if a.path == "" {
		return nil, sdkError(ErrorKindInvalidArgument, "local ctx CLI path is empty", nil)
	}
	args := append([]string(nil), op.Args...)
	env := append([]string(nil), a.env...)
	if a.dataRoot != "" {
		env = append(env, "CTX_DATA_ROOT="+a.dataRoot)
	}
	env = append(env, analyticsEnabledEnvironment+"=false")
	result := a.runner.Run(ctx, a.path, args, env)
	if result.Err != nil {
		kind := ErrorKindCommandFailed
		if errors.Is(result.Err, context.DeadlineExceeded) {
			kind = ErrorKindTimeout
		} else if errors.Is(result.Err, context.Canceled) {
			kind = ErrorKindCancelled
		} else if errors.Is(result.Err, exec.ErrNotFound) {
			kind = ErrorKindUnavailable
		}
		err := commandError(append([]string{a.path}, args...), result.ExitCode, string(result.Stdout), string(result.Stderr), result.Err)
		err.Kind = kind
		return nil, err
	}
	stdout := bytes.TrimSpace(result.Stdout)
	if len(stdout) == 0 {
		return nil, sdkError(ErrorKindDecode, "ctx command returned empty stdout", nil)
	}
	if !utf8.Valid(stdout) {
		return nil, sdkError(ErrorKindDecode, "ctx command returned invalid UTF-8 on stdout", nil)
	}
	return stdout, nil
}
