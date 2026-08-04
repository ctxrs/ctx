package ctxagenthistory

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"
)

const (
	processHelperModeEnv = "CTX_GO_SDK_PROCESS_HELPER_MODE"
	drainedMarkerEnv     = "CTX_GO_SDK_DRAINED_MARKER"
	descendantPIDEnv     = "CTX_GO_SDK_DESCENDANT_PID"
	rootPIDEnv           = "CTX_GO_SDK_ROOT_PID"
	termMarkerEnv        = "CTX_GO_SDK_TERM_MARKER"
)

func TestExecCommandRunnerBoundsStdoutWhileContinuingToDrain(t *testing.T) {
	temp := t.TempDir()
	drained := filepath.Join(temp, "drained")
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	result := (execCommandRunner{stdoutLimit: 1024}).Run(
		ctx,
		os.Args[0],
		[]string{"-test.run=^TestLocalProcessHelper$"},
		[]string{processHelperModeEnv + "=stdout", drainedMarkerEnv + "=" + drained},
	)
	var limitErr outputLimitError
	if !errors.As(result.Err, &limitErr) || limitErr.maximum != 1024 {
		t.Fatalf("expected 1024-byte stdout limit error, got %v", result.Err)
	}
	if len(result.Stdout) != 1024 || string(result.Stdout) != strings.Repeat("x", 1024) {
		t.Fatalf("retained stdout was not the bounded prefix: %d bytes", len(result.Stdout))
	}
	if body, err := os.ReadFile(drained); err != nil || string(body) != "done" {
		t.Fatalf("child did not finish after the stdout limit was crossed: %q, %v", body, err)
	}
}

func TestExecCommandRunnerBoundsStderrWithoutChangingSuccess(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	result := (execCommandRunner{stderrLimit: 1024}).Run(
		ctx,
		os.Args[0],
		[]string{"-test.run=^TestLocalProcessHelper$"},
		[]string{processHelperModeEnv + "=stderr"},
	)
	if result.Err != nil {
		t.Fatalf("large stderr changed successful execution: %v", result.Err)
	}
	if len(result.Stderr) != 1024 || string(result.Stderr) != strings.Repeat("e", 1024) {
		t.Fatalf("retained stderr was not the bounded prefix: %d bytes", len(result.Stderr))
	}
	if string(result.Stdout) != `{"initialized":true,"local_only":true}` {
		t.Fatalf("unexpected stdout: %q", result.Stdout)
	}
}

func TestLocalCLIAdapterDrainsMaximumValidMCPAttribution(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	adapter := NewLocalCLIAdapter(
		WithCLIPath(os.Args[0]),
		WithEnv([]string{processHelperModeEnv + "=attribution"}),
	)
	stdout, err := adapter.Do(ctx, Operation{
		Name: "showEvent",
		Args: []string{"-test.run=^TestLocalProcessHelper$"},
	})
	if err != nil {
		t.Fatalf("maximum valid attribution failed: %v", err)
	}
	var payload struct {
		Event struct {
			MCPToolCall struct {
				Server string `json:"server"`
				Tool   string `json:"tool"`
			} `json:"mcp_tool_call"`
		} `json:"event"`
	}
	if err := json.Unmarshal(stdout, &payload); err != nil {
		t.Fatalf("decode maximum attribution: %v", err)
	}
	if len(payload.Event.MCPToolCall.Server) != 65536 || len(payload.Event.MCPToolCall.Tool) != 65536 {
		t.Fatalf(
			"maximum attribution lengths = %d/%d",
			len(payload.Event.MCPToolCall.Server),
			len(payload.Event.MCPToolCall.Tool),
		)
	}
}

