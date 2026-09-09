package ctxagenthistory

import (
	"context"
	"strings"
)

const (
	// APIVersion identifies the ctx agent history contract implemented by this SDK.
	APIVersion = "agent-history-v2"

	// SchemaVersion is the JSON schema version emitted by ctx agent-history-v2 commands.
	SchemaVersion = 1

	// SDKVersion is the experimental Go SDK version.
	SDKVersion = "0.1.0-experimental"
)

// VersionInfo reports SDK, contract, and local ctx version metadata.
type VersionInfo struct {
	APIVersion    string `json:"apiVersion"`
	SchemaVersion int    `json:"schemaVersion"`
	SDKVersion    string `json:"sdkVersion"`
	Transport     string `json:"transport"`
	CtxVersion    string `json:"ctxVersion,omitempty"`
}

// Version returns SDK/contract metadata and, for local clients, the ctx CLI version.
func (c *Client) Version(ctx context.Context) (*VersionInfo, error) {
	info := &VersionInfo{
		APIVersion:    APIVersion,
		SchemaVersion: SchemaVersion,
		SDKVersion:    SDKVersion,
		Transport:     "custom",
	}
	if c.transport == nil {
		return nil, sdkError(ErrorKindTransportUnavailable, "ctxagenthistory client has no transport", nil)
	}
	payload, err := c.transport.Do(ctx, Operation{Name: "version", Args: []string{"--version"}})
	if err != nil {
		return nil, err
	}
	info.CtxVersion = strings.TrimSpace(string(payload))
	if _, ok := c.transport.(*LocalCLIAdapter); ok {
		info.Transport = "local-cli"
	}
	if _, ok := c.transport.(hostedTransport); ok {
		info.Transport = "hosted-placeholder"
		info.CtxVersion = ""
	}
	return info, nil
}
