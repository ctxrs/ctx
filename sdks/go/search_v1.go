package ctxagenthistory

import (
	"encoding/json"
	"fmt"
	"strings"
	"unicode"
	"unicode/utf8"
)

const (
	// SearchQueryVersion is the only structured search-query version accepted by this SDK.
	SearchQueryVersion = "ctx-search-v1"
	// SearchSchemaVersion is the nested ctx search response schema version.
	SearchSchemaVersion = 2

	searchMaxClauses                 = 32
	searchMaxClauseBytes             = 1024
	searchMaxTotalClauseBytes        = 8192
	searchMaxQueryJSONBytes          = 64 * 1024
	searchMaxAnalyzedTokensPerClause = 32
	searchMinLiteralBytes            = 3
	searchMaxLiteralBytes            = 256
	searchMaxResults                 = 200
)

// SearchClauseKind identifies one externally tagged ctx-search-v1 matcher.
type SearchClauseKind string

const (
	SearchClauseAll      SearchClauseKind = "all"
	SearchClausePhrase   SearchClauseKind = "phrase"
	SearchClauseLiteral  SearchClauseKind = "literal"
	SearchClauseSemantic SearchClauseKind = "semantic"
)

// SearchClause is one canonical externally tagged ctx-search-v1 matcher.
// Construct clauses with SearchAll, SearchPhrase, SearchLiteral, or SearchSemantic.
type SearchClause struct {
	kind  SearchClauseKind
	value string
}

func SearchAll(value string) SearchClause { return SearchClause{kind: SearchClauseAll, value: value} }
func SearchPhrase(value string) SearchClause {
	return SearchClause{kind: SearchClausePhrase, value: value}
}
func SearchLiteral(value string) SearchClause {
	return SearchClause{kind: SearchClauseLiteral, value: value}
}
func SearchSemantic(value string) SearchClause {
	return SearchClause{kind: SearchClauseSemantic, value: value}
}

// Kind returns the clause's matcher kind.
func (clause SearchClause) Kind() SearchClauseKind { return clause.kind }

// Value returns the clause's canonical matcher value.
func (clause SearchClause) Value() string { return clause.value }

func (clause SearchClause) MarshalJSON() ([]byte, error) {
	if !isSearchClauseKind(clause.kind) {
		return nil, fmt.Errorf("search clause has invalid matcher %q", clause.kind)
	}
	return json.Marshal(map[string]string{string(clause.kind): clause.value})
}

func (clause *SearchClause) UnmarshalJSON(data []byte) error {
	var object map[string]json.RawMessage
	if err := json.Unmarshal(data, &object); err != nil {
		return fmt.Errorf("decode search clause: %w", err)
	}
	if len(object) != 1 {
		return fmt.Errorf("search clause must contain exactly one matcher")
	}
	for rawKind, rawValue := range object {
		kind := SearchClauseKind(rawKind)
		if !isSearchClauseKind(kind) {
			return fmt.Errorf("search clause matcher %q is not supported", rawKind)
		}
		var value string
		if err := json.Unmarshal(rawValue, &value); err != nil {
			return fmt.Errorf("search clause %s value must be a string", rawKind)
		}
		clause.kind = kind
		clause.value = value
		return nil
	}
	return fmt.Errorf("search clause must contain one matcher")
}

// SearchQuery is the canonical ctx-search-v1 query DTO.
// Any clauses are alternatives; Must and MustNot accept lexical clauses only.
type SearchQuery struct {
	Version string         `json:"version"`
	Any     []SearchClause `json:"any,omitempty"`
	Must    []SearchClause `json:"must,omitempty"`
	MustNot []SearchClause `json:"must_not,omitempty"`
}

// NewSearchQuery creates a ctx-search-v1 query with the supplied alternatives.
func NewSearchQuery(any ...SearchClause) SearchQuery {
	return SearchQuery{Version: SearchQueryVersion, Any: any}
}

