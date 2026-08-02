import Foundation

public struct AgentHistoryStatus: Codable, Equatable, Sendable {
    public static let maximumExactCounter = 9_007_199_254_740_991

    public var initialized: Bool
    public var localOnly: Bool
    public var readOnly: Bool?
    public var dataRoot: String?
    public var indexedItems: Int?
    public var indexedSessions: Int?
    public var indexedEvents: Int?
    public var indexedSources: Int?
    public var historyEpoch: JSONValue?
    public var lexical: JSONValue?
    public var refresh: JSONValue?
    public var semantic: JSONValue?
    public var daemon: JSONValue?

    public init(
        initialized: Bool,
        localOnly: Bool,
        readOnly: Bool? = nil,
        dataRoot: String? = nil,
        indexedItems: Int? = nil,
        indexedSessions: Int? = nil,
        indexedEvents: Int? = nil,
        indexedSources: Int? = nil,
        historyEpoch: JSONValue? = nil,
        lexical: JSONValue? = nil,
        refresh: JSONValue? = nil,
        semantic: JSONValue? = nil,
        daemon: JSONValue? = nil
    ) {
        self.initialized = initialized
        self.localOnly = localOnly
        self.readOnly = readOnly
        self.dataRoot = dataRoot
        self.indexedItems = indexedItems
        self.indexedSessions = indexedSessions
        self.indexedEvents = indexedEvents
        self.indexedSources = indexedSources
        self.historyEpoch = historyEpoch
        self.lexical = lexical
        self.refresh = refresh
        self.semantic = semantic
        self.daemon = daemon
    }

    private enum CodingKeys: String, CodingKey {
        case initialized
        case localOnly
        case readOnly
        case dataRoot
        case indexedItems
        case indexedSessions
        case indexedEvents
        case indexedSources
        case historyEpoch
        case lexical
        case refresh
        case semantic
        case daemon
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        initialized = try container.decode(Bool.self, forKey: .initialized)
        localOnly = try container.decode(Bool.self, forKey: .localOnly)
        readOnly = try container.decodeIfPresent(Bool.self, forKey: .readOnly)
        dataRoot = try container.decodeIfPresent(String.self, forKey: .dataRoot)
        indexedItems = try Self.decodeCounter(.indexedItems, from: container)
        indexedSessions = try Self.decodeCounter(.indexedSessions, from: container)
        indexedEvents = try Self.decodeCounter(.indexedEvents, from: container)
        indexedSources = try Self.decodeCounter(.indexedSources, from: container)
        historyEpoch = try container.decodeIfPresent(JSONValue.self, forKey: .historyEpoch)
        lexical = try container.decodeIfPresent(JSONValue.self, forKey: .lexical)
        refresh = try container.decodeIfPresent(JSONValue.self, forKey: .refresh)
        semantic = try container.decodeIfPresent(JSONValue.self, forKey: .semantic)
        daemon = try container.decodeIfPresent(JSONValue.self, forKey: .daemon)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(initialized, forKey: .initialized)
        try container.encode(localOnly, forKey: .localOnly)
        try container.encodeIfPresent(readOnly, forKey: .readOnly)
        try container.encodeIfPresent(dataRoot, forKey: .dataRoot)
        try Self.encodeCounter(indexedItems, forKey: .indexedItems, to: &container)
        try Self.encodeCounter(indexedSessions, forKey: .indexedSessions, to: &container)
        try Self.encodeCounter(indexedEvents, forKey: .indexedEvents, to: &container)
        try Self.encodeCounter(indexedSources, forKey: .indexedSources, to: &container)
        try container.encodeIfPresent(historyEpoch, forKey: .historyEpoch)
        try container.encodeIfPresent(lexical, forKey: .lexical)
        try container.encodeIfPresent(refresh, forKey: .refresh)
        try container.encodeIfPresent(semantic, forKey: .semantic)
        try container.encodeIfPresent(daemon, forKey: .daemon)
    }

