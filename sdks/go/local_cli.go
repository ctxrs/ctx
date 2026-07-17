package ctxagenthistory

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"time"
)

const (
	localStdoutCapBytes  = 2 * 1024 * 1024
	localStderrCapBytes  = 256 * 1024
	localReadBufferBytes = 64 * 1024
	localTeardownDelay   = 100 * time.Millisecond
	localTeardownLimit   = time.Second
)

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
	result := a.runner.Run(ctx, a.path, args, env)
	if len(result.Stdout) > localStdoutCapBytes {
		return nil, captureLimitSDKError(append([]string{a.path}, args...), "stdout", localStdoutCapBytes)
	}
	if len(result.Stderr) > localStderrCapBytes {
		return nil, captureLimitSDKError(append([]string{a.path}, args...), "stderr", localStderrCapBytes)
	}
	if result.Err != nil {
		var captureError *captureLimitError
		if errors.As(result.Err, &captureError) {
			return nil, captureLimitSDKError(
				append([]string{a.path}, args...),
				captureError.Stream,
				captureError.CapBytes,
			)
		}
		var captureFailure *captureFailureError
		if errors.As(result.Err, &captureFailure) {
			return nil, captureFailureSDKError(
				append([]string{a.path}, args...),
				captureFailure.Stream,
				captureFailure.Err,
			)
		}
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
	return stdout, nil
}

type execCommandRunner struct{}

func (execCommandRunner) Run(ctx context.Context, path string, args []string, env []string) commandResult {
	if err := ctx.Err(); err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}
	cmd := exec.Command(path, args...)
	configureProcessScope(cmd)
	if len(env) > 0 {
		cmd.Env = append(cmd.Environ(), env...)
	}
	stdoutPipe, err := cmd.StdoutPipe()
	if err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}
	stderrPipe, err := cmd.StderrPipe()
	if err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}
	if err := cmd.Start(); err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}

	stdoutResults := make(chan captureResult, 1)
	stderrResults := make(chan captureResult, 1)
	waitResults := make(chan error, 1)
	go func() { stdoutResults <- readBoundedPipe(stdoutPipe, "stdout", localStdoutCapBytes) }()
	go func() { stderrResults <- readBoundedPipe(stderrPipe, "stderr", localStderrCapBytes) }()
	go func() { waitResults <- cmd.Wait() }()

	var stdout, stderr captureResult
	var stdoutDone, stderrDone, waitDone bool
	var waitErr error
	var drainDeadline <-chan time.Time
	for !stdoutDone || !stderrDone || !waitDone {
		select {
		case stdout = <-stdoutResults:
			stdoutDone = true
			stdoutResults = nil
			if stdout.Err != nil {
				waitErr = abortCommand(cmd, stdoutPipe, stderrPipe, waitResults, waitDone, stdout.Err)
				return commandResult{Stdout: stdout.Data, Stderr: stderr.Data, ExitCode: exitCode(waitErr), Err: stdout.Err}
			}
		case stderr = <-stderrResults:
			stderrDone = true
			stderrResults = nil
			if stderr.Err != nil {
				waitErr = abortCommand(cmd, stdoutPipe, stderrPipe, waitResults, waitDone, stderr.Err)
				return commandResult{Stdout: stdout.Data, Stderr: stderr.Data, ExitCode: exitCode(waitErr), Err: stderr.Err}
			}
		case waitErr = <-waitResults:
			waitDone = true
			waitResults = nil
			if !stdoutDone || !stderrDone {
				drainDeadline = time.After(localTeardownDelay)
			}
		case <-drainDeadline:
			failure := &captureFailureError{Stream: "pipe", Err: errors.New("descendant retained a CLI output pipe")}
			_ = abortCommand(cmd, stdoutPipe, stderrPipe, waitResults, waitDone, failure)
			return commandResult{Stdout: stdout.Data, Stderr: stderr.Data, ExitCode: exitCode(waitErr), Err: failure}
		case <-ctx.Done():
			waitErr = abortCommand(cmd, stdoutPipe, stderrPipe, waitResults, waitDone, ctx.Err())
			return commandResult{Stdout: stdout.Data, Stderr: stderr.Data, ExitCode: exitCode(waitErr), Err: ctx.Err()}
		}
		if waitDone && stdoutDone && stderrDone {
			drainDeadline = nil
		}
	}

	resultExitCode := 0
	if waitErr != nil {
		resultExitCode = exitCode(waitErr)
	}
	return commandResult{
		Stdout:   stdout.Data,
		Stderr:   stderr.Data,
		ExitCode: resultExitCode,
		Err:      waitErr,
	}
}

type captureResult struct {
	Data []byte
	Err  error
}

func readBoundedPipe(stream io.Reader, name string, capBytes int) captureResult {
	var output bytes.Buffer
	output.Grow(min(capBytes, localReadBufferBytes))
	buffer := make([]byte, localReadBufferBytes)
	for {
		read, err := stream.Read(buffer)
		if read > 0 {
			remaining := capBytes - output.Len()
			if read > remaining {
				if remaining > 0 {
					_, _ = output.Write(buffer[:remaining])
				}
				return captureResult{Data: output.Bytes(), Err: &captureLimitError{Stream: name, CapBytes: capBytes}}
			}
			_, _ = output.Write(buffer[:read])
		}
		if errors.Is(err, io.EOF) {
			return captureResult{Data: output.Bytes()}
		}
		if err != nil {
			return captureResult{Data: output.Bytes(), Err: &captureFailureError{Stream: name, Err: err}}
		}
	}
}

func abortCommand(
	cmd *exec.Cmd,
	stdout io.Closer,
	stderr io.Closer,
	waitResults <-chan error,
	waitDone bool,
	fallback error,
) error {
	terminateProcessScope(cmd)
	_ = stdout.Close()
	_ = stderr.Close()
	if waitDone || waitResults == nil {
		return fallback
	}
	select {
	case err := <-waitResults:
		if err != nil {
			return err
		}
	case <-time.After(localTeardownLimit):
	}
	return fallback
}

func exitCode(err error) int {
	if err == nil {
		return 0
	}
	var exitError *exec.ExitError
	if errors.As(err, &exitError) {
		return exitError.ExitCode()
	}
	return -1
}

type captureLimitError struct {
	Stream   string
	CapBytes int
}

func (e *captureLimitError) Error() string {
	return fmt.Sprintf("ctx CLI %s exceeded %d bytes", e.Stream, e.CapBytes)
}

type captureFailureError struct {
	Stream string
	Err    error
}

func (e *captureFailureError) Error() string {
	return fmt.Sprintf("ctx CLI %s capture failed: %v", e.Stream, e.Err)
}

func (e *captureFailureError) Unwrap() error {
	return e.Err
}
