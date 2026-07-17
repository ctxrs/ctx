package cli

import (
	"bytes"
	"context"
	"errors"
	"strings"
	"testing"
)

func TestRootHelpListsPublicCommands(t *testing.T) {
	var out, stderr bytes.Buffer
	app := NewApp(&out, &stderr, Dependencies{})

	if err := app.Run(context.Background(), []string{"--help"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}

	got := out.String()
	for _, name := range []string{
		"setup", "status", "index", "sources", "import", "search", "show", "locate",
		"sql", "docs", "integrations", "daemon", "doctor", "mcp", "upgrade", "version",
	} {
		if !strings.Contains(got, name) {
			t.Fatalf("root help missing %q\nhelp:\n%s", name, got)
		}
	}
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr, got %q", stderr.String())
	}
}

func TestVersionCommand(t *testing.T) {
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{Version: "v0.0.0-test"})

	if err := app.Run(context.Background(), []string{"version"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}
	if got, want := out.String(), "ctx v0.0.0-test\n"; got != want {
		t.Fatalf("version output = %q, want %q", got, want)
	}
}

func TestVersionHelp(t *testing.T) {
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{})

	if err := app.Run(context.Background(), []string{"help", "version"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}
	if got := out.String(); !strings.Contains(got, "ctx version") {
		t.Fatalf("version help missing usage, got %q", got)
	}
}

func TestCommandHelp(t *testing.T) {
	var out bytes.Buffer
	app := NewApp(&out, &bytes.Buffer{}, Dependencies{})

	if err := app.Run(context.Background(), []string{"search", "--help"}); err != nil {
		t.Fatalf("Run returned error: %v", err)
	}

	got := out.String()
	for _, want := range []string{"Usage:", "ctx search", "lexical|semantic|hybrid", "must not silently fall back"} {
		if !strings.Contains(got, want) {
			t.Fatalf("search help missing %q\nhelp:\n%s", want, got)
		}
	}
}

func TestUnknownCommandReturnsUsageError(t *testing.T) {
	app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{})

	err := app.Run(context.Background(), []string{"cloud"})
	var cliErr *Error
	if !errors.As(err, &cliErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if cliErr.Code != CodeUsage || cliErr.Command != "cloud" {
		t.Fatalf("error = %#v, want usage error for cloud", cliErr)
	}
	if ExitCode(err) != 2 {
		t.Fatalf("ExitCode = %d, want 2", ExitCode(err))
	}
	if !strings.Contains(err.Error(), "unknown command") {
		t.Fatalf("error should be user-readable, got %q", err.Error())
	}
}

func TestUnfinishedCommandsReturnTypedErrors(t *testing.T) {
	for _, name := range commandNames() {
		t.Run(name, func(t *testing.T) {
			app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{})
			err := app.Run(context.Background(), []string{name})
			var cliErr *Error
			if !errors.As(err, &cliErr) {
				t.Fatalf("error type = %T, want *Error", err)
			}
			if cliErr.Code != CodeUnimplemented || cliErr.Command != name {
				t.Fatalf("error = %#v, want unimplemented error for %s", cliErr, name)
			}
			if !strings.Contains(err.Error(), "not implemented") {
				t.Fatalf("error should explain implementation status, got %q", err.Error())
			}
		})
	}
}

func TestCommandInventoryIsStable(t *testing.T) {
	got := commandList()
	want := "setup, status, index, sources, import, search, show, locate, sql, docs, integrations, daemon, doctor, mcp, upgrade"
	if got != want {
		t.Fatalf("commandList() = %q, want %q", got, want)
	}
}

func TestJSONFlagDoesNotPrintErrorPayloadToStdout(t *testing.T) {
	var out, stderr bytes.Buffer
	app := NewApp(&out, &stderr, Dependencies{})

	err := app.Run(context.Background(), []string{"status", "--json"})
	if err == nil {
		t.Fatal("Run returned nil, want unimplemented error")
	}
	if out.Len() != 0 {
		t.Fatalf("--json error wrote stdout %q, want no stdout", out.String())
	}
}

func TestUnsupportedProviderFailsClearly(t *testing.T) {
	app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{})

	err := app.Run(context.Background(), []string{"import", "--provider", "claude"})
	var cliErr *Error
	if !errors.As(err, &cliErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if cliErr.Code != CodeUsage {
		t.Fatalf("error code = %s, want %s", cliErr.Code, CodeUsage)
	}
	if !strings.Contains(err.Error(), "unsupported provider") || !strings.Contains(err.Error(), "codex and pi") {
		t.Fatalf("error should name supported providers, got %q", err.Error())
	}
}

func TestMissingProviderValueFailsClearly(t *testing.T) {
	app := NewApp(&bytes.Buffer{}, &bytes.Buffer{}, Dependencies{})

	err := app.Run(context.Background(), []string{"sources", "--provider"})
	var cliErr *Error
	if !errors.As(err, &cliErr) {
		t.Fatalf("error type = %T, want *Error", err)
	}
	if cliErr.Code != CodeUsage {
		t.Fatalf("error code = %s, want %s", cliErr.Code, CodeUsage)
	}
	if !strings.Contains(err.Error(), "missing value for --provider") {
		t.Fatalf("error should name missing provider value, got %q", err.Error())
	}
}
