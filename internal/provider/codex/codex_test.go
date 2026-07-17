package codex_test

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
	"strings"
	"testing"

	"github.com/ctxrs/ctx/internal/capture"
	"github.com/ctxrs/ctx/internal/provider/codex"
)

func TestCodexSessionCapturedBatchGolden(t *testing.T) {
	path := repoPath(t, "tests/fixtures/provider-history/codex-sessions/2026/06/23/root.jsonl")
	batch := captureCodex(t, path, codexOptions())

	assertBatchGolden(t, batch, batchGolden{
		ID:             "cb_6cd118fc767f4b37b8391f2524d49e74",
		ContentHash:    "sha256:09b2fd40acb9ad704f0763929b96f9bd6665d7b201d765233e5209da9b0e678a",
		SourceRevision: "rev_0720fa1e54c26f0204ca30d6a96eb2f0",
		SourceNativeID: "codex-session-root",
		RecordCount:    8,
		EndByte:        1738,
		EndRecord:      8,
	})
	assertLosslessJSONLRecords(t, path, batch.Records)
	if batch.Provider != capture.ProviderCodex || batch.SourceFormat != codex.SourceFormat {
		t.Fatalf("unexpected provider/source format: %q %q", batch.Provider, batch.SourceFormat)
	}
	if batch.Privacy.PolicyID != capture.DefaultPrivacyPolicyID || !batch.Privacy.Applied {
		t.Fatalf("privacy status was not recorded: %+v", batch.Privacy)
	}
}

func TestCodexTreeCaptureSortsFilesAndPreservesSourceIdentity(t *testing.T) {
	root := repoPath(t, "tests/fixtures/provider-history/codex-sessions")
	batches, err := codex.NewAdapter().CaptureTree(context.Background(), root, capture.CaptureOptions{})
	if err != nil {
		t.Fatalf("CaptureTree returned error: %v", err)
	}
	if len(batches) != 2 {
		t.Fatalf("expected two codex session batches, got %d", len(batches))
	}
	if !strings.HasSuffix(filepath.ToSlash(batches[0].Source.Path), "tests/fixtures/provider-history/codex-sessions/2026/06/23/root.jsonl") {
		t.Fatalf("tree batches were not sorted by path: %+v", batches[0].Source)
	}
	if batches[1].Source.NativeID != "codex-session-child" {
		t.Fatalf("subagent native source identity missing: %+v", batches[1].Source)
	}
	if batches[0].Source.ID == batches[1].Source.ID {
		t.Fatalf("tree files should receive distinct source identities: %q", batches[0].Source.ID)
	}
}

