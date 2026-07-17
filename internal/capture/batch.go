package capture

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"strconv"
	"strings"
)

const (
	CapturedBatchSchemaVersion = 1
	NativeRecordSchemaVersion  = 1

	DefaultPrivacyPolicyID = "local-private-lossless-v1"
)

var ErrNoRecords = errors.New("capture: no records")

type RecordRange struct {
	StartByte   int64 `json:"startByte"`
	EndByte     int64 `json:"endByte"`
	StartRecord int64 `json:"startRecord"`
	EndRecord   int64 `json:"endRecord"`
	RecordCount int   `json:"recordCount"`
}

type ProviderRecord struct {
	Ordinal     int64             `json:"ordinal"`
	ByteStart   int64             `json:"byteStart"`
	ByteEnd     int64             `json:"byteEnd"`
	Raw         []byte            `json:"raw"`
	Delimiter   []byte            `json:"delimiter,omitempty"`
	ContentHash string            `json:"contentHash"`
	Malformed   bool              `json:"malformed,omitempty"`
	ParseError  string            `json:"parseError,omitempty"`
	Kind        string            `json:"kind,omitempty"`
	NativeID    string            `json:"nativeId,omitempty"`
	Hints       map[string]string `json:"hints,omitempty"`
}

type DeterministicHints struct {
	RepoRoot     string `json:"repoRoot,omitempty"`
	RepoRemote   string `json:"repoRemote,omitempty"`
	Commit       string `json:"commit,omitempty"`
	RelativePath string `json:"relativePath,omitempty"`
}

type PrivacyFilterStatus struct {
	Applied         bool             `json:"applied"`
	PolicyID        string           `json:"policyId"`
	FilteredRecords int              `json:"filteredRecords"`
	FilteredBytes   int64            `json:"filteredBytes"`
	Reasons         map[string]int   `json:"reasons,omitempty"`
	SourceExcluded  bool             `json:"sourceExcluded,omitempty"`
	Notes           []string         `json:"notes,omitempty"`
	RecordPolicy    PrivacyPolicyRef `json:"recordPolicy"`
}

type PrivacyPolicyRef struct {
	LosslessAfterFiltering bool   `json:"losslessAfterFiltering"`
	Stage                  string `json:"stage"`
}

type SourceCheckpoint struct {
	SourceID       string `json:"sourceId"`
	SourceRevision string `json:"sourceRevision"`
	NextByte       int64  `json:"nextByte"`
	NextRecord     int64  `json:"nextRecord"`
	ContentHash    string `json:"contentHash"`
}

type BatchInput struct {
	Provider       Provider
	SourceFormat   string
	SourceID       string
	SourceURI      string
	SourceRootURI  string
	SourcePath     string
	NativeSourceID string
	Revision       SourceRevision
	Range          RecordRange
	Records        []ProviderRecord
	Hints          DeterministicHints
	Privacy        PrivacyFilterStatus
}