func (query *SearchQuery) UnmarshalJSON(data []byte) error {
	if len(data) > searchMaxQueryJSONBytes {
		return fmt.Errorf("search query JSON is %d bytes; maximum is %d", len(data), searchMaxQueryJSONBytes)
	}
	var object map[string]json.RawMessage
	if err := json.Unmarshal(data, &object); err != nil {
		return fmt.Errorf("decode ctx-search-v1 query: %w", err)
	}
	for field := range object {
		switch field {
		case "version", "any", "must", "must_not":
		default:
			return fmt.Errorf("search query contains unknown field %q", field)
		}
	}
	var wire struct {
		Version string         `json:"version"`
		Any     []SearchClause `json:"any"`
		Must    []SearchClause `json:"must"`
		MustNot []SearchClause `json:"must_not"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return fmt.Errorf("decode ctx-search-v1 query: %w", err)
	}
	canonical, err := (SearchQuery{
		Version: wire.Version,
		Any:     wire.Any,
		Must:    wire.Must,
		MustNot: wire.MustNot,
	}).Canonical()
	if err != nil {
		return err
	}
	*query = canonical
	return nil
}

// Canonical validates a query and returns its whitespace-normalized, de-duplicated form.
func (query SearchQuery) Canonical() (SearchQuery, error) {
	if query.Version != SearchQueryVersion {
		return SearchQuery{}, fmt.Errorf("search query version must be %s", SearchQueryVersion)
	}
	query.Any = canonicalSearchClauses(query.Any)
	query.Must = canonicalSearchClauses(query.Must)
	query.MustNot = canonicalSearchClauses(query.MustNot)

	clauseCount := len(query.Any) + len(query.Must) + len(query.MustNot)
	if clauseCount > searchMaxClauses {
		return SearchQuery{}, fmt.Errorf("search query has %d clauses; maximum is %d", clauseCount, searchMaxClauses)
	}
	if len(query.Any)+len(query.Must) == 0 {
		return SearchQuery{}, fmt.Errorf("search query needs a positive any or must clause")
	}
	semanticClauses := 0
	totalBytes := 0
	placements := []struct {
		name    string
		clauses []SearchClause
	}{
		{name: "any", clauses: query.Any},
		{name: "must", clauses: query.Must},
		{name: "must_not", clauses: query.MustNot},
	}
	for _, placement := range placements {
		clauses := placement.clauses
		for _, clause := range clauses {
			if !isSearchClauseKind(clause.kind) {
				return SearchQuery{}, fmt.Errorf("search clause has invalid matcher %q", clause.kind)
			}
			if placement.name != "any" && clause.kind == SearchClauseSemantic {
				return SearchQuery{}, fmt.Errorf("semantic clauses are allowed only in any")
			}
			if clause.kind == SearchClauseSemantic {
				semanticClauses++
			}
			valueBytes := len([]byte(clause.value))
			if valueBytes == 0 {
				return SearchQuery{}, fmt.Errorf("%s clause cannot be empty", clause.kind)
			}
			if valueBytes > searchMaxClauseBytes {
				return SearchQuery{}, fmt.Errorf("%s clause is %d bytes; maximum is %d", clause.kind, valueBytes, searchMaxClauseBytes)
			}
			if clause.kind == SearchClauseLiteral && (valueBytes < searchMinLiteralBytes || valueBytes > searchMaxLiteralBytes) {
				return SearchQuery{}, fmt.Errorf("literal clause is %d bytes; expected %d..=%d", valueBytes, searchMinLiteralBytes, searchMaxLiteralBytes)
			}
			tokens := searchAnalyzedTokenCount(clause.value)
			if tokens == 0 {
				return SearchQuery{}, fmt.Errorf("%s clause has no searchable tokens", clause.kind)
			}
			if tokens > searchMaxAnalyzedTokensPerClause {
				return SearchQuery{}, fmt.Errorf("%s clause has %d analyzed tokens; maximum is %d", clause.kind, tokens, searchMaxAnalyzedTokensPerClause)
			}
			totalBytes += valueBytes
		}
	}
	if semanticClauses > 1 {
		return SearchQuery{}, fmt.Errorf("search query has %d semantic clauses; maximum is 1", semanticClauses)
	}
	if totalBytes > searchMaxTotalClauseBytes {
		return SearchQuery{}, fmt.Errorf("search query has %d clause bytes; maximum is %d", totalBytes, searchMaxTotalClauseBytes)
	}
	return query, nil
}

// SerializeSearchQuery validates and serializes one canonical query for --query-json.
func SerializeSearchQuery(query SearchQuery) (string, error) {
	canonical, err := query.Canonical()
	if err != nil {
		return "", err
	}
	data, err := json.Marshal(canonical)
	if err != nil {
		return "", fmt.Errorf("encode ctx-search-v1 query: %w", err)
	}
	if len(data) > searchMaxQueryJSONBytes {
		return "", fmt.Errorf("search query JSON is %d bytes; maximum is %d", len(data), searchMaxQueryJSONBytes)
	}
	return string(data), nil
}

func (result *SearchResult) UnmarshalJSON(data []byte) error {
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(data, &fields); err != nil {
		return err
	}
	var schemaVersion int
	if raw, ok := fields["schema_version"]; !ok {
		return fmt.Errorf("ctx search response is missing schema_version")
	} else if err := json.Unmarshal(raw, &schemaVersion); err != nil || schemaVersion != SearchSchemaVersion {
		return fmt.Errorf("ctx search response schema_version must be %d", SearchSchemaVersion)
	}
	if raw, ok := fields["query_execution"]; !ok || string(raw) == "null" {
		return fmt.Errorf("ctx search response is missing query_execution")
	}
	type searchResultWire SearchResult
	var wire searchResultWire
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	*result = SearchResult(wire)
	if retrieval, ok := result.Retrieval.(map[string]any); ok {
		delete(retrieval, "semantic_weight")
		delete(retrieval, "semantic_fallback_code")
		delete(retrieval, "semantic_fallback")
		delete(retrieval, "semanticWeight")
		delete(retrieval, "semanticFallbackCode")
		delete(retrieval, "semanticFallback")
	}
	return nil
}

func canonicalSearchClauses(clauses []SearchClause) []SearchClause {
	canonical := make([]SearchClause, 0, len(clauses))
	seen := make(map[string]struct{}, len(clauses))
	for _, clause := range clauses {
		value := trimSearchWhitespace(clause.value)
		if clause.kind != SearchClauseLiteral {
			value = strings.Join(strings.FieldsFunc(value, isSearchWhitespace), " ")
		}
		clause.value = value
		key := string(clause.kind) + "\x00" + value
		if _, duplicate := seen[key]; duplicate {
			continue
		}
		seen[key] = struct{}{}
		canonical = append(canonical, clause)
	}
	return canonical
}

func isSearchClauseKind(kind SearchClauseKind) bool {
	return kind == SearchClauseAll || kind == SearchClausePhrase || kind == SearchClauseLiteral || kind == SearchClauseSemantic
}

func searchAnalyzedTokenCount(value string) int {
	if !utf8.ValidString(value) {
		return 0
	}
	count := 0
	inToken := false
	for _, current := range value {
		if unicode.IsLetter(current) || unicode.IsNumber(current) || (inToken && isSearchUnicodeMark(current)) {
			if !inToken {
				count++
				inToken = true
			}
		} else {
			inToken = false
		}
	}
	return count
}

func trimSearchWhitespace(value string) string {
	return strings.TrimFunc(value, isSearchWhitespace)
}

func isSearchWhitespace(current rune) bool {
	return current >= '\u0009' && current <= '\u000d' ||
		current == '\u0020' || current == '\u0085' || current == '\u00a0' ||
		current == '\u1680' || current >= '\u2000' && current <= '\u200a' ||
		current == '\u2028' || current == '\u2029' || current == '\u202f' ||
		current == '\u205f' || current == '\u3000'
}

func isSearchUnicodeMark(current rune) bool {
	return current >= '\u0300' && current <= '\u036f' ||
		current >= '\u1ab0' && current <= '\u1aff' ||
		current >= '\u1dc0' && current <= '\u1dff' ||
		current >= '\u20d0' && current <= '\u20ff' ||
		current >= '\ufe20' && current <= '\ufe2f' ||
		current == '\u200c' || current == '\u200d'
}