    private static func decodeCounter(
        _ key: CodingKeys,
        from container: KeyedDecodingContainer<CodingKeys>
    ) throws -> Int? {
        guard let value = try container.decodeIfPresent(Int.self, forKey: key) else {
            return nil
        }
        guard value >= 0, value <= maximumExactCounter else {
            throw DecodingError.dataCorruptedError(
                forKey: key,
                in: container,
                debugDescription: "status counter exceeds maximum \(maximumExactCounter)"
            )
        }
        return value
    }

    private static func encodeCounter(
        _ value: Int?,
        forKey key: CodingKeys,
        to container: inout KeyedEncodingContainer<CodingKeys>
    ) throws {
        if let value, value < 0 || value > maximumExactCounter {
            throw EncodingError.invalidValue(
                value,
                EncodingError.Context(
                    codingPath: container.codingPath + [key],
                    debugDescription: "status counter exceeds maximum \(maximumExactCounter)"
                )
            )
        }
        try container.encodeIfPresent(value, forKey: key)
    }
}

public struct ProviderSource: Codable, Equatable, Sendable {
    public var provider: String
    public var path: String
    public var exists: Bool?
    public var sourceFormat: String?
    public var status: String
    public var importSupport: String?
    public var nativeImport: Bool?
    public var importable: Bool
    public var unsupportedReason: String?

    public init(
        provider: String,
        path: String,
        exists: Bool? = nil,
        sourceFormat: String? = nil,
        status: String,
        importSupport: String? = nil,
        nativeImport: Bool? = nil,
        importable: Bool,
        unsupportedReason: String? = nil
    ) {
        self.provider = provider
        self.path = path
        self.exists = exists
        self.sourceFormat = sourceFormat
        self.status = status
        self.importSupport = importSupport
        self.nativeImport = nativeImport
        self.importable = importable
        self.unsupportedReason = unsupportedReason
    }
}

public struct AgentHistoryImportResult: Codable, Equatable, Sendable {
    public var resume: Bool
    public var resumeMode: String?
    public var totals: AgentHistoryTotals
    public var sources: [JSONValue]

    public init(
        resume: Bool,
        resumeMode: String? = nil,
        totals: AgentHistoryTotals = AgentHistoryTotals(),
        sources: [JSONValue] = []
    ) {
        self.resume = resume
        self.resumeMode = resumeMode
        self.totals = totals
        self.sources = sources
    }

    enum CodingKeys: String, CodingKey {
        case resume
        case resumeMode
        case totals
        case sources
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        resume = try container.decode(Bool.self, forKey: .resume)
        resumeMode = try container.decodeIfPresent(String.self, forKey: .resumeMode)
        totals = try container.decodeIfPresent(AgentHistoryTotals.self, forKey: .totals) ?? AgentHistoryTotals()
        sources = try container.decodeIfPresent([JSONValue].self, forKey: .sources) ?? []
    }
}

public struct AgentHistorySearchResult: Codable, Equatable, Sendable {
    public var query: String?
    public var filters: JSONValue?
    public var freshness: AgentHistoryFreshness?
    public var generatedAt: String?
    public var retrieval: JSONValue?
    public var results: [AgentHistorySearchHit]
    public var resultWindow: AgentHistorySearchResultWindow?
    public var pagination: AgentHistoryPagination?
    public var truncation: AgentHistoryTruncation?

    public init(
        query: String? = nil,
        filters: JSONValue? = nil,
        freshness: AgentHistoryFreshness? = nil,
        generatedAt: String? = nil,
        retrieval: JSONValue? = nil,
        results: [AgentHistorySearchHit] = [],
        resultWindow: AgentHistorySearchResultWindow? = nil,
        pagination: AgentHistoryPagination? = nil,
        truncation: AgentHistoryTruncation? = nil
    ) {
        self.query = query
        self.filters = filters
        self.freshness = freshness
        self.generatedAt = generatedAt
        self.retrieval = retrieval
        self.results = results
        self.resultWindow = resultWindow
        self.pagination = pagination
        self.truncation = truncation
    }