func NewCapturedBatch(input BatchInput) (CapturedBatch, error) {
	if input.Provider == "" {
		return CapturedBatch{}, errors.New("capture: provider is required")
	}
	if input.SourceFormat == "" {
		return CapturedBatch{}, errors.New("capture: source format is required")
	}
	if len(input.Records) == 0 {
		return CapturedBatch{}, ErrNoRecords
	}

	sourceID := input.SourceID
	if sourceID == "" {
		sourceID = stableID("src", string(input.Provider), input.SourceFormat, input.SourceURI, input.NativeSourceID)
	}
	contentHash := batchContentHash(input.Records)
	input.Range.RecordCount = len(input.Records)
	if input.Range.EndRecord == 0 {
		input.Range.EndRecord = input.Range.StartRecord + int64(len(input.Records))
	}
	if input.Range.EndByte == 0 {
		input.Range.EndByte = input.Records[len(input.Records)-1].ByteEnd
	}
	if input.Range.StartByte == 0 {
		input.Range.StartByte = input.Records[0].ByteStart
	}
	if input.Privacy.PolicyID == "" {
		input.Privacy = DefaultPrivacyStatus()
	}
	hints, sanitizedHints := sanitizeDeterministicHints(input.Hints)
	if sanitizedHints {
		input.Privacy.Notes = append(input.Privacy.Notes, "repo_hints_sanitized")
	}
	input.Privacy.Applied = true
	input.Privacy.RecordPolicy = PrivacyPolicyRef{
		LosslessAfterFiltering: true,
		Stage:                  "pre_batch",
	}
	batchID := stableID(
		"cb",
		strconv.Itoa(CapturedBatchSchemaVersion),
		string(input.Provider),
		sourceID,
		input.SourceFormat,
		input.Revision.ID,
		strconv.FormatInt(input.Range.StartByte, 10),
		strconv.FormatInt(input.Range.EndByte, 10),
		strconv.FormatInt(input.Range.StartRecord, 10),
		strconv.FormatInt(input.Range.EndRecord, 10),
		strconv.Itoa(NativeRecordSchemaVersion),
		contentHash,
		input.Privacy.PolicyID,
	)

	source := SourceRef{
		ID:       sourceID,
		Provider: input.Provider,
		Kind:     input.SourceFormat,
		Path:     input.SourcePath,
		URI:      input.SourceURI,
		RootURI:  input.SourceRootURI,
		NativeID: input.NativeSourceID,
	}
	revision := input.Revision
	revision.Source = source
	if revision.Generation == "" {
		revision.Generation = revision.ID
	}
	if revision.Identity == "" {
		revision.Identity = revision.ID
	}
	return CapturedBatch{
		SchemaVersion:       CapturedBatchSchemaVersion,
		ID:                  batchID,
		Provider:            input.Provider,
		Source:              source,
		SourceFormat:        input.SourceFormat,
		Revision:            revision,
		Range:               input.Range,
		ContentHash:         contentHash,
		RecordSchemaVersion: NativeRecordSchemaVersion,
		Records:             input.Records,
		Hints:               hints,
		Privacy:             input.Privacy,
		Checkpoint: SourceCheckpoint{
			SourceID:       sourceID,
			SourceRevision: revision.ID,
			NextByte:       input.Range.EndByte,
			NextRecord:     input.Range.EndRecord,
			ContentHash:    contentHash,
		},
	}, nil
}

func DefaultPrivacyStatus() PrivacyFilterStatus {
	return PrivacyFilterStatus{
		Applied:  true,
		PolicyID: DefaultPrivacyPolicyID,
		RecordPolicy: PrivacyPolicyRef{
			LosslessAfterFiltering: true,
			Stage:                  "pre_batch",
		},
	}
}

func FileSourceRevision(path string) (SourceRevision, error) {
	file, err := os.Open(path)
	if err != nil {
		return SourceRevision{}, err
	}
	defer file.Close()

	stat, err := file.Stat()
	if err != nil {
		return SourceRevision{}, err
	}
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return SourceRevision{}, err
	}
	contentHash := "sha256:" + hex.EncodeToString(hash.Sum(nil))
	return SourceRevision{
		Generation:  stableID("rev", "file", strconv.FormatInt(stat.Size(), 10), contentHash),
		Identity:    contentHash,
		ID:          stableID("rev", "file", strconv.FormatInt(stat.Size(), 10), contentHash),
		Kind:        "file_content",
		SizeBytes:   stat.Size(),
		ModifiedAt:  stat.ModTime().UTC(),
		ContentHash: contentHash,
	}, nil
}

func DetectRepoHints(path string) DeterministicHints {
	abs, err := filepath.Abs(path)
	if err != nil {
		abs = filepath.Clean(path)
	}
	dir := abs
	if info, err := os.Stat(abs); err == nil && !info.IsDir() {
		dir = filepath.Dir(abs)
	}
	for {
		gitPath := filepath.Join(dir, ".git")
		if _, err := os.Stat(gitPath); err == nil {
			hints := DeterministicHints{}
			if rel, err := filepath.Rel(dir, abs); err == nil {
				hints.RelativePath = filepath.ToSlash(rel)
			}
			hints.Commit = readGitHeadCommit(gitPath)
			sanitized, _ := sanitizeDeterministicHints(hints)
			return sanitized
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return DeterministicHints{}
		}
		dir = parent
	}
}

func NewRecord(ordinal, byteStart int64, raw, delimiter []byte) ProviderRecord {
	recordBytes := make([]byte, 0, len(raw)+len(delimiter))
	recordBytes = append(recordBytes, raw...)
	recordBytes = append(recordBytes, delimiter...)
	sum := sha256.Sum256(recordBytes)
	return ProviderRecord{
		Ordinal:     ordinal,
		ByteStart:   byteStart,
		ByteEnd:     byteStart + int64(len(recordBytes)),
		Raw:         append([]byte(nil), raw...),
		Delimiter:   append([]byte(nil), delimiter...),
		ContentHash: "sha256:" + hex.EncodeToString(sum[:]),
	}
}

