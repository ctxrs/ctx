import Foundation

public let CTX_SEARCH_V1_VERSION = "ctx-search-v1"
public let CTX_SEARCH_MAX_RESULTS = 200

public enum SearchClause: Codable, Equatable, Sendable {
    case all(String)
    case phrase(String)
    case literal(String)
    case semantic(String)

    public var matcher: String {
        switch self { case .all: return "all"; case .phrase: return "phrase"; case .literal: return "literal"; case .semantic: return "semantic" }
    }

    public var value: String {
        switch self { case let .all(v), let .phrase(v), let .literal(v), let .semantic(v): return v }
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: SearchCodingKey.self)
        guard container.allKeys.count == 1, let key = container.allKeys.first else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "search clause must contain exactly one matcher"))
        }
        let value = try container.decode(String.self, forKey: key)
        switch key.stringValue {
        case "all": self = .all(value)
        case "phrase": self = .phrase(value)
        case "literal": self = .literal(value)
        case "semantic": self = .semantic(value)
        default: throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "unknown search matcher"))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: SearchCodingKey.self)
        try container.encode(value, forKey: SearchCodingKey(matcher))
    }
}

public struct SearchQueryV1: Codable, Equatable, Sendable {
    public static let maxClauses = 32
    public static let maxClauseBytes = 1_024
    public static let maxTotalClauseBytes = 8_192
    public static let maxJSONBytes = 64 * 1_024
    public static let maxAnalyzedTokensPerClause = 32
    public static let minLiteralBytes = 3
    public static let maxLiteralBytes = 256

    public var version: String
    public var any: [SearchClause]
    public var must: [SearchClause]
    public var mustNot: [SearchClause]

    public init(
        version: String = CTX_SEARCH_V1_VERSION,
        any: [SearchClause] = [],
        must: [SearchClause] = [],
        mustNot: [SearchClause] = []
    ) throws {
        self.version = version; self.any = any; self.must = must; self.mustNot = mustNot
        self = try canonicalized()
    }

    public static func all(_ value: String) throws -> SearchQueryV1 { try SearchQueryV1(any: [.all(value)]) }

    public func validate() throws {
        _ = try canonicalized()
    }

    private func canonicalized() throws -> SearchQueryV1 {
        var canonical = self
        canonical.any = canonicalizeSearchClauses(any)
        canonical.must = canonicalizeSearchClauses(must)
        canonical.mustNot = canonicalizeSearchClauses(mustNot)
        try canonical.validateCanonical()
        return canonical
    }

    private func validateCanonical() throws {
        guard version == CTX_SEARCH_V1_VERSION else { throw invalid("search query version must be ctx-search-v1") }
        guard !any.isEmpty || !must.isEmpty else { throw invalid("search query needs a positive any or must clause") }
        let placements = [("any", any), ("must", must), ("must_not", mustNot)]
        var count = 0; var totalBytes = 0; var semanticCount = 0
        for (placement, clauses) in placements {
            for clause in clauses {
                if placement != "any", clause.matcher == "semantic" { throw invalid("semantic clauses are allowed only in any") }
                if clause.matcher == "semantic" { semanticCount += 1 }
                guard semanticCount <= 1 else { throw invalid("search query allows at most one semantic clause in any") }
                let bytes = clause.value.lengthOfBytes(using: .utf8)
                guard bytes > 0 else { throw invalid("search clause cannot be empty") }
                guard bytes <= Self.maxClauseBytes else { throw invalid("search clause exceeds the 1024-byte limit") }
                if clause.matcher == "literal", !(Self.minLiteralBytes ... Self.maxLiteralBytes).contains(bytes) {
                    throw invalid("literal search clause must be between 3 and 256 bytes")
                }
                let analyzedTokens = searchAnalyzedTokenCount(clause.value)
                guard analyzedTokens > 0 else { throw invalid("search clause has no searchable tokens") }
                guard analyzedTokens <= Self.maxAnalyzedTokensPerClause else {
                    throw invalid("search clause exceeds the 32 analyzed-token limit")
                }
                count += 1; totalBytes += bytes
            }
        }
        guard count <= Self.maxClauses else { throw invalid("search query exceeds the 32-clause limit") }
        guard totalBytes <= Self.maxTotalClauseBytes else { throw invalid("search query exceeds the 8192-byte clause limit") }
    }