    enum CodingKeys: String, CodingKey {
        case query
        case filters
        case freshness
        case generatedAt
        case retrieval
        case results
        case resultWindow
        case pagination
        case truncation
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        query = try container.decodeIfPresent(String.self, forKey: .query)
        filters = try container.decodeIfPresent(JSONValue.self, forKey: .filters)
        freshness = try container.decodeIfPresent(AgentHistoryFreshness.self, forKey: .freshness)
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt)
        retrieval = try container.decodeIfPresent(JSONValue.self, forKey: .retrieval)
        results = try container.decodeIfPresent([AgentHistorySearchHit].self, forKey: .results) ?? []
        resultWindow = try container.decodeIfPresent(AgentHistorySearchResultWindow.self, forKey: .resultWindow)
        pagination = try container.decodeIfPresent(AgentHistoryPagination.self, forKey: .pagination)
        truncation = try container.decodeIfPresent(AgentHistoryTruncation.self, forKey: .truncation)
    }
}

public struct AgentHistorySearchHit: Codable, Equatable, Sendable {
    public var ctxEventId: String?
    public var ctxSessionId: String?
    public var providerSessionId: String?
    public var eventSeq: Int?
    public var title: String?
    public var snippet: String?
    public var rank: Double?
    public var retrievalScore: Double?
    public var resultType: String?
    public var resultScope: String
    public var provider: String?
    public var sourceFormat: String?
    public var timestamp: String?
    public var cwd: String?
    public var whyMatched: [String]
    public var citations: [AgentHistoryCitation]
    public var suggestedNextCommands: [String]
    public var visibility: String?

    public init(
        ctxEventId: String? = nil,
        ctxSessionId: String? = nil,
        providerSessionId: String? = nil,
        eventSeq: Int? = nil,
        title: String? = nil,
        snippet: String? = nil,
        rank: Double? = nil,
        retrievalScore: Double? = nil,
        resultType: String? = nil,
        resultScope: String,
        provider: String? = nil,
        sourceFormat: String? = nil,
        timestamp: String? = nil,
        cwd: String? = nil,
        whyMatched: [String] = [],
        citations: [AgentHistoryCitation] = [],
        suggestedNextCommands: [String] = [],
        visibility: String? = nil
    ) {
        self.ctxEventId = ctxEventId
        self.ctxSessionId = ctxSessionId
        self.providerSessionId = providerSessionId
        self.eventSeq = eventSeq
        self.title = title
        self.snippet = snippet
        self.rank = rank
        self.retrievalScore = retrievalScore
        self.resultType = resultType
        self.resultScope = resultScope
        self.provider = provider
        self.sourceFormat = sourceFormat
        self.timestamp = timestamp
        self.cwd = cwd
        self.whyMatched = whyMatched
        self.citations = citations
        self.suggestedNextCommands = suggestedNextCommands
        self.visibility = visibility
    }

    enum CodingKeys: String, CodingKey {
        case ctxEventId
        case ctxSessionId
        case providerSessionId
        case eventSeq
        case title
        case snippet
        case rank
        case retrievalScore
        case resultType
        case resultScope
        case provider
        case sourceFormat
        case timestamp
        case cwd
        case whyMatched
        case citations
        case suggestedNextCommands
        case visibility
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        ctxEventId = try container.decodeIfPresent(String.self, forKey: .ctxEventId)
        ctxSessionId = try container.decodeIfPresent(String.self, forKey: .ctxSessionId)
        providerSessionId = try container.decodeIfPresent(String.self, forKey: .providerSessionId)
        eventSeq = try container.decodeIfPresent(Int.self, forKey: .eventSeq)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        snippet = try container.decodeIfPresent(String.self, forKey: .snippet)
        rank = try container.decodeIfPresent(Double.self, forKey: .rank)
        retrievalScore = try container.decodeIfPresent(Double.self, forKey: .retrievalScore)
        resultType = try container.decodeIfPresent(String.self, forKey: .resultType)
        resultScope = try container.decodeIfPresent(String.self, forKey: .resultScope) ?? "unknown"
        provider = try container.decodeIfPresent(String.self, forKey: .provider)
        sourceFormat = try container.decodeIfPresent(String.self, forKey: .sourceFormat)
        timestamp = try container.decodeIfPresent(String.self, forKey: .timestamp)
        cwd = try container.decodeIfPresent(String.self, forKey: .cwd)
        whyMatched = try container.decodeIfPresent([String].self, forKey: .whyMatched) ?? []
        citations = try container.decodeIfPresent([AgentHistoryCitation].self, forKey: .citations) ?? []
        suggestedNextCommands = try container.decodeIfPresent([String].self, forKey: .suggestedNextCommands) ?? []
        visibility = try container.decodeIfPresent(String.self, forKey: .visibility)
    }
}

