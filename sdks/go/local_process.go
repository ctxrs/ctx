package ctxagenthistory

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"time"
)

const (
	maxRetainedStdoutBytes  = 64 * 1024 * 1024
	maxRetainedStderrBytes  = 64 * 1024
	defaultTerminationGrace = 150 * time.Millisecond
)

type execCommandRunner struct {
	stdoutLimit      int
	stderrLimit      int
	terminationGrace time.Duration
}

type taskProcessTree interface {
	gracefulTerminate()
	forceTerminate()
	close()
}

type retainedPipe struct {
	bytes     []byte
	truncated bool
	err       error
}

type outputLimitError struct {
	maximum int
}

func (e outputLimitError) Error() string {
	return fmt.Sprintf("ctx command stdout exceeded %d bytes", e.maximum)
}

func (r execCommandRunner) Run(ctx context.Context, path string, args []string, env []string) commandResult {
	if ctx == nil {
		panic("nil Context")
	}
	if err := ctx.Err(); err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}
	stdoutLimit := positiveOr(r.stdoutLimit, maxRetainedStdoutBytes)
	stderrLimit := positiveOr(r.stderrLimit, maxRetainedStderrBytes)
	terminationGrace := r.terminationGrace
	if terminationGrace <= 0 {
		terminationGrace = defaultTerminationGrace
	}

	stdoutReader, stdoutWriter, err := os.Pipe()
	if err != nil {
		return commandResult{ExitCode: -1, Err: err}
	}
	stderrReader, stderrWriter, err := os.Pipe()
	if err != nil {
		_ = stdoutReader.Close()
		_ = stdoutWriter.Close()
		return commandResult{ExitCode: -1, Err: err}
	}

	cmd := exec.Command(path, args...)
	if len(env) > 0 {
		cmd.Env = append(cmd.Environ(), env...)
	}
	cmd.Stdout = stdoutWriter
	cmd.Stderr = stderrWriter
	tree, err := startTaskProcess(cmd)
	_ = stdoutWriter.Close()
	_ = stderrWriter.Close()
	if err != nil {
		_ = stdoutReader.Close()
		_ = stderrReader.Close()
		return commandResult{ExitCode: -1, Err: err}
	}
	defer tree.close()

	stdoutDone := make(chan retainedPipe, 1)
	stderrDone := make(chan retainedPipe, 1)
	go drainRetainedPipe(stdoutReader, stdoutLimit, stdoutDone)
	go drainRetainedPipe(stderrReader, stderrLimit, stderrDone)
	waitDone := make(chan error, 1)
	go func() { waitDone <- cmd.Wait() }()

	var stdout, stderr retainedPipe
	var waitErr, terminalErr error
	var stdoutPending, stderrPending, waitPending = true, true, true
	contextDone := ctx.Done()
	var graceTimer *time.Timer
	var graceDone <-chan time.Time
	startTermination := func(cause error) {
		if terminalErr != nil {
			return
		}
		terminalErr = cause
		tree.gracefulTerminate()
		graceTimer = time.NewTimer(terminationGrace)
		graceDone = graceTimer.C
		contextDone = nil
	}

	for stdoutPending || stderrPending || waitPending {
		select {
		case stdout = <-stdoutDone:
			stdoutPending = false
			stdoutDone = nil
			if stdout.truncated {
				startTermination(outputLimitError{maximum: stdoutLimit})
			} else if stdout.err != nil {
				startTermination(stdout.err)
			}
		case stderr = <-stderrDone:
			stderrPending = false
			stderrDone = nil
			if stderr.err != nil {
				startTermination(stderr.err)
			}
		case waitErr = <-waitDone:
			waitPending = false
			waitDone = nil
		case <-contextDone:
			startTermination(ctx.Err())
		case <-graceDone:
			tree.forceTerminate()
			graceDone = nil
		}
	}
	if graceTimer != nil {
		graceTimer.Stop()
	}
	if terminalErr == nil {
		terminalErr = waitErr
	}
	exitCode := 0
	if cmd.ProcessState != nil {
		exitCode = cmd.ProcessState.ExitCode()
	} else if terminalErr != nil {
		exitCode = -1
	}
	return commandResult{
		Stdout:   stdout.bytes,
		Stderr:   stderr.bytes,
		ExitCode: exitCode,
		Err:      terminalErr,
	}
}

func drainRetainedPipe(
	pipe *os.File,
	maximum int,
	done chan<- retainedPipe,
) {
	defer pipe.Close()
	result := retainedPipe{bytes: make([]byte, 0, min(maximum, 8*1024))}
	buffer := make([]byte, 8*1024)
	for {
		read, err := pipe.Read(buffer)
		if read > 0 {
			retained := min(read, maximum-len(result.bytes))
			result.bytes = append(result.bytes, buffer[:retained]...)
			if retained < read && !result.truncated {
				result.truncated = true
			}
		}
		if err != nil {
			if !errors.Is(err, io.EOF) {
				result.err = err
			}
			done <- result
			return
		}
	}
}

func positiveOr(value, fallback int) int {
	if value > 0 {
		return value
	}
	return fallback
}