func SourceURI(path string) string {
	abs, err := filepath.Abs(path)
	if err != nil {
		return filepath.ToSlash(filepath.Clean(path))
	}
	return filepath.ToSlash(abs)
}

func SourceRootURI(path string) string {
	if path == "" {
		return ""
	}
	return SourceURI(path)
}

func FormatCheckpoint(checkpoint SourceCheckpoint) string {
	return fmt.Sprintf("%s@%s:%d/%d", checkpoint.SourceID, checkpoint.SourceRevision, checkpoint.NextByte, checkpoint.NextRecord)
}

func stableID(prefix string, parts ...string) string {
	hash := sha256.New()
	for _, part := range parts {
		hash.Write([]byte(part))
		hash.Write([]byte{0})
	}
	sum := hex.EncodeToString(hash.Sum(nil))
	return prefix + "_" + sum[:32]
}

func batchContentHash(records []ProviderRecord) string {
	hash := sha256.New()
	for _, record := range records {
		hash.Write(record.Raw)
		hash.Write(record.Delimiter)
	}
	return "sha256:" + hex.EncodeToString(hash.Sum(nil))
}

func readGitHeadCommit(gitPath string) string {
	gitDir := gitDirPath(gitPath)
	headBytes, err := os.ReadFile(filepath.Join(gitDir, "HEAD"))
	if err != nil {
		return ""
	}
	head := strings.TrimSpace(string(headBytes))
	if refName, ok := strings.CutPrefix(head, "ref:"); ok {
		refName = strings.TrimSpace(refName)
		refBytes, err := os.ReadFile(filepath.Join(gitDir, filepath.FromSlash(refName)))
		if err == nil {
			return strings.TrimSpace(string(refBytes))
		}
		packed, err := os.ReadFile(filepath.Join(gitDir, "packed-refs"))
		if err != nil {
			return ""
		}
		for _, line := range strings.Split(string(packed), "\n") {
			fields := strings.Fields(line)
			if len(fields) == 2 && fields[1] == refName {
				return fields[0]
			}
		}
		return ""
	}
	return head
}

func sanitizeDeterministicHints(hints DeterministicHints) (DeterministicHints, bool) {
	sanitized := DeterministicHints{}
	changed := hints.RepoRoot != "" || hints.RepoRemote != ""
	if commit := sanitizeCommit(hints.Commit); commit != "" {
		sanitized.Commit = commit
	} else if hints.Commit != "" {
		changed = true
	}
	if relativePath := sanitizeRelativePath(hints.RelativePath); relativePath != "" {
		sanitized.RelativePath = relativePath
	} else if hints.RelativePath != "" {
		changed = true
	}
	return sanitized, changed
}

func sanitizeCommit(commit string) string {
	if len(commit) != 40 && len(commit) != 64 {
		return ""
	}
	for _, char := range commit {
		if (char >= '0' && char <= '9') || (char >= 'a' && char <= 'f') || (char >= 'A' && char <= 'F') {
			continue
		}
		return ""
	}
	return strings.ToLower(commit)
}

func sanitizeRelativePath(relativePath string) string {
	if relativePath == "" || strings.ContainsRune(relativePath, 0) {
		return ""
	}
	cleaned := path.Clean(strings.ReplaceAll(relativePath, "\\", "/"))
	if cleaned == "." || strings.HasPrefix(cleaned, "/") || cleaned == ".." || strings.HasPrefix(cleaned, "../") || strings.Contains(cleaned, ":") {
		return ""
	}
	return cleaned
}

func gitDirPath(gitPath string) string {
	data, err := os.ReadFile(gitPath)
	if err != nil {
		return gitPath
	}
	text := strings.TrimSpace(string(data))
	rel, ok := strings.CutPrefix(text, "gitdir:")
	if !ok {
		return gitPath
	}
	rel = strings.TrimSpace(rel)
	if filepath.IsAbs(rel) {
		return rel
	}
	return filepath.Clean(filepath.Join(filepath.Dir(gitPath), rel))
}