    public func jsonString() throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(self)
        guard data.count <= Self.maxJSONBytes else { throw invalid("search query JSON exceeds the 65536-byte limit") }
        return String(decoding: data, as: UTF8.self)
    }

    enum CodingKeys: String, CodingKey { case version, any, must; case mustNot = "must_not" }

    public func encode(to encoder: Encoder) throws {
        let canonical = try canonicalized()
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(canonical.version, forKey: .version)
        if !canonical.any.isEmpty { try container.encode(canonical.any, forKey: .any) }
        if !canonical.must.isEmpty { try container.encode(canonical.must, forKey: .must) }
        if !canonical.mustNot.isEmpty { try container.encode(canonical.mustNot, forKey: .mustNot) }
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: SearchCodingKey.self)
        let allowed = Set(["version", "any", "must", "must_not"])
        guard let unknown = container.allKeys.first(where: { !allowed.contains($0.stringValue) }) else {
            do {
                self = try SearchQueryV1(
                    version: container.decode(String.self, forKey: SearchCodingKey("version")),
                    any: container.decodeIfPresent([SearchClause].self, forKey: SearchCodingKey("any")) ?? [],
                    must: container.decodeIfPresent([SearchClause].self, forKey: SearchCodingKey("must")) ?? [],
                    mustNot: container.decodeIfPresent([SearchClause].self, forKey: SearchCodingKey("must_not")) ?? []
                )
            } catch {
                throw DecodingError.dataCorrupted(
                    .init(codingPath: decoder.codingPath, debugDescription: "invalid ctx-search-v1 query: \(error)")
                )
            }
            return
        }
        throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "unknown search query field \(unknown.stringValue)"))
    }
}

private struct SearchCodingKey: CodingKey {
    let stringValue: String; let intValue: Int? = nil
    init(_ value: String) { stringValue = value }
    init?(stringValue: String) { self.init(stringValue) }
    init?(intValue: Int) { return nil }
}

private func invalid(_ message: String) -> CtxAgentHistorySDKError {
    CtxAgentHistorySDKError(code: .invalidRequest, message: message)
}

private func canonicalizeSearchClauses(_ clauses: [SearchClause]) -> [SearchClause] {
    var canonical: [SearchClause] = []
    for clause in clauses {
        let value = canonicalSearchValue(clause.value, preserveInteriorWhitespace: clause.matcher == "literal")
        let normalized: SearchClause
        switch clause {
        case .all: normalized = .all(value)
        case .phrase: normalized = .phrase(value)
        case .literal: normalized = .literal(value)
        case .semantic: normalized = .semantic(value)
        }
        if !canonical.contains(normalized) {
            canonical.append(normalized)
        }
    }
    return canonical
}

private func canonicalSearchValue(_ value: String, preserveInteriorWhitespace: Bool) -> String {
    let scalars = value.unicodeScalars
    guard let first = scalars.firstIndex(where: { !isSearchWhitespace($0.value) }),
          let last = scalars.lastIndex(where: { !isSearchWhitespace($0.value) })
    else {
        return ""
    }
    let trimmed = String(scalars[first ... last])
    guard !preserveInteriorWhitespace else {
        return trimmed
    }
    return trimmed.unicodeScalars
        .split(whereSeparator: { isSearchWhitespace($0.value) })
        .map(String.init)
        .joined(separator: " ")
}

private func searchAnalyzedTokenCount(_ value: String) -> Int {
    var count = 0
    var inToken = false
    for scalar in value.unicodeScalars {
        let continuesToken = CharacterSet.alphanumerics.contains(scalar)
            || (inToken && isSearchContinuationMark(scalar.value))
        if continuesToken {
            if !inToken {
                count += 1
            }
            inToken = true
        } else {
            inToken = false
        }
    }
    return count
}

private func isSearchWhitespace(_ scalar: UInt32) -> Bool {
    (0x0009 ... 0x000D).contains(scalar)
        || scalar == 0x0020
        || scalar == 0x0085
        || scalar == 0x00A0
        || scalar == 0x1680
        || (0x2000 ... 0x200A).contains(scalar)
        || scalar == 0x2028
        || scalar == 0x2029
        || scalar == 0x202F
        || scalar == 0x205F
        || scalar == 0x3000
}

private func isSearchContinuationMark(_ scalar: UInt32) -> Bool {
    (0x0300 ... 0x036F).contains(scalar)
        || (0x1AB0 ... 0x1AFF).contains(scalar)
        || (0x1DC0 ... 0x1DFF).contains(scalar)
        || (0x20D0 ... 0x20FF).contains(scalar)
        || (0xFE20 ... 0xFE2F).contains(scalar)
        || scalar == 0x200C
        || scalar == 0x200D
}

