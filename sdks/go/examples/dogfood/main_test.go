package main

import (
	"bytes"
	"context"
	"strings"
	"testing"
)

func TestDogfoodExampleUsesFakeTransportByDefault(t *testing.T) {
	var stdout bytes.Buffer
	getenv := func(string) string { return "" }

	if err := run(context.Background(), getenv, &stdout); err != nil {
		t.Fatalf("run returned error: %v", err)
	}

	output := stdout.String()
	for _, want := range []string{
		"status initialized=true indexedItems=1",
		"init initialized=true",
		"import sessions=1",
		"sync events=1",
		"search results=1",
		"search window=1/1 more=true",
		"show event=11111111-1111-4111-8111-111111111111 sequence=1",
		"show session events=1 mode=lite",
		"locate provider=codex cursor=line:1",
		"locate session provider=codex cursor=line:1",
	} {
		if !strings.Contains(output, want) {
			t.Fatalf("output missing %q\n%s", want, output)
		}
	}
}