func TestCodexAppendAndReplacementRanges(t *testing.T) {
	path := repoPath(t, "tests/fixtures/provider-history/codex-sessions/2026/06/23/root.jsonl")
	first := captureCodex(t, path, capture.CaptureOptions{
		SourceID:      "src_codex_root",
		SourceURI:     "fixture://codex/root.jsonl",
		SourceRootURI: "fixture://codex",
		MaxRecords:    2,
		RepoHints:     fixedHints("codex/root.jsonl"),
	})
	second := captureCodex(t, path, capture.CaptureOptions{
		SourceID:      "src_codex_root",
		SourceURI:     "fixture://codex/root.jsonl",
		SourceRootURI: "fixture://codex",
		StartByte:     first.Checkpoint.NextByte,
		StartRecord:   first.Checkpoint.NextRecord,
		RepoHints:     fixedHints("codex/root.jsonl"),
	})

	if first.Range.StartByte != 0 || first.Range.EndRecord != 2 {
		t.Fatalf("unexpected first append range: %+v", first.Range)
	}
	if second.Range.StartByte != first.Checkpoint.NextByte || second.Range.StartRecord != first.Checkpoint.NextRecord {
		t.Fatalf("append range did not start at first checkpoint: first=%+v second=%+v", first.Checkpoint, second.Range)
	}
	if second.Range.EndByte != int64(fileSize(t, path)) || second.Range.EndRecord != 8 {
		t.Fatalf("append range did not end at source EOF: %+v", second.Range)
	}
	if first.ID == second.ID || first.ContentHash == second.ContentHash {
		t.Fatalf("append batches should have distinct identity/content: %s %s", first.ID, second.ID)
	}

	temp := t.TempDir()
	replacement := filepath.Join(temp, "root.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	data = append(data, []byte(`{"timestamp":"2026-06-23T15:00:08.000Z","type":"event_msg","payload":{"type":"turn_aborted"}}`+"\n")...)
	if err := os.WriteFile(replacement, data, 0o600); err != nil {
		t.Fatal(err)
	}
	replaced := captureCodex(t, replacement, capture.CaptureOptions{
		SourceID:      "src_codex_root",
		SourceURI:     "fixture://codex/root.jsonl",
		SourceRootURI: "fixture://codex",
		RepoHints:     fixedHints("codex/root.jsonl"),
	})
	if replaced.Revision.ID == first.Revision.ID || replaced.ID == first.ID {
		t.Fatalf("replacement did not produce a new revision and batch: old=%s/%s new=%s/%s", first.Revision.ID, first.ID, replaced.Revision.ID, replaced.ID)
	}
	if replaced.Range.EndRecord != 9 || replaced.Checkpoint.NextByte <= second.Checkpoint.NextByte {
		t.Fatalf("replacement checkpoint did not cover new record: %+v", replaced.Checkpoint)
	}
}

func TestCodexMalformedRecordIsolation(t *testing.T) {
	path := repoPath(t, "tests/fixtures/provider-history/codex-malformed-session.jsonl")
	batch := captureCodex(t, path, capture.CaptureOptions{
		SourceID:      "src_codex_malformed",
		SourceURI:     "fixture://codex/malformed.jsonl",
		SourceRootURI: "fixture://codex",
		RepoHints:     fixedHints("codex/malformed.jsonl"),
	})
	if len(batch.Records) != 4 {
		t.Fatalf("expected all records including malformed one, got %d", len(batch.Records))
	}
	if !batch.Records[2].Malformed || batch.Records[2].ParseError == "" {
		t.Fatalf("malformed record was not isolated: %+v", batch.Records[2])
	}
	if batch.Records[1].Malformed || batch.Records[3].Malformed {
		t.Fatalf("valid records around malformed line were contaminated: before=%+v after=%+v", batch.Records[1], batch.Records[3])
	}
	assertLosslessJSONLRecords(t, path, batch.Records)
}

func TestCodexPrivacyFilterRunsBeforeBatchCreation(t *testing.T) {
	path := repoPath(t, "tests/fixtures/provider-history/codex-sessions/2026/06/23/root.jsonl")
	options := codexOptions()
	options.PrivacyFilter = ordinalFilter{drop: 1}
	batch := captureCodex(t, path, options)

	if batch.Privacy.FilteredRecords != 1 || batch.Privacy.Reasons["test_exclusion"] != 1 {
		t.Fatalf("privacy filtering status missing: %+v", batch.Privacy)
	}
	if len(batch.Records) != 7 {
		t.Fatalf("filtered record was included in batch: %d records", len(batch.Records))
	}
	for _, record := range batch.Records {
		if record.Ordinal == 1 {
			t.Fatalf("privacy-filtered record ordinal 1 reached batch: %+v", record)
		}
	}
}

type ordinalFilter struct {
	drop int64
}

func (filter ordinalFilter) PolicyID() string { return "test-drop-ordinal-v1" }

func (filter ordinalFilter) AllowSource(capture.SourceCandidate) capture.PrivacyDecision {
	return capture.PrivacyDecision{Allow: true}
}

func (filter ordinalFilter) AllowRecord(candidate capture.RecordCandidate) capture.PrivacyDecision {
	if candidate.Ordinal == filter.drop {
		return capture.PrivacyDecision{Allow: false, Reason: "test_exclusion"}
	}
	return capture.PrivacyDecision{Allow: true}
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

func captureCodex(t *testing.T, path string, options capture.CaptureOptions) *capture.CapturedBatch {
	t.Helper()
	batch, err := codex.NewAdapter().CaptureFile(context.Background(), path, options)
	if err != nil {
		t.Fatalf("CaptureFile returned error: %v", err)
	}
	if batch == nil {
		t.Fatal("CaptureFile returned nil batch")
	}
	return batch
}

func codexOptions() capture.CaptureOptions {
	return capture.CaptureOptions{
		SourceID:      "src_codex_root",
		SourceURI:     "fixture://codex/root.jsonl",
		SourceRootURI: "fixture://codex",
		RepoHints:     fixedHints("tests/fixtures/provider-history/codex-sessions/2026/06/23/root.jsonl"),
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

func fileSize(t *testing.T, path string) int {
	t.Helper()
	stat, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	return int(stat.Size())
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