public struct AgentHistoryEventResult: Codable, Equatable, Sendable {
    public var event: AgentHistoryEventRecord?
    public var events: [AgentHistoryEventRecord]

    public init(event: AgentHistoryEventRecord? = nil, events: [AgentHistoryEventRecord] = []) {
        self.event = event
        self.events = events
    }

    enum CodingKeys: String, CodingKey {
        case event
        case events
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        event = try container.decodeIfPresent(AgentHistoryEventRecord.self, forKey: .event)
        events = try container.decodeIfPresent([AgentHistoryEventRecord].self, forKey: .events) ?? []
    }
}

public struct AgentHistorySessionResult: Codable, Equatable, Sendable {
    public var session: AgentHistorySessionSummary?
    public var events: [AgentHistoryEventRecord]
    public var mode: String?
    public var format: String?

    public init(
        session: AgentHistorySessionSummary? = nil,
        events: [AgentHistoryEventRecord] = [],
        mode: String? = nil,
        format: String? = nil
    ) {
        self.session = session
        self.events = events
        self.mode = mode
        self.format = format
    }

    enum CodingKeys: String, CodingKey {
        case session
        case events
        case mode
        case format
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        session = try container.decodeIfPresent(AgentHistorySessionSummary.self, forKey: .session)
        events = try container.decodeIfPresent([AgentHistoryEventRecord].self, forKey: .events) ?? []
        mode = try container.decodeIfPresent(String.self, forKey: .mode)
        format = try container.decodeIfPresent(String.self, forKey: .format)
    }
}

public struct AgentHistoryEventRecord: Codable, Equatable, Sendable {
    public var ctxEventId: String?
    public var ctxSessionId: String?
    public var provider: String?
    public var providerSessionId: String?
    public var sourceFormat: String?
    public var sequence: Int?
    public var eventType: String?
    public var role: String?
    public var occurredAt: String?
    public var text: String?
    public var structuredContent: JSONValue?
    public var content: CoreContentMetadata?
    public var citations: [AgentHistoryCitation]?

    public init(
        ctxEventId: String? = nil,
        ctxSessionId: String? = nil,
        provider: String? = nil,
        providerSessionId: String? = nil,
        sourceFormat: String? = nil,
        sequence: Int? = nil,
        eventType: String? = nil,
        role: String? = nil,
        occurredAt: String? = nil,
        text: String? = nil,
        structuredContent: JSONValue? = nil,
        content: CoreContentMetadata? = nil,
        citations: [AgentHistoryCitation]? = nil
    ) {
        self.ctxEventId = ctxEventId
        self.ctxSessionId = ctxSessionId
        self.provider = provider
        self.providerSessionId = providerSessionId
        self.sourceFormat = sourceFormat
        self.sequence = sequence
        self.eventType = eventType
        self.role = role
        self.occurredAt = occurredAt
        self.text = text
        self.structuredContent = structuredContent
        self.content = content
        self.citations = citations
    }
}

public enum CoreContentPolicyStatus: String, Codable, Equatable, Sendable {
    case selected
    case redacted
    case omitted
}

public struct CoreContentMetadata: Codable, Equatable, Sendable {
    public var complete: Bool
    public var policyStatus: CoreContentPolicyStatus
    public var policyReason: String?

    public init(
        complete: Bool,
        policyStatus: CoreContentPolicyStatus,
        policyReason: String? = nil
    ) {
        self.complete = complete
        self.policyStatus = policyStatus
        self.policyReason = policyReason
    }
}

public struct AgentHistorySessionSummary: Codable, Equatable, Sendable {
    public var ctxSessionId: String?
    public var provider: String?
    public var providerSessionId: String?
    public var sourceFormat: String?
    public var title: String?

