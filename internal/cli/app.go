package cli

import (
	"context"
	"fmt"
	"io"
	"sort"
	"strings"

	"github.com/ctxrs/ctx/internal/capture"
	"github.com/ctxrs/ctx/internal/search"
	"github.com/ctxrs/ctx/internal/store"
)

type Dependencies struct {
	Store    store.Store
	Capturer capture.Capturer
	Searcher search.Engine
	Version  string
	DataRoot string
}

type App struct {
	out  io.Writer
	err  io.Writer
	deps Dependencies
}

func NewApp(out, err io.Writer, deps Dependencies) *App {
	if deps.Version == "" {
		deps.Version = "dev"
	}
	return &App{out: out, err: err, deps: deps}
}

func (a *App) Run(ctx context.Context, args []string) error {
	args, err := a.applyGlobalFlags(args)
	if err != nil {
		return err
	}
	if len(args) == 0 {
		a.printRootHelp()
		return nil
	}

	switch args[0] {
	case "-h", "--help", "help":
		if len(args) > 1 && args[0] == "help" {
			return a.printCommandHelp(args[1])
		}
		a.printRootHelp()
		return nil
	case "-V", "--version", "version":
		if len(args) > 1 && wantsHelp(args[1:]) {
			a.printVersionHelp()
			return nil
		}
		fmt.Fprintf(a.out, "ctx %s\n", a.deps.Version)
		return nil
	}

	spec, ok := commands[args[0]]
	if !ok {
		return &Error{
			Code:    CodeUsage,
			Command: args[0],
			Message: "unknown command; run `ctx --help` for available commands",
		}
	}
	if wantsHelp(args[1:]) {
		a.printCommand(spec)
		return nil
	}
	if err := validateCommandArgs(spec, args[1:]); err != nil {
		return err
	}
	return a.runCommand(ctx, spec, args[1:])
}

func (a *App) runCommand(ctx context.Context, spec commandSpec, args []string) error {
	switch spec.name {
	case "setup":
		return a.runSetup(ctx, args)
	case "status":
		return a.runStatus(ctx, args)
	case "sources":
		return a.runSources(ctx, args)
	case "import":
		return a.runImport(ctx, args)
	case "search":
		return a.runSearch(ctx, args)
	default:
		return &Error{
			Code:    CodeUnimplemented,
			Command: spec.name,
			Message: "not implemented in the Go edge CLI yet; backing interfaces are defined but not wired",
		}
	}
}

func (a *App) applyGlobalFlags(args []string) ([]string, error) {
	filtered := make([]string, 0, len(args))
	for i := 0; i < len(args); i++ {
		arg := args[i]
		switch {
		case arg == "--data-root":
			if i+1 >= len(args) || args[i+1] == "" || strings.HasPrefix(args[i+1], "-") {
				return nil, &Error{Code: CodeUsage, Message: "missing value for --data-root"}
			}
			a.deps.DataRoot = args[i+1]
			i++
		case strings.HasPrefix(arg, "--data-root="):
			value := strings.TrimPrefix(arg, "--data-root=")
			if value == "" {
				return nil, &Error{Code: CodeUsage, Message: "missing value for --data-root"}
			}
			a.deps.DataRoot = value
		case arg == "--quiet":
		default:
			filtered = append(filtered, arg)
		}
	}
	return filtered, nil
}

func wantsHelp(args []string) bool {
	for _, arg := range args {
		if arg == "-h" || arg == "--help" {
			return true
		}
	}
	return false
}

func (a *App) printRootHelp() {
	fmt.Fprintln(a.out, "Search local agent history")
	fmt.Fprintln(a.out)
	fmt.Fprintln(a.out, "Usage:")
	fmt.Fprintln(a.out, "  ctx <command> [flags]")
	fmt.Fprintln(a.out)
	fmt.Fprintln(a.out, "Commands:")
	for _, spec := range orderedCommands() {
		fmt.Fprintf(a.out, "  %-8s %s\n", spec.name, spec.summary)
	}
	fmt.Fprintf(a.out, "  %-8s %s\n", "version", "Print ctx version")
	fmt.Fprintln(a.out)
	fmt.Fprintln(a.out, "Run `ctx help <command>` for command-specific help.")
}

func (a *App) printCommandHelp(name string) error {
	if name == "version" {
		a.printVersionHelp()
		return nil
	}
	spec, ok := commands[name]
	if !ok {
		return &Error{
			Code:    CodeUsage,
			Command: name,
			Message: "unknown command; run `ctx --help` for available commands",
		}
	}
	a.printCommand(spec)
	return nil
}

func (a *App) printCommand(spec commandSpec) {
	fmt.Fprintf(a.out, "%s\n\n", spec.summary)
	fmt.Fprintln(a.out, "Usage:")
	fmt.Fprintf(a.out, "  %s\n", spec.usage)
	if len(spec.notes) > 0 {
		fmt.Fprintln(a.out)
		for _, note := range spec.notes {
			fmt.Fprintf(a.out, "%s\n", note)
		}
	}
}

func (a *App) printVersionHelp() {
	fmt.Fprintln(a.out, "Print ctx version")
	fmt.Fprintln(a.out)
	fmt.Fprintln(a.out, "Usage:")
	fmt.Fprintln(a.out, "  ctx version")
}

