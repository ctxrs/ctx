package pi_test

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/ctxrs/ctx/internal/capture"
	"github.com/ctxrs/ctx/internal/provider/pi"
)

func TestPiSessionCapturedBatchGolden(t *testing.T) {
	path := repoPath(t, "tests/fixtures/provider-history/pi-session.jsonl")
	batch := capturePi(t, path, piOptions())

	assertBatchGolden(t, batch, batchGolden{
		ID:             "cb_332d5bcf0a571ac5cf83f231f357da50",
		ContentHash:    "sha256:e7bfda4b16f395efaef948c4a0dcb1fa393b7f7ae7c25f45835bf04624e005ad",
		SourceRevision: "rev_098a4d9cd8a5efd1bf185ef37003404a",
		SourceNativeID: "pi-session-docs-1",
		RecordCount:    7,
		EndByte:        1843,
		EndRecord:      7,
	})
	assertLosslessJSONLRecords(t, path, batch.Records)
	if batch.Provider != capture.ProviderPi || batch.SourceFormat != pi.SourceFormat {
		t.Fatalf("unexpected provider/source format: %q %q", batch.Provider, batch.SourceFormat)
	}
}

func TestPiMalformedRecordIsolation(t *testing.T) {
	path := repoPath(t, "tests/fixtures/provider-history/pi-malformed-mixed.jsonl")
	batch := capturePi(t, path, capture.CaptureOptions{
		SourceID:      "src_pi_malformed",
		SourceURI:     "fixture://pi/malformed.jsonl",
		SourceRootURI: "fixture://pi",
		RepoHints:     fixedHints("pi/malformed.jsonl"),
	})
	if len(batch.Records) != 5 {
		t.Fatalf("expected all records including malformed one, got %d", len(batch.Records))
	}
	if batch.Source.NativeID != "pi-malformed-mixed-1" {
		t.Fatalf("session header did not set source native ID: %+v", batch.Source)
	}
	if !batch.Records[3].Malformed || batch.Records[3].ParseError == "" {
		t.Fatalf("malformed record was not isolated: %+v", batch.Records[3])
	}
	if batch.Records[2].Malformed || batch.Records[4].Malformed {
		t.Fatalf("valid Pi records around malformed line were contaminated: before=%+v after=%+v", batch.Records[2], batch.Records[4])
	}
	assertLosslessJSONLRecords(t, path, batch.Records)
}

func capturePi(t *testing.T, path string, options capture.CaptureOptions) *capture.CapturedBatch {
	t.Helper()
	batch, err := pi.NewAdapter().CaptureFile(context.Background(), path, options)
	if err != nil {
		t.Fatalf("CaptureFile returned error: %v", err)
	}
	if batch == nil {
		t.Fatal("CaptureFile returned nil batch")
	}
	return batch
}

func piOptions() capture.CaptureOptions {
	return capture.CaptureOptions{
		SourceID:      "src_pi_docs",
		SourceURI:     "fixture://pi/session.jsonl",
		SourceRootURI: "fixture://pi",
		RepoHints:     fixedHints("pi/session.jsonl"),
	}
}

type batchGolden struct {
	ID             string
	ContentHash    string
	SourceRevision string
	SourceNativeID string
	RecordCount    int
	EndByte        int64
	EndRecord      int64
}

func assertBatchGolden(t *testing.T, batch *capture.CapturedBatch, want batchGolden) {
	t.Helper()
	if batch.ID != want.ID || batch.ContentHash != want.ContentHash || batch.Revision.ID != want.SourceRevision {
		encoded, _ := json.MarshalIndent(batch, "", "  ")
		t.Fatalf("batch golden mismatch\nwant: %+v\n got: id=%s hash=%s revision=%s\nbatch:\n%s", want, batch.ID, batch.ContentHash, batch.Revision.ID, encoded)
	}
	if batch.Source.NativeID != want.SourceNativeID || batch.Range.RecordCount != want.RecordCount || batch.Range.EndByte != want.EndByte || batch.Range.EndRecord != want.EndRecord {
		t.Fatalf("batch metadata mismatch\nwant: %+v\n got: source=%+v range=%+v", want, batch.Source, batch.Range)
	}
}

func fixedHints(path string) *capture.DeterministicHints {
	return &capture.DeterministicHints{
		RepoRoot:     "fixture://repo",
		RepoRemote:   "https://github.com/ctxrs/ctx.git",
		Commit:       "0000000000000000000000000000000000000000",
		RelativePath: path,
	}
}

func assertLosslessJSONLRecords(t *testing.T, path string, records []capture.ProviderRecord) {
	t.Helper()
	want := readJSONLLines(t, path)
	if len(records) != len(want) {
		t.Fatalf("record count mismatch for lossless check: want %d got %d", len(want), len(records))
	}
	for index, record := range records {
		got := append(append([]byte(nil), record.Raw...), record.Delimiter...)
		if !bytes.Equal(got, want[index]) {
			t.Fatalf("record %d was not lossless\nwant: %q\n got: %q", index, want[index], got)
		}
	}
}

func readJSONLLines(t *testing.T, path string) [][]byte {
	t.Helper()
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	reader := bufio.NewReader(file)
	var lines [][]byte
	for {
		line, err := reader.ReadBytes('\n')
		if len(line) > 0 {
			lines = append(lines, append([]byte(nil), line...))
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			t.Fatal(err)
		}
	}
	return lines
}

func repoPath(t *testing.T, rel string) string {
	t.Helper()
	if workspace := os.Getenv("BUILD_WORKSPACE_DIRECTORY"); workspace != "" {
		candidate := filepath.Join(workspace, filepath.FromSlash(rel))
		if _, err := os.Stat(candidate); err == nil {
			return candidate
		}
	}
	for _, env := range []string{"TEST_SRCDIR", "RUNFILES_DIR"} {
		if root := os.Getenv(env); root != "" {
			for _, prefix := range []string{"_main", "ctx"} {
				candidate := filepath.Join(root, prefix, filepath.FromSlash(rel))
				if _, err := os.Stat(candidate); err == nil {
					return candidate
				}
			}
		}
	}
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	root := filepath.Clean(filepath.Join(filepath.Dir(file), "../../.."))
	return filepath.Join(root, filepath.FromSlash(rel))
}
