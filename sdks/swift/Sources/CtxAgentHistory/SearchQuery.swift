import Foundation

public let CTX_SEARCH_V1_VERSION = "ctx-search-v1"

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
        try validate()
    }

    public static func all(_ value: String) throws -> SearchQueryV1 { try SearchQueryV1(any: [.all(value)]) }

    public func validate() throws {
        guard version == CTX_SEARCH_V1_VERSION else { throw invalid("search query version must be ctx-search-v1") }
        guard !any.isEmpty || !must.isEmpty else { throw invalid("search query needs a positive any or must clause") }
        let placements = [("any", any), ("must", must), ("must_not", mustNot)]
        var count = 0; var totalBytes = 0; var semanticCount = 0
        for (placement, clauses) in placements {
            for clause in clauses {
                if placement != "any", clause.matcher == "semantic" { throw invalid("semantic clauses are allowed only in any") }
                if clause.matcher == "semantic" { semanticCount += 1 }
                guard semanticCount <= 1 else { throw invalid("search query allows at most one semantic clause in any") }
                guard !clause.value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw invalid("search clause value must be a non-empty string") }
                let bytes = clause.value.lengthOfBytes(using: .utf8)
                guard bytes <= Self.maxClauseBytes else { throw invalid("search clause exceeds the 1024-byte limit") }
                if clause.matcher == "literal", !(Self.minLiteralBytes ... Self.maxLiteralBytes).contains(bytes) {
                    throw invalid("literal search clause must be between 3 and 256 bytes")
                }
                count += 1; totalBytes += bytes
            }
        }
        guard count <= Self.maxClauses else { throw invalid("search query exceeds the 32-clause limit") }
        guard totalBytes <= Self.maxTotalClauseBytes else { throw invalid("search query exceeds the 8192-byte clause limit") }
    }

    public func jsonString() throws -> String {
        try validate()
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(self)
        guard data.count <= Self.maxJSONBytes else { throw invalid("search query JSON exceeds the 65536-byte limit") }
        return String(decoding: data, as: UTF8.self)
    }

    enum CodingKeys: String, CodingKey { case version, any, must; case mustNot = "must_not" }

    public func encode(to encoder: Encoder) throws {
        try validate()
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        if !any.isEmpty { try container.encode(any, forKey: .any) }
        if !must.isEmpty { try container.encode(must, forKey: .must) }
        if !mustNot.isEmpty { try container.encode(mustNot, forKey: .mustNot) }
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: SearchCodingKey.self)
        let allowed = Set(["version", "any", "must", "must_not"])
        guard let unknown = container.allKeys.first(where: { !allowed.contains($0.stringValue) }) else {
            version = try container.decode(String.self, forKey: SearchCodingKey("version"))
            any = try container.decodeIfPresent([SearchClause].self, forKey: SearchCodingKey("any")) ?? []
            must = try container.decodeIfPresent([SearchClause].self, forKey: SearchCodingKey("must")) ?? []
            mustNot = try container.decodeIfPresent([SearchClause].self, forKey: SearchCodingKey("must_not")) ?? []
            try validate()
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
    public let hydratedRows, legacyFallbackRows, hydrationInputBytes, largestHydrationInputBytes, snippetInputBytes: Int
    public let returnedResults, returnedTextBytes, serializedResponseBytes, elapsedMs: Int
    enum CodingKeys: String, CodingKey { case queryBytes = "query_bytes", clauses, analyzedTokens = "analyzed_tokens", largestAnalyzedTokensPerClause = "largest_analyzed_tokens_per_clause", largestPositiveSeedCandidates = "largest_positive_seed_candidates", candidateRows = "candidate_rows", retainedCandidateIds = "retained_candidate_ids", residualRows = "residual_rows", verificationBytes = "verification_bytes", largestVerificationLookupBytes = "largest_verification_lookup_bytes", hydratedRows = "hydrated_rows", legacyFallbackRows = "legacy_fallback_rows", hydrationInputBytes = "hydration_input_bytes", largestHydrationInputBytes = "largest_hydration_input_bytes", snippetInputBytes = "snippet_input_bytes", returnedResults = "returned_results", returnedTextBytes = "returned_text_bytes", serializedResponseBytes = "serialized_response_bytes", elapsedMs = "elapsed_ms" }
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