type commandSpec struct {
	name    string
	summary string
	usage   string
	notes   []string
}

var commandOrder = []string{
	"setup",
	"status",
	"index",
	"sources",
	"import",
	"search",
	"show",
	"locate",
	"sql",
	"docs",
	"integrations",
	"daemon",
	"doctor",
	"mcp",
	"upgrade",
}

var commands = map[string]commandSpec{
	"setup": {
		name:    "setup",
		summary: "Create local ctx storage and index discovered history",
		usage:   "ctx setup [--catalog-only] [--no-daemon] [--wait] [--json]",
	},
	"status": {
		name:    "status",
		summary: "Show local ctx index status",
		usage:   "ctx status [--json]",
	},
	"index": {
		name:    "index",
		summary: "Show, watch, or wait for local indexing progress",
		usage:   "ctx index status | ctx index watch | ctx index wait [--json]",
	},
	"sources": {
		name:    "sources",
		summary: "List configured and discovered agent history sources",
		usage:   "ctx sources [--provider <name>] [--all] [--show-missing] [--json]",
	},
	"import": {
		name:    "import",
		summary: "Index provider history into local search",
		usage:   "ctx import [--provider <name>] [--path <path>] [--all] [--json]",
	},
	"search": {
		name:    "search",
		summary: "Search indexed agent history",
		usage:   "ctx search [--json] [--mode lexical|semantic|hybrid] [--limit <n>] [--provider <name>] [--term <query>] <query>",
		notes:   []string{"Search mode is explicit; semantic search must not silently fall back to lexical results."},
	},
	"show": {
		name:    "show",
		summary: "Show an indexed session transcript or event",
		usage:   "ctx show session <id> | ctx show event <id> [--before <n>] [--after <n>]",
	},
	"locate": {
		name:    "locate",
		summary: "Locate provider/source metadata for an indexed session or event",
		usage:   "ctx locate session <id> | ctx locate event <id>",
	},
	"sql": {
		name:    "sql",
		summary: "Run read-only SQL against the local ctx index",
		usage:   "ctx sql <statement> [--json]",
	},
	"docs": {
		name:    "docs",
		summary: "Read embedded ctx documentation",
		usage:   "ctx docs list | ctx docs show <name> | ctx docs search <query>",
	},
	"integrations": {
		name:    "integrations",
		summary: "Install or inspect ctx integrations",
		usage:   "ctx integrations <command> [flags]",
	},
	"daemon": {
		name:    "daemon",
		summary: "Run or inspect local ctx background maintenance",
		usage:   "ctx daemon status | ctx daemon run | ctx daemon stop",
	},
	"doctor": {
		name:    "doctor",
		summary: "Check local ctx health",
		usage:   "ctx doctor [--json]",
	},
	"mcp": {
		name:    "mcp",
		summary: "Serve read-only ctx tools over MCP",
		usage:   "ctx mcp [--stdio]",
	},
	"upgrade": {
		name:    "upgrade",
		summary: "Check or apply signed ctx CLI upgrades",
		usage:   "ctx upgrade status | ctx upgrade apply",
	},
}

func orderedCommands() []commandSpec {
	result := make([]commandSpec, 0, len(commands))
	seen := make(map[string]bool, len(commands))
	for _, name := range commandOrder {
		if spec, ok := commands[name]; ok {
			result = append(result, spec)
			seen[name] = true
		}
	}
	var rest []string
	for name := range commands {
		if !seen[name] {
			rest = append(rest, name)
		}
	}
	sort.Strings(rest)
	for _, name := range rest {
		result = append(result, commands[name])
	}
	return result
}

func commandNames() []string {
	specs := orderedCommands()
	names := make([]string, 0, len(specs))
	for _, spec := range specs {
		names = append(names, spec.name)
	}
	return names
}

func commandList() string {
	return strings.Join(commandNames(), ", ")
}

func validateCommandArgs(spec commandSpec, args []string) error {
	provider, ok, err := flagValue(args, "--provider")
	if err != nil {
		return &Error{
			Code:    CodeUsage,
			Command: spec.name,
			Message: err.Error(),
		}
	}
	if ok && spec.name != "sources" && !supportedProvider(provider) {
		return &Error{
			Code:    CodeUnavailable,
			Command: spec.name,
			Message: fmt.Sprintf("unsupported provider %q in the Go edge runtime; available providers are codex and pi", provider),
		}
	}
	return nil
}

func flagValue(args []string, name string) (string, bool, error) {
	prefix := name + "="
	for i, arg := range args {
		if strings.HasPrefix(arg, prefix) {
			value := strings.TrimPrefix(arg, prefix)
			if value == "" {
				return "", false, fmt.Errorf("missing value for %s", name)
			}
			return value, true, nil
		}
		if arg == name && i+1 < len(args) {
			if args[i+1] == "" || strings.HasPrefix(args[i+1], "-") {
				return "", false, fmt.Errorf("missing value for %s", name)
			}
			return args[i+1], true, nil
		}
		if arg == name {
			return "", false, fmt.Errorf("missing value for %s", name)
		}
	}
	return "", false, nil
}

func supportedProvider(provider string) bool {
	switch provider {
	case "codex", "pi":
		return true
	default:
		return false
	}
}
