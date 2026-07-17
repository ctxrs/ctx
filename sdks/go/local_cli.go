package ctxagenthistory

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"sync"
	"time"
	"unicode/utf8"
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
	if !utf8.Valid(result.Stdout) {
		return nil, invalidUTF8SDKError(append([]string{a.path}, args...), "stdout")
	}
	if !utf8.Valid(result.Stderr) {
		return nil, invalidUTF8SDKError(append([]string{a.path}, args...), "stderr")
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
	scope, err := newOwnedProcessScope(cmd)
	if err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}
	defer scope.Close()
	if len(env) > 0 {
		cmd.Env = append(cmd.Environ(), env...)
	}
	stdout := newBoundedCaptureWriter("stdout", localStdoutCapBytes)
	stderr := newBoundedCaptureWriter("stderr", localStderrCapBytes)
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	cmd.WaitDelay = localTeardownDelay
	if err := cmd.Start(); err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}
	waitResults := make(chan error, 1)
	go func() { waitResults <- cmd.Wait() }()
	if err := scope.AfterStart(cmd); err != nil {
		scope.Terminate(cmd)
		return commandResult{ExitCode: exitCode(boundedWait(waitResults, err)), Err: err}
	}

	var waitErr error
	select {
	case waitErr = <-waitResults:
	case overflow := <-stdout.Failures():
		scope.Terminate(cmd)
		waitErr = boundedWait(waitResults, overflow)
		return commandResult{ExitCode: exitCode(waitErr), Err: overflow}
	case overflow := <-stderr.Failures():
		scope.Terminate(cmd)
		waitErr = boundedWait(waitResults, overflow)
		return commandResult{ExitCode: exitCode(waitErr), Err: overflow}
	case <-ctx.Done():
		scope.Terminate(cmd)
		waitErr = boundedWait(waitResults, ctx.Err())
		return commandResult{ExitCode: exitCode(waitErr), Err: ctx.Err()}
	}
	if failure := stdout.Failure(); failure != nil {
		scope.Terminate(cmd)
		return commandResult{Stdout: stdout.Bytes(), Stderr: stderr.Bytes(), ExitCode: exitCode(waitErr), Err: failure}
	}
	if failure := stderr.Failure(); failure != nil {
		scope.Terminate(cmd)
		return commandResult{Stdout: stdout.Bytes(), Stderr: stderr.Bytes(), ExitCode: exitCode(waitErr), Err: failure}
	}
	if errors.Is(waitErr, exec.ErrWaitDelay) {
		scope.Terminate(cmd)
		failure := &captureFailureError{Stream: "pipe", Err: errors.New("descendant retained a CLI output pipe")}
		return commandResult{Stdout: stdout.Bytes(), Stderr: stderr.Bytes(), ExitCode: -1, Err: failure}
	}
	if waitErr != nil {
		scope.Terminate(cmd)
	}
	if !utf8.Valid(stdout.Bytes()) || !utf8.Valid(stderr.Bytes()) {
		scope.Terminate(cmd)
	}
	return commandResult{
		Stdout:   stdout.Bytes(),
		Stderr:   stderr.Bytes(),
		ExitCode: exitCode(waitErr),
		Err:      waitErr,
	}
}

func boundedWait(waitResults <-chan error, fallback error) error {
	select {
	case err := <-waitResults:
		if err != nil {
			return err
		}
	case <-time.After(localTeardownLimit):
	}
	return fallback
}

type boundedCaptureWriter struct {
	mu       sync.Mutex
	data     []byte
	stream   string
	capBytes int
	failure  error
	failures chan error
}

func newBoundedCaptureWriter(stream string, capBytes int) *boundedCaptureWriter {
	return &boundedCaptureWriter{
		data:     make([]byte, 0, capBytes),
		stream:   stream,
		capBytes: capBytes,
		failures: make(chan error, 1),
	}
}

func (w *boundedCaptureWriter) Write(data []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.failure != nil {
		return 0, w.failure
	}
	remaining := w.capBytes - len(w.data)
	if len(data) > remaining {
		if remaining > 0 {
			w.data = append(w.data, data[:remaining]...)
		}
		w.failure = &captureLimitError{Stream: w.stream, CapBytes: w.capBytes}
		select {
		case w.failures <- w.failure:
		default:
		}
		return 0, w.failure
	}
	w.data = append(w.data, data...)
	return len(data), nil
}

func (w *boundedCaptureWriter) Bytes() []byte {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.data
}

func (w *boundedCaptureWriter) Failure() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.failure
}

func (w *boundedCaptureWriter) Failures() <-chan error {
	return w.failures
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
