package capture

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestJSONLAppendCaptureDoesNotReadBeforeStartByte(t *testing.T) {
	prefix := bytes.Repeat([]byte("prefix that must not be read\n"), 8192)
	suffix := []byte("{\"ok\":1}\n{\"ok\":2}\n")
	startByte := int64(len(prefix))
	source := &trackingSource{
		data:          append(append([]byte(nil), prefix...), suffix...),
		startByte:     startByte,
		minReadOffset: -1,
		info: fakeFileInfo{
			name:    "append.jsonl",
			size:    int64(len(prefix) + len(suffix)),
			modTime: time.Unix(1720000000, 0).UTC(),
		},
	}

	batch, err := (JSONLAdapter{
		Provider:     ProviderCodex,
		SourceFormat: "test_jsonl",
	}).CaptureFile(context.Background(), "/unopened/append.jsonl", CaptureOptions{
		SourceID:    "src_append",
		SourceURI:   "fixture://append.jsonl",
		StartByte:   startByte,
		StartRecord: 41,
		openFile: func(string) (readSeekStatCloser, error) {
			return source, nil
		},
	})
	if err != nil {
		t.Fatalf("CaptureFile returned error: %v", err)
	}
	if batch == nil {
		t.Fatal("CaptureFile returned nil batch")
	}
	if source.readBeforeStart {
		t.Fatalf("append capture read before StartByte: first read at %d, start %d", source.minReadOffset, startByte)
	}
	if source.minReadOffset != startByte {
		t.Fatalf("append capture did not start reading at StartByte: first read at %d, start %d", source.minReadOffset, startByte)
	}
	if batch.Range.StartByte != startByte || batch.Range.StartRecord != 41 || batch.Range.RecordCount != 2 {
		t.Fatalf("unexpected append range: %+v", batch.Range)
	}
	if batch.Revision.Kind != "file_slice_content" || batch.Revision.ContentHash != batch.ContentHash || batch.Revision.SizeBytes != int64(len(suffix)) {
		t.Fatalf("append revision does not describe the captured slice: revision=%+v content=%s", batch.Revision, batch.ContentHash)
	}
}

func TestJSONLRepoHintsDoNotSerializeRawRemoteOrRoot(t *testing.T) {
	root := filepath.Join(t.TempDir(), "repo")
	if err := os.MkdirAll(filepath.Join(root, ".git"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, ".git", "HEAD"), []byte("0000000000000000000000000000000000000000\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	secretRemote := "https://alice:ghp_super_secret@example.com/private/ctx.git"
	if err := os.WriteFile(filepath.Join(root, ".git", "config"), []byte("[remote \"origin\"]\n\turl = "+secretRemote+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	sourcePath := filepath.Join(root, "session.jsonl")
	if err := os.WriteFile(sourcePath, []byte("{\"ok\":true}\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	adapter := JSONLAdapter{Provider: ProviderCodex, SourceFormat: "test_jsonl"}
	batch, err := adapter.CaptureFile(context.Background(), sourcePath, CaptureOptions{
		SourceID:  "src_repo_hints",
		SourceURI: "fixture://session.jsonl",
	})
	if err != nil {
		t.Fatalf("CaptureFile returned error: %v", err)
	}
	if batch.Hints.RepoRoot != "" || batch.Hints.RepoRemote != "" {
		t.Fatalf("raw repo root/remote leaked into detected hints: %+v", batch.Hints)
	}
	encoded, err := json.Marshal(batch)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(encoded), "ghp_super_secret") || strings.Contains(string(encoded), secretRemote) {
		t.Fatalf("credential-like remote leaked into serialized batch: %s", encoded)
	}

	explicitRoot := "/private/local/root/that/must/not/serialize"
	batch, err = adapter.CaptureFile(context.Background(), sourcePath, CaptureOptions{
		SourceID:  "src_explicit_repo_hints",
		SourceURI: "fixture://session.jsonl",
		RepoHints: &DeterministicHints{
			RepoRoot:     explicitRoot,
			RepoRemote:   secretRemote,
			Commit:       "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
			RelativePath: "session.jsonl",
		},
	})
	if err != nil {
		t.Fatalf("CaptureFile with explicit hints returned error: %v", err)
	}
	encoded, err = json.Marshal(batch)
	if err != nil {
		t.Fatal(err)
	}
	serialized := string(encoded)
	if strings.Contains(serialized, "ghp_super_secret") || strings.Contains(serialized, secretRemote) || strings.Contains(serialized, explicitRoot) {
		t.Fatalf("raw explicit repo hints leaked into serialized batch: %s", serialized)
	}
	if batch.Hints.RepoRoot != "" || batch.Hints.RepoRemote != "" || batch.Hints.RelativePath != "session.jsonl" {
		t.Fatalf("explicit hints were not safely sanitized: %+v", batch.Hints)
	}
	if !containsString(batch.Privacy.Notes, "repo_hints_sanitized") {
		t.Fatalf("privacy status did not record repo hint sanitization: %+v", batch.Privacy)
	}
}

type trackingSource struct {
	data            []byte
	pos             int64
	startByte       int64
	minReadOffset   int64
	readBeforeStart bool
	info            os.FileInfo
}

func (source *trackingSource) Read(p []byte) (int, error) {
	if source.minReadOffset == -1 || source.pos < source.minReadOffset {
		source.minReadOffset = source.pos
	}
	if source.pos < source.startByte {
		source.readBeforeStart = true
	}
	if source.pos >= int64(len(source.data)) {
		return 0, io.EOF
	}
	n := copy(p, source.data[source.pos:])
	source.pos += int64(n)
	return n, nil
}

func (source *trackingSource) Seek(offset int64, whence int) (int64, error) {
	var next int64
	switch whence {
	case io.SeekStart:
		next = offset
	case io.SeekCurrent:
		next = source.pos + offset
	case io.SeekEnd:
		next = int64(len(source.data)) + offset
	default:
		return 0, errors.New("invalid seek whence")
	}
	if next < 0 {
		return 0, errors.New("negative seek offset")
	}
	source.pos = next
	return source.pos, nil
}

func (source *trackingSource) Stat() (os.FileInfo, error) {
	return source.info, nil
}

func (source *trackingSource) Close() error {
	return nil
}

type fakeFileInfo struct {
	name    string
	size    int64
	modTime time.Time
}

func (info fakeFileInfo) Name() string       { return info.name }
func (info fakeFileInfo) Size() int64        { return info.size }
func (info fakeFileInfo) Mode() os.FileMode  { return 0o600 }
func (info fakeFileInfo) ModTime() time.Time { return info.modTime }
func (info fakeFileInfo) IsDir() bool        { return false }
func (info fakeFileInfo) Sys() any           { return nil }

func containsString(values []string, target string) bool {
	for _, value := range values {
		if value == target {
			return true
		}
	}
	return false
}