    public init(
        ctxSessionId: String? = nil,
        provider: String? = nil,
        providerSessionId: String? = nil,
        sourceFormat: String? = nil,
        title: String? = nil
    ) {
        self.ctxSessionId = ctxSessionId
        self.provider = provider
        self.providerSessionId = providerSessionId
        self.sourceFormat = sourceFormat
        self.title = title
    }
}

public struct AgentHistoryFreshness: Codable, Equatable, Sendable {
    public var mode: String?
    public var status: String?
    public var reason: String?
    public var budgetReasons: [String]?
    public var sourceCount: Int?
    public var daemonLastRunAtMs: Int?
    public var totals: AgentHistoryTotals?
    public var error: String?

    public init(
        mode: String? = nil,
        status: String? = nil,
        reason: String? = nil,
        budgetReasons: [String]? = nil,
        sourceCount: Int? = nil,
        daemonLastRunAtMs: Int? = nil,
        totals: AgentHistoryTotals? = nil,
        error: String? = nil
    ) {
        self.mode = mode
        self.status = status
        self.reason = reason
        self.budgetReasons = budgetReasons
        self.sourceCount = sourceCount
        self.daemonLastRunAtMs = daemonLastRunAtMs
        self.totals = totals
        self.error = error
    }
}

public struct AgentHistoryCitation: Codable, Equatable, Sendable {
    public var itemId: String?
    public var targetType: String?
    public var ctxEventId: String?
    public var ctxSessionId: String?
    public var label: String?
    public var time: String?
    public var provider: String?
    public var sessionId: String?
    public var eventSeq: Int?

    public init(
        itemId: String? = nil,
        targetType: String? = nil,
        ctxEventId: String? = nil,
        ctxSessionId: String? = nil,
        label: String? = nil,
        time: String? = nil,
        provider: String? = nil,
        sessionId: String? = nil,
        eventSeq: Int? = nil
    ) {
        self.itemId = itemId
        self.targetType = targetType
        self.ctxEventId = ctxEventId
        self.ctxSessionId = ctxSessionId
        self.label = label
        self.time = time
        self.provider = provider
        self.sessionId = sessionId
        self.eventSeq = eventSeq
    }
}

public struct AgentHistoryTotals: Codable, Equatable, Sendable {
    public var sourceFiles: Int?
    public var sourceBytes: Int?
    public var importedSources: Int?
    public var failedSources: Int?
    public var importedSessions: Int?
    public var importedEvents: Int?
    public var importedEdges: Int?
    public var skipped: Int?
    public var failed: Int?

    public init(
        sourceFiles: Int? = nil,
        sourceBytes: Int? = nil,
        importedSources: Int? = nil,
        failedSources: Int? = nil,
        importedSessions: Int? = nil,
        importedEvents: Int? = nil,
        importedEdges: Int? = nil,
        skipped: Int? = nil,
        failed: Int? = nil
    ) {
        self.sourceFiles = sourceFiles
        self.sourceBytes = sourceBytes
        self.importedSources = importedSources
        self.failedSources = failedSources
        self.importedSessions = importedSessions
        self.importedEvents = importedEvents
        self.importedEdges = importedEdges
        self.skipped = skipped
        self.failed = failed
    }
}

public struct AgentHistorySearchResultWindow: Codable, Equatable, Sendable {
    public var limit: Int
    public var returned: Int
    public var moreAvailable: Bool

    public init(limit: Int, returned: Int, moreAvailable: Bool) {
        self.limit = limit
        self.returned = returned
        self.moreAvailable = moreAvailable
    }
}

public struct AgentHistoryPagination: Codable, Equatable, Sendable {
    public var limit: Int?
    public var hasMore: Bool?

    public init(limit: Int? = nil, hasMore: Bool? = nil) {
        self.limit = limit
        self.hasMore = hasMore
    }
}

public struct AgentHistoryTruncation: Codable, Equatable, Sendable {
    public var truncated: Bool?

    public init(truncated: Bool? = nil) {
        self.truncated = truncated
    }
}
