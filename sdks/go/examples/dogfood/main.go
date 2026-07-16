package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"

	ctxagenthistory "github.com/ctxrs/ctx/sdks/go"
)

const (
	fixtureEventID   = "11111111-1111-4111-8111-111111111111"
	fixtureSessionID = "22222222-2222-4222-8222-222222222222"
)

func main() {
	if err := run(context.Background(), os.Getenv, os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(ctx context.Context, getenv func(string) string, stdout io.Writer) error {
	client, eventID, sessionID := newClient(getenv)

	status, err := client.Status(ctx)
	if err != nil {
		return fmt.Errorf("status: %w", err)
	}
	fmt.Fprintf(stdout, "status initialized=%t indexedItems=%d\n", status.Status.Initialized, status.Status.IndexedItems)

	initResult, err := client.Init(ctx, ctxagenthistory.InitOptions{CatalogOnly: true})
	if err != nil {
		return fmt.Errorf("init: %w", err)
	}
	fmt.Fprintf(stdout, "init initialized=%t\n", initResult.Status.Initialized)

	importResult, err := client.Import(ctx, ctxagenthistory.ImportOptions{Provider: "codex", Resume: true})
	if err != nil {
		return fmt.Errorf("import: %w", err)
	}
	fmt.Fprintf(stdout, "import sessions=%d\n", importResult.Import.Totals.ImportedSessions)

	syncResult, err := client.Sync(ctx, ctxagenthistory.ImportOptions{All: true})
	if err != nil {
		return fmt.Errorf("sync: %w", err)
	}
	fmt.Fprintf(stdout, "sync events=%d\n", syncResult.Import.Totals.ImportedEvents)

	query := ctxagenthistory.NewSearchQuery(ctxagenthistory.SearchAll("local agent history"))
	search, err := client.Search(ctx, ctxagenthistory.SearchOptions{
		Query:   &query,
		Limit:   5,
		Refresh: "off",
	})
	if err != nil {
		return fmt.Errorf("search: %w", err)
	}
	fmt.Fprintf(stdout, "search results=%d\n", len(search.Search.Results))
	if len(search.Search.Results) > 0 {
		if eventID == "" {
			eventID = search.Search.Results[0].CtxEventID
		}
		if sessionID == "" {
			sessionID = search.Search.Results[0].CtxSessionID
		}
	}
	if eventID == "" {
		return fmt.Errorf("show/locate require CTX_EXAMPLE_EVENT_ID when using a real ctx binary with no search hits")
	}
	if sessionID == "" {
		return fmt.Errorf("show/locate require CTX_EXAMPLE_SESSION_ID when using a real ctx binary with no search hits")
	}

	show, err := client.ShowEvent(ctx, ctxagenthistory.ShowEventOptions{ID: eventID, Before: 1, After: 1})
	if err != nil {
		return fmt.Errorf("show event: %w", err)
	}
	if show.Event.Event != nil {
		fmt.Fprintf(stdout, "show event=%s sequence=%d\n", show.Event.Event.CtxEventID, show.Event.Event.Sequence)
	}

	session, err := client.ShowSession(ctx, ctxagenthistory.ShowSessionOptions{ID: sessionID, Mode: "lite"})
	if err != nil {
		return fmt.Errorf("show session: %w", err)
	}
	fmt.Fprintf(stdout, "show session events=%d mode=%s\n", len(session.Session.Events), session.Session.Mode)

	locate, err := client.LocateEvent(ctx, ctxagenthistory.LocateEventOptions{ID: eventID})
	if err != nil {
		return fmt.Errorf("locate event: %w", err)
	}
	if locate.Location.Source != nil {
		fmt.Fprintf(stdout, "locate provider=%s cursor=%s\n", locate.Location.Provider, locate.Location.Source.Cursor)
	}

	sessionLocation, err := client.LocateSession(ctx, ctxagenthistory.LocateSessionOptions{ID: sessionID})
	if err != nil {
		return fmt.Errorf("locate session: %w", err)
	}
	if sessionLocation.Location.Source != nil {
		fmt.Fprintf(stdout, "locate session provider=%s cursor=%s\n", sessionLocation.Location.Provider, sessionLocation.Location.Source.Cursor)
	}

	return nil
}

func newClient(getenv func(string) string) (*ctxagenthistory.Client, string, string) {
	if path := getenv("CTX_EXAMPLE_CTX_PATH"); path != "" {
		options := []ctxagenthistory.LocalCLIOption{ctxagenthistory.WithCLIPath(path)}
		if dataRoot := getenv("CTX_EXAMPLE_DATA_ROOT"); dataRoot != "" {
			options = append(options, ctxagenthistory.WithDataRoot(dataRoot))
		}
		return ctxagenthistory.NewLocalClient(options...), getenv("CTX_EXAMPLE_EVENT_ID"), getenv("CTX_EXAMPLE_SESSION_ID")
	}
	return ctxagenthistory.NewClient(ctxagenthistory.WithTransport(fakeTransport{})), fixtureEventID, fixtureSessionID
}

type fakeTransport struct{}

func (fakeTransport) Do(_ context.Context, op ctxagenthistory.Operation) ([]byte, error) {
	if op.Name == "version" {
		return []byte("ctx dogfood"), nil
	}
	envelope := map[string]any{
		"contractVersion": ctxagenthistory.APIVersion,
		"schemaVersion":   ctxagenthistory.SchemaVersion,
		"operation":       operationName(op.Name),
		"backend": map[string]any{
			"kind":     "local",
			"dataRoot": "/tmp/ctx-sdk-dogfood",
		},
	}

	switch op.Name {
	case "status", "init":
		envelope["status"] = map[string]any{
			"initialized":  true,
			"localOnly":    true,
			"dataRoot":     "/tmp/ctx-sdk-dogfood",
			"indexedItems": 1,
		}
	case "import", "sync":
		envelope["import"] = map[string]any{
			"resume": true,
			"totals": map[string]any{
				"importedSources":  1,
				"importedSessions": 1,
				"importedEvents":   1,
			},
			"sources": []map[string]any{
				{
					"provider":         "codex",
					"path":             "/tmp/ctx-sdk-dogfood/session.jsonl",
					"status":           "imported",
					"importedSessions": 1,
					"importedEvents":   1,
				},
			},
		}
	case "search":
		envelope["search"] = map[string]any{
			"schema_version": ctxagenthistory.SearchSchemaVersion,
			"query": map[string]any{
				"version": ctxagenthistory.SearchQueryVersion,
				"any":     []map[string]any{{"all": "local agent history"}},
			},
			"query_execution": map[string]any{
				"query_version":      ctxagenthistory.SearchQueryVersion,
				"candidate_strategy": "bounded_union_rrf_v1",
			},
			"freshness": map[string]any{
				"mode":        "off",
				"status":      "skipped",
				"sourceCount": 0,
				"totals":      map[string]any{},
			},
			"results": []map[string]any{
				{
					"ctxEventId":   fixtureEventID,
					"ctxSessionId": fixtureSessionID,
					"title":        "Dogfood fixture",
					"snippet":      "local agent history search result",
					"rank":         1.0,
					"resultScope":  "event",
					"provider":     "codex",
					"whyMatched":   []string{"text"},
				},
			},
			"pagination": map[string]any{"limit": 5},
			"truncation": map[string]any{"truncated": false},
		}
	case "showEvent":
		event := dogfoodEvent()
		envelope["event"] = map[string]any{
			"event":  event,
			"events": []map[string]any{event},
			"source": dogfoodSource(),
		}
	case "showSession":
		envelope["session"] = map[string]any{
			"session": map[string]any{
				"ctxSessionId":      fixtureSessionID,
				"provider":          "codex",
				"providerSessionId": "dogfood-session",
			},
			"events": []map[string]any{dogfoodEvent()},
			"source": dogfoodSource(),
			"mode":   "lite",
			"format": "json",
		}
	case "locateEvent":
		envelope["location"] = dogfoodLocation(true)
	case "locateSession":
		envelope["location"] = dogfoodLocation(false)
	default:
		envelope["operation"] = "error"
		envelope["error"] = map[string]any{
			"code":      "invalid_request",
			"message":   fmt.Sprintf("unsupported fake operation %q", op.Name),
			"retryable": false,
		}
	}

	return json.Marshal(envelope)
}

func dogfoodEvent() map[string]any {
	return map[string]any{
		"ctxEventId":   fixtureEventID,
		"ctxSessionId": fixtureSessionID,
		"sequence":     1,
		"eventType":    "message",
		"role":         "assistant",
		"source":       "codex",
		"cursor":       "line:1",
		"text":         "local agent history search result",
	}
}

func dogfoodSource() map[string]any {
	return map[string]any{
		"path":         "/tmp/ctx-sdk-dogfood/session.jsonl",
		"cursor":       "line:1",
		"exists":       false,
		"sourceId":     "33333333-3333-4333-8333-333333333333",
		"sourceFormat": "codex_session_jsonl",
	}
}

func dogfoodLocation(includeEvent bool) map[string]any {
	location := map[string]any{
		"ctxSessionId":      fixtureSessionID,
		"provider":          "codex",
		"providerSessionId": "dogfood-session",
		"source":            dogfoodSource(),
		"resume":            map[string]any{"cursor": "line:1"},
	}
	if includeEvent {
		location["ctxEventId"] = fixtureEventID
	}
	return location
}

func operationName(name string) string {
	switch name {
	case "show_event":
		return "showEvent"
	case "show_session":
		return "showSession"
	case "locate_event":
		return "locateEvent"
	case "locate_session":
		return "locateSession"
	case "setup":
		return "init"
	default:
		return name
	}
}
