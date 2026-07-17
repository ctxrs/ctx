package capture

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"strconv"
)

type CaptureOptions struct {
	SourceID       string
	SourceURI      string
	SourceRootURI  string
	NativeSourceID string
	RepoHints      *DeterministicHints
	StartByte      int64
	StartRecord    int64
	MaxRecords     int
	PrivacyFilter  PrivacyFilter
	openFile       func(string) (readSeekStatCloser, error)
}

type JSONLAdapter struct {
	Provider       Provider
	SourceFormat   string
	ClassifyRecord func(json.RawMessage) RecordClassification
}

type RecordClassification struct {
	Kind           string
	NativeID       string
	NativeSourceID string
	Hints          map[string]string
}

type PrivacyFilter interface {
	PolicyID() string
	AllowSource(SourceCandidate) PrivacyDecision
	AllowRecord(RecordCandidate) PrivacyDecision
}

type SourceCandidate struct {
	Provider     Provider
	SourceFormat string
	URI          string
	RootURI      string
}

type RecordCandidate struct {
	Provider     Provider
	SourceFormat string
	Ordinal      int64
	ByteStart    int64
	Raw          []byte
	Delimiter    []byte
}

type PrivacyDecision struct {
	Allow  bool
	Reason string
}

type AllowAllPrivacyFilter struct {
	ID string
}

type readSeekStatCloser interface {
	io.Reader
	io.Seeker
	io.Closer
	Stat() (os.FileInfo, error)
}

func (filter AllowAllPrivacyFilter) PolicyID() string {
	if filter.ID == "" {
		return DefaultPrivacyPolicyID
	}
	return filter.ID
}

func (filter AllowAllPrivacyFilter) AllowSource(SourceCandidate) PrivacyDecision {
	return PrivacyDecision{Allow: true}
}

func (filter AllowAllPrivacyFilter) AllowRecord(RecordCandidate) PrivacyDecision {
	return PrivacyDecision{Allow: true}
}