func TestLocalCLIAdapterInheritedPipeDeadlineKillsIgnoredTermTree(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("Linux /proc assertions provide deterministic complete-tree proof")
	}
	temp := t.TempDir()
	rootPID := filepath.Join(temp, "root.pid")
	descendantPID := filepath.Join(temp, "descendant.pid")
	termMarker := filepath.Join(temp, "term-observed")
	adapter := NewLocalCLIAdapter(
		WithCLIPath(os.Args[0]),
		WithEnv([]string{
			processHelperModeEnv + "=tree",
			rootPIDEnv + "=" + rootPID,
			descendantPIDEnv + "=" + descendantPID,
			termMarkerEnv + "=" + termMarker,
		}),
	)
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	started := time.Now()
	_, err := adapter.Do(ctx, Operation{
		Name: "status",
		Args: []string{"-test.run=^TestLocalProcessHelper$"},
	})
	if !IsErrorKind(err, ErrorKindTimeout) || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected wrapped timeout, got %v", err)
	}
	if elapsed := time.Since(started); elapsed >= 2*time.Second {
		t.Fatalf("tree cleanup exceeded its bounded grace: %s", elapsed)
	}
	if body, readErr := os.ReadFile(termMarker); readErr != nil || string(body) != "term" {
		t.Fatalf("descendant did not observe graceful TERM before force-kill: %q, %v", body, readErr)
	}
	for _, path := range []string{rootPID, descendantPID} {
		pid, readErr := os.ReadFile(path)
		if readErr != nil {
			t.Fatalf("read process witness %s: %v", path, readErr)
		}
		assertProcessGone(t, string(pid))
	}
}

func assertProcessGone(t *testing.T, pid string) {
	t.Helper()
	process := filepath.Join("/proc", pid)
	deadline := time.Now().Add(time.Second)
	for {
		_, err := os.Stat(process)
		if os.IsNotExist(err) {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed-out process %s remains after tree cleanup: %v", pid, err)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func TestLocalProcessHelper(t *testing.T) {
	mode := os.Getenv(processHelperModeEnv)
	if mode == "" {
		return
	}
	switch mode {
	case "stdout":
		_, _ = os.Stdout.WriteString(strings.Repeat("x", 64*1024))
		if err := os.WriteFile(os.Getenv(drainedMarkerEnv), []byte("done"), 0o600); err != nil {
			os.Exit(2)
		}
	case "stderr":
		_, _ = os.Stderr.WriteString(strings.Repeat("e", 64*1024))
		_, _ = os.Stdout.WriteString(`{"initialized":true,"local_only":true}`)
	case "attribution":
		component := strings.Repeat("x", 65536)
		_, _ = fmt.Fprintf(
			os.Stdout,
			`{"event":{"mcp_tool_call":{"server":"%s","tool":"%s"}},"events":[]}`,
			component,
			component,
		)
	case "tree":
		if err := os.WriteFile(os.Getenv(rootPIDEnv), []byte(strconv.Itoa(os.Getpid())), 0o600); err != nil {
			os.Exit(2)
		}
		descendant := exec.Command(
			"/bin/sh",
			"-c",
			`trap 'printf term > "$TERM_MARKER"' TERM; printf '%s' "$$" > "$DESCENDANT_PID"; while :; do sleep 1; done`,
		)
		descendant.Env = append(
			os.Environ(),
			"DESCENDANT_PID="+os.Getenv(descendantPIDEnv),
			"TERM_MARKER="+os.Getenv(termMarkerEnv),
		)
		descendant.Stdout = os.Stdout
		descendant.Stderr = os.Stderr
		if err := descendant.Start(); err != nil {
			os.Exit(2)
		}
		deadline := time.Now().Add(time.Second)
		for {
			if info, err := os.Stat(os.Getenv(descendantPIDEnv)); err == nil && info.Size() > 0 {
				break
			}
			if time.Now().After(deadline) {
				os.Exit(2)
			}
			time.Sleep(time.Millisecond)
		}
		_, _ = os.Stdout.WriteString(`{"initialized":true,"local_only":true}`)
	default:
		os.Exit(2)
	}
	os.Exit(0)
}
