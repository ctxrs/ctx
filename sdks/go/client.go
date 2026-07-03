package ctxagenthistory

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
)

// Operation is the adapter-neutral command executed by a transport.
type Operation struct {
	Name string
	Args []string
}

// Transport executes agent-history-v1 operations and returns JSON stdout.
type Transport interface {
	Do(ctx context.Context, op Operation) ([]byte, error)
}

// Client is a agent-history-v1 ctx client.
type Client struct {
	transport Transport
}

// Option configures a Client.
type Option func(*Client)

// WithTransport sets the transport used by the client.
func WithTransport(transport Transport) Option {
	return func(client *Client) {
		client.transport = transport
	}
}

// NewClient creates a agent-history-v1 client. By default it uses the local ctx CLI.
func NewClient(options ...Option) *Client {
	client := &Client{transport: NewLocalCLIAdapter()}
	for _, option := range options {
		option(client)
	}
	return client
}

// NewLocalClient creates a agent-history-v1 client backed by the local ctx CLI.
func NewLocalClient(options ...LocalCLIOption) *Client {
	return NewClient(WithTransport(NewLocalCLIAdapter(options...)))
}

// InitOptions configures Client.Init.
type InitOptions struct {
	CatalogOnly bool
}

// ImportOptions configures Client.Import and Client.Sync.
type ImportOptions struct {
	Provider string
	Path     string
	All      bool
	Resume   bool
}

// SearchOptions configures Client.Search.
type SearchOptions struct {
	Query                 string
	Terms                 []string
	Limit                 int
	Provider              string
	Workspace             string
	Since                 string
	PrimaryOnly           bool
	IncludeSubagents      bool
	EventType             string
	File                  string
	Session               string
	Events                bool
	Refresh               string
	IncludeCurrentSession bool
}

// ShowSessionOptions configures Client.ShowSession.
type ShowSessionOptions struct {
	ID                string
	Provider          string
	ProviderSessionID string
	Mode              string
}

// ShowEventOptions configures Client.ShowEvent.
type ShowEventOptions struct {
	ID     string
	Before int
	After  int
	Window *int
}

// LocateSessionOptions configures Client.LocateSession.
type LocateSessionOptions struct {
	ID                string
	Provider          string
	ProviderSessionID string
}

// LocateEventOptions configures Client.LocateEvent.
type LocateEventOptions struct {
	ID string
}