func (adapter JSONLAdapter) CaptureFile(ctx context.Context, path string, options CaptureOptions) (*CapturedBatch, error) {
	if adapter.Provider == "" || adapter.SourceFormat == "" {
		return nil, errors.New("capture: provider and source format are required")
	}
	privacy := options.PrivacyFilter
	if privacy == nil {
		privacy = AllowAllPrivacyFilter{}
	}
	sourceURI := options.SourceURI
	if sourceURI == "" {
		sourceURI = SourceURI(path)
	}
	source := SourceCandidate{
		Provider:     adapter.Provider,
		SourceFormat: adapter.SourceFormat,
		URI:          sourceURI,
		RootURI:      options.SourceRootURI,
	}
	sourceDecision := privacy.AllowSource(source)
	status := DefaultPrivacyStatus()
	status.PolicyID = privacy.PolicyID()
	if !sourceDecision.Allow {
		status.SourceExcluded = true
		status.Reasons = incrementReason(status.Reasons, sourceDecision.Reason)
		return nil, nil
	}

	opener := options.openFile
	if opener == nil {
		opener = func(path string) (readSeekStatCloser, error) {
			return os.Open(path)
		}
	}
	file, err := opener(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	return adapter.captureOpenFile(ctx, file, path, sourceURI, options, privacy, status)
}

func (adapter JSONLAdapter) captureOpenFile(ctx context.Context, file readSeekStatCloser, path, sourceURI string, options CaptureOptions, privacy PrivacyFilter, status PrivacyFilterStatus) (*CapturedBatch, error) {
	stat, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if options.StartByte < 0 {
		return nil, errors.New("capture: start byte must be non-negative")
	}
	if options.StartByte > stat.Size() {
		return nil, errors.New("capture: start byte is beyond source size")
	}
	if options.StartByte > 0 {
		if _, err := file.Seek(options.StartByte, io.SeekStart); err != nil {
			return nil, err
		}
	}

	reader := bufio.NewReader(file)
	records := make([]ProviderRecord, 0)
	nativeSourceID := options.NativeSourceID
	byteOffset := options.StartByte
	ordinal := options.StartRecord
	for {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		line, readErr := reader.ReadBytes('\n')
		if len(line) == 0 && readErr == io.EOF {
			break
		}
		if readErr != nil && readErr != io.EOF {
			return nil, readErr
		}
		raw, delimiter := splitJSONLRecord(line)
		recordLength := int64(len(raw) + len(delimiter))
		decision := privacy.AllowRecord(RecordCandidate{
			Provider:     adapter.Provider,
			SourceFormat: adapter.SourceFormat,
			Ordinal:      ordinal,
			ByteStart:    byteOffset,
			Raw:          raw,
			Delimiter:    delimiter,
		})
		if !decision.Allow {
			status.FilteredRecords++
			status.FilteredBytes += recordLength
			status.Reasons = incrementReason(status.Reasons, decision.Reason)
			byteOffset += recordLength
			ordinal++
			if readErr == io.EOF {
				break
			}
			continue
		}
		record := NewRecord(ordinal, byteOffset, raw, delimiter)
		trimmed := bytes.TrimSpace(raw)
		if !json.Valid(trimmed) {
			record.Malformed = true
			record.ParseError = "malformed JSON record"
		} else if adapter.ClassifyRecord != nil {
			classification := adapter.ClassifyRecord(json.RawMessage(append([]byte(nil), trimmed...)))
			record.Kind = classification.Kind
			record.NativeID = classification.NativeID
			record.Hints = classification.Hints
			if nativeSourceID == "" && classification.NativeSourceID != "" {
				nativeSourceID = classification.NativeSourceID
			}
		}
		records = append(records, record)
		byteOffset += recordLength
		ordinal++
		if options.MaxRecords > 0 && len(records) >= options.MaxRecords {
			break
		}
		if readErr == io.EOF {
			break
		}
	}
	if len(records) == 0 {
		return nil, nil
	}

	var revision SourceRevision
	if options.StartByte > 0 {
		revision = observedSliceRevision(stat, records[0].ByteStart, records[len(records)-1].ByteEnd, batchContentHash(records))
	} else {
		revision, err = FileSourceRevision(path)
		if err != nil {
			return nil, err
		}
	}

	hints := DetectRepoHints(path)
	if options.RepoHints != nil {
		hints = *options.RepoHints
	}
	batch, err := NewCapturedBatch(BatchInput{
		Provider:       adapter.Provider,
		SourceFormat:   adapter.SourceFormat,
		SourceID:       options.SourceID,
		SourceURI:      sourceURI,
		SourceRootURI:  options.SourceRootURI,
		SourcePath:     path,
		NativeSourceID: nativeSourceID,
		Revision:       revision,
		Range: RecordRange{
			StartByte:   records[0].ByteStart,
			EndByte:     records[len(records)-1].ByteEnd,
			StartRecord: records[0].Ordinal,
			EndRecord:   records[len(records)-1].Ordinal + 1,
			RecordCount: len(records),
		},
		Records: records,
		Hints:   hints,
		Privacy: status,
	})
	if err != nil {
		return nil, err
	}
	return &batch, nil
}

func observedSliceRevision(stat os.FileInfo, startByte, endByte int64, contentHash string) SourceRevision {
	if endByte < startByte {
		endByte = startByte
	}
	sizeBytes := endByte - startByte
	return SourceRevision{
		Generation: stableID(
			"rev",
			"file_slice",
			strconv.FormatInt(stat.Size(), 10),
			strconv.FormatInt(stat.ModTime().UTC().UnixNano(), 10),
			strconv.FormatInt(startByte, 10),
			strconv.FormatInt(endByte, 10),
			contentHash,
		),
		Identity:    contentHash,
		ID:          stableID("rev", "file_slice", strconv.FormatInt(startByte, 10), strconv.FormatInt(endByte, 10), contentHash),
		Kind:        "file_slice_content",
		SizeBytes:   sizeBytes,
		ModifiedAt:  stat.ModTime().UTC(),
		ContentHash: contentHash,
	}
}

func splitJSONLRecord(line []byte) ([]byte, []byte) {
	if len(line) == 0 {
		return nil, nil
	}
	if line[len(line)-1] != '\n' {
		return append([]byte(nil), line...), nil
	}
	if len(line) >= 2 && line[len(line)-2] == '\r' {
		return append([]byte(nil), line[:len(line)-2]...), []byte("\r\n")
	}
	return append([]byte(nil), line[:len(line)-1]...), []byte("\n")
}

func incrementReason(reasons map[string]int, reason string) map[string]int {
	if reason == "" {
		reason = "unspecified"
	}
	if reasons == nil {
		reasons = make(map[string]int)
	}
	reasons[reason]++
	return reasons
}