public struct SearchExecutionLimits: Codable, Equatable, Sendable {
    public let queryBytes, clauses, analyzedTokensPerClause, candidatesPerPositiveSeed: Int
    public let candidateRows, retainedCandidateIds, residualRows, verificationBytes, verificationLookupBytes: Int
    public let hydratedRows, hydrationInputBytes, hydrationInputBytesPerEvent, snippetInputBytes: Int
    public let returnedTextBytes, serializedResponseBytes, results, elapsedMs: Int
    enum CodingKeys: String, CodingKey { case queryBytes = "query_bytes", clauses, analyzedTokensPerClause = "analyzed_tokens_per_clause", candidatesPerPositiveSeed = "candidates_per_positive_seed", candidateRows = "candidate_rows", retainedCandidateIds = "retained_candidate_ids", residualRows = "residual_rows", verificationBytes = "verification_bytes", verificationLookupBytes = "verification_lookup_bytes", hydratedRows = "hydrated_rows", hydrationInputBytes = "hydration_input_bytes", hydrationInputBytesPerEvent = "hydration_input_bytes_per_event", snippetInputBytes = "snippet_input_bytes", returnedTextBytes = "returned_text_bytes", serializedResponseBytes = "serialized_response_bytes", results, elapsedMs = "elapsed_ms" }
}

public struct SearchExecutionConsumption: Codable, Equatable, Sendable {
    public let queryBytes, clauses, analyzedTokens, largestAnalyzedTokensPerClause, largestPositiveSeedCandidates: Int
    public let candidateRows, retainedCandidateIds, residualRows, verificationBytes, largestVerificationLookupBytes: Int
    public let hydratedRows, hydrationInputBytes, largestHydrationInputBytes, snippetInputBytes: Int
    public let returnedResults, returnedTextBytes, serializedResponseBytes, elapsedMs: Int
    enum CodingKeys: String, CodingKey { case queryBytes = "query_bytes", clauses, analyzedTokens = "analyzed_tokens", largestAnalyzedTokensPerClause = "largest_analyzed_tokens_per_clause", largestPositiveSeedCandidates = "largest_positive_seed_candidates", candidateRows = "candidate_rows", retainedCandidateIds = "retained_candidate_ids", residualRows = "residual_rows", verificationBytes = "verification_bytes", largestVerificationLookupBytes = "largest_verification_lookup_bytes", hydratedRows = "hydrated_rows", hydrationInputBytes = "hydration_input_bytes", largestHydrationInputBytes = "largest_hydration_input_bytes", snippetInputBytes = "snippet_input_bytes", returnedResults = "returned_results", returnedTextBytes = "returned_text_bytes", serializedResponseBytes = "serialized_response_bytes", elapsedMs = "elapsed_ms" }
}

public struct SearchSemanticCoverage: Codable, Equatable, Sendable {
    public let indexedDocuments, searchableDocuments: Int?
    enum CodingKeys: String, CodingKey { case indexedDocuments = "indexed_documents", searchableDocuments = "searchable_documents" }
}

public struct SearchSemanticExecution: Codable, Equatable, Sendable {
    public let attempted, required: Bool
    public let readiness, effectiveBackend: String
    public let backend: String?
    public let requestedCandidates, eligibleCandidates, candidatesSupplied, candidatesConsumed, candidatesUsed: Int
    public let coverage: SearchSemanticCoverage
    public let completeness: String
    public let incompletenessReasons: [String]?
    public let skipReason: String?
    public let positiveTextRuleVersion: String
    enum CodingKeys: String, CodingKey { case attempted, required, readiness, backend, coverage, completeness; case effectiveBackend = "effective_backend", requestedCandidates = "requested_candidates", eligibleCandidates = "eligible_candidates", candidatesSupplied = "candidates_supplied", candidatesConsumed = "candidates_consumed", candidatesUsed = "candidates_used", incompletenessReasons = "incompleteness_reasons", skipReason = "skip_reason", positiveTextRuleVersion = "positive_text_rule_version" }
}

public struct SearchQueryExecution: Codable, Equatable, Sendable {
    public let queryVersion, candidateStrategy: String
    public let resolved: SearchExecutionLimits
    public let consumed: SearchExecutionConsumption
    public let semantic: SearchSemanticExecution
    public let rrfK, perBranchCandidateRows, requestedResultLimit, resultLimit, maxResultLimit: Int
    public let clausesExecuted, verificationDropped, filterVerificationDropped: Int
    public let candidateBudgetExhausted, timedOut, truncated: Bool
    public let truncationReasons: [String]?
    enum CodingKeys: String, CodingKey { case queryVersion = "query_version", candidateStrategy = "candidate_strategy", resolved, consumed, semantic, rrfK = "rrf_k", perBranchCandidateRows = "per_branch_candidate_rows", requestedResultLimit = "requested_result_limit", resultLimit = "result_limit", maxResultLimit = "max_result_limit", clausesExecuted = "clauses_executed", verificationDropped = "verification_dropped", filterVerificationDropped = "filter_verification_dropped", candidateBudgetExhausted = "candidate_budget_exhausted", timedOut = "timed_out", truncated, truncationReasons = "truncation_reasons" }
}