func (c *Client) Status(ctx context.Context) (*StatusResponse, error) {
	var out StatusResponse
	if err := c.do(ctx, Operation{Name: "status", Args: []string{"status", "--json"}}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) Init(ctx context.Context, opts InitOptions) (*InitResponse, error) {
	args := []string{"setup", "--json", "--progress", "none"}
	if opts.CatalogOnly {
		args = append(args, "--catalog-only")
	}
	var out InitResponse
	if err := c.do(ctx, Operation{Name: "init", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) Sources(ctx context.Context) (*SourcesResponse, error) {
	var out SourcesResponse
	if err := c.do(ctx, Operation{Name: "sources", Args: []string{"sources", "--json"}}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) Import(ctx context.Context, opts ImportOptions) (*ImportResponse, error) {
	args := []string{"import", "--json", "--progress", "none"}
	args = appendImportOptions(args, opts)
	var out ImportResponse
	if err := c.do(ctx, Operation{Name: "import", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// Sync is an alias for Import in agent-history-v1. ctx writes and refreshes the local index.
func (c *Client) Sync(ctx context.Context, opts ImportOptions) (*ImportResponse, error) {
	args := []string{"import", "--json", "--progress", "none"}
	args = appendImportOptions(args, opts)
	var out ImportResponse
	if err := c.do(ctx, Operation{Name: "sync", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) Search(ctx context.Context, opts SearchOptions) (*SearchResponse, error) {
	if !opts.hasIntent() {
		return nil, sdkError(ErrorKindInvalidArgument, "search requires a query, term, or file option", nil)
	}
	args := []string{"search"}
	if opts.Query != "" {
		args = append(args, opts.Query)
	}
	args = append(args, "--json")
	if opts.Limit > 0 {
		args = append(args, "--limit", strconv.Itoa(opts.Limit))
	}
	for _, term := range opts.Terms {
		args = append(args, "--term", term)
	}
	appendStringFlag := func(name, value string) {
		if value != "" {
			args = append(args, name, value)
		}
	}
	appendStringFlag("--provider", opts.Provider)
	appendStringFlag("--workspace", opts.Workspace)
	appendStringFlag("--since", opts.Since)
	appendStringFlag("--event-type", opts.EventType)
	appendStringFlag("--file", opts.File)
	appendStringFlag("--session", opts.Session)
	appendStringFlag("--refresh", opts.Refresh)
	if opts.PrimaryOnly {
		args = append(args, "--primary-only")
	}
	if opts.IncludeSubagents {
		args = append(args, "--include-subagents")
	}
	if opts.Events {
		args = append(args, "--events")
	}
	if opts.IncludeCurrentSession {
		args = append(args, "--include-current-session")
	}
	var out SearchResponse
	if err := c.do(ctx, Operation{Name: "search", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (opts SearchOptions) hasIntent() bool {
	if strings.TrimSpace(opts.Query) != "" || strings.TrimSpace(opts.File) != "" {
		return true
	}
	for _, term := range opts.Terms {
		if strings.TrimSpace(term) != "" {
			return true
		}
	}
	return false
}

func (c *Client) ShowSession(ctx context.Context, opts ShowSessionOptions) (*ShowSessionResponse, error) {
	args := []string{"show", "session"}
	if opts.ID != "" {
		args = append(args, opts.ID)
	}
	if opts.Provider != "" {
		args = append(args, "--provider", opts.Provider)
	}
	if opts.ProviderSessionID != "" {
		args = append(args, "--provider-session", opts.ProviderSessionID)
	}
	if opts.Mode != "" {
		args = append(args, "--mode", opts.Mode)
	}
	args = append(args, "--format", "json")
	var out ShowSessionResponse
	if err := c.do(ctx, Operation{Name: "showSession", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) ShowEvent(ctx context.Context, opts ShowEventOptions) (*ShowEventResponse, error) {
	if opts.ID == "" {
		return nil, sdkError(ErrorKindInvalidArgument, "show event requires ID", nil)
	}
	args := []string{"show", "event", opts.ID, "--format", "json"}
	if opts.Before > 0 {
		args = append(args, "--before", strconv.Itoa(opts.Before))
	}
	if opts.After > 0 {
		args = append(args, "--after", strconv.Itoa(opts.After))
	}
	if opts.Window != nil {
		args = append(args, "--window", strconv.Itoa(*opts.Window))
	}
	var out ShowEventResponse
	if err := c.do(ctx, Operation{Name: "showEvent", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) LocateSession(ctx context.Context, opts LocateSessionOptions) (*LocateSessionResponse, error) {
	args := []string{"locate", "session"}
	if opts.ID != "" {
		args = append(args, opts.ID)
	}
	if opts.Provider != "" {
		args = append(args, "--provider", opts.Provider)
	}
	if opts.ProviderSessionID != "" {
		args = append(args, "--provider-session", opts.ProviderSessionID)
	}
	args = append(args, "--format", "json")
	var out LocateSessionResponse
	if err := c.do(ctx, Operation{Name: "locateSession", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) LocateEvent(ctx context.Context, opts LocateEventOptions) (*LocateEventResponse, error) {
	if opts.ID == "" {
		return nil, sdkError(ErrorKindInvalidArgument, "locate event requires ID", nil)
	}
	args := []string{"locate", "event", opts.ID, "--format", "json"}
	var out LocateEventResponse
	if err := c.do(ctx, Operation{Name: "locateEvent", Args: args}, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

func (c *Client) do(ctx context.Context, op Operation, out any) error {
	if c.transport == nil {
		return sdkError(ErrorKindTransportUnavailable, "ctxagenthistory client has no transport", nil)
	}
	payload, err := c.transport.Do(ctx, op)
	if err != nil {
		return err
	}
	payload, err = normalizePayload(op, payload)
	if err != nil {
		return sdkError(ErrorKindDecode, fmt.Sprintf("normalize %s response", op.Name), err)
	}
	if err := json.Unmarshal(payload, out); err != nil {
		return sdkError(ErrorKindDecode, fmt.Sprintf("decode %s response", op.Name), err)
	}
	if envelope, ok := responseEnvelope(out); ok {
		if envelope.ContractVersion != APIVersion {
			return sdkError(ErrorKindUnsupportedSchema, fmt.Sprintf("unsupported ctx contract version %q", envelope.ContractVersion), nil)
		}
		if envelope.SchemaVersion != SchemaVersion {
			return sdkError(ErrorKindUnsupportedSchema, fmt.Sprintf("unsupported ctx schema version %d", envelope.SchemaVersion), nil)
		}
		if want := OperationName(agentHistoryOperationName(op.Name)); envelope.Operation != want {
			return sdkError(ErrorKindDecode, fmt.Sprintf("decode %s response: operation was %q", op.Name, envelope.Operation), nil)
		}
	}
	return nil
}

func appendImportOptions(args []string, opts ImportOptions) []string {
	if opts.Provider != "" {
		args = append(args, "--provider", opts.Provider)
	}
	if opts.Path != "" {
		args = append(args, "--path", opts.Path)
	}
	if opts.All {
		args = append(args, "--all")
	}
	if opts.Resume {
		args = append(args, "--resume")
	}
	return args
}

func responseEnvelope(out any) (Envelope, bool) {
	switch value := out.(type) {
	case *StatusResponse:
		return value.Envelope, true
	case *InitResponse:
		return value.Envelope, true
	case *SourcesResponse:
		return value.Envelope, true
	case *ImportResponse:
		return value.Envelope, true
	case *SearchResponse:
		return value.Envelope, true
	case *ShowSessionResponse:
		return value.Envelope, true
	case *ShowEventResponse:
		return value.Envelope, true
	case *LocateSessionResponse:
		return value.Envelope, true
	case *LocateEventResponse:
		return value.Envelope, true
	case *ErrorResponse:
		return value.Envelope, true
	default:
		return Envelope{}, false
	}
}
