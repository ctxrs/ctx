import Foundation

public struct AgentHistoryStatus: Codable, Equatable, Sendable {
    public var initialized: Bool
    public var localOnly: Bool
    public var dataRoot: String?
    public var indexedItems: Int?
    public var indexedSources: Int?
    public var catalogedSessions: Int?
    public var indexedCatalogSessions: Int?
    public var pendingCatalogSessions: Int?
    public var failedCatalogSessions: Int?
    public var staleCatalogSessions: Int?
    public var freshness: AgentHistoryFreshness?

    public init(
        initialized: Bool,
        localOnly: Bool,
        dataRoot: String? = nil,
        indexedItems: Int? = nil,
        indexedSources: Int? = nil,
        catalogedSessions: Int? = nil,
        indexedCatalogSessions: Int? = nil,
        pendingCatalogSessions: Int? = nil,
        failedCatalogSessions: Int? = nil,
        staleCatalogSessions: Int? = nil,
        freshness: AgentHistoryFreshness? = nil
    ) {
        self.initialized = initialized
        self.localOnly = localOnly
        self.dataRoot = dataRoot
        self.indexedItems = indexedItems
        self.indexedSources = indexedSources
        self.catalogedSessions = catalogedSessions
        self.indexedCatalogSessions = indexedCatalogSessions
        self.pendingCatalogSessions = pendingCatalogSessions
        self.failedCatalogSessions = failedCatalogSessions
        self.staleCatalogSessions = staleCatalogSessions
        self.freshness = freshness
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
    public var rawRetention: String?
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
        rawRetention: String? = nil,
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
        self.rawRetention = rawRetention
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
    public var results: [AgentHistorySearchHit]
    public var pagination: AgentHistoryPagination?
    public var truncation: AgentHistoryTruncation?

    public init(
        query: String? = nil,
        filters: JSONValue? = nil,
        freshness: AgentHistoryFreshness? = nil,
        generatedAt: String? = nil,
        results: [AgentHistorySearchHit] = [],
        pagination: AgentHistoryPagination? = nil,
        truncation: AgentHistoryTruncation? = nil
    ) {
        self.query = query
        self.filters = filters
        self.freshness = freshness
        self.generatedAt = generatedAt
        self.results = results
        self.pagination = pagination
        self.truncation = truncation
    }

    enum CodingKeys: String, CodingKey {
        case query
        case filters
        case freshness
        case generatedAt
        case results
        case pagination
        case truncation
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        query = try container.decodeIfPresent(String.self, forKey: .query)
        filters = try container.decodeIfPresent(JSONValue.self, forKey: .filters)
        freshness = try container.decodeIfPresent(AgentHistoryFreshness.self, forKey: .freshness)
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt)
        results = try container.decodeIfPresent([AgentHistorySearchHit].self, forKey: .results) ?? []
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
    public var resultScope: String
    public var provider: String?
    public var timestamp: String?
    public var cwd: String?
    public var sourcePath: String?
    public var sourceExists: Bool?
    public var cursor: String?
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
        resultScope: String,
        provider: String? = nil,
        timestamp: String? = nil,
        cwd: String? = nil,
        sourcePath: String? = nil,
        sourceExists: Bool? = nil,
        cursor: String? = nil,
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
        self.resultScope = resultScope
        self.provider = provider
        self.timestamp = timestamp
        self.cwd = cwd
        self.sourcePath = sourcePath
        self.sourceExists = sourceExists
        self.cursor = cursor
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
        case resultScope
        case provider
        case timestamp
        case cwd
        case sourcePath
        case sourceExists
        case cursor
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
        resultScope = try container.decodeIfPresent(String.self, forKey: .resultScope) ?? "unknown"
        provider = try container.decodeIfPresent(String.self, forKey: .provider)
        timestamp = try container.decodeIfPresent(String.self, forKey: .timestamp)
        cwd = try container.decodeIfPresent(String.self, forKey: .cwd)
        sourcePath = try container.decodeIfPresent(String.self, forKey: .sourcePath)
        sourceExists = try container.decodeIfPresent(Bool.self, forKey: .sourceExists)
        cursor = try container.decodeIfPresent(String.self, forKey: .cursor)
        whyMatched = try container.decodeIfPresent([String].self, forKey: .whyMatched) ?? []
        citations = try container.decodeIfPresent([AgentHistoryCitation].self, forKey: .citations) ?? []
        suggestedNextCommands = try container.decodeIfPresent([String].self, forKey: .suggestedNextCommands) ?? []
        visibility = try container.decodeIfPresent(String.self, forKey: .visibility)
    }
}

public struct AgentHistoryEventResult: Codable, Equatable, Sendable {
    public var event: AgentHistoryEventRecord?
    public var events: [AgentHistoryEventRecord]
    public var source: AgentHistorySourceLocation?

    public init(event: AgentHistoryEventRecord? = nil, events: [AgentHistoryEventRecord] = [], source: AgentHistorySourceLocation? = nil) {
        self.event = event
        self.events = events
        self.source = source
    }

    enum CodingKeys: String, CodingKey {
        case event
        case events
        case source
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        event = try container.decodeIfPresent(AgentHistoryEventRecord.self, forKey: .event)
        events = try container.decodeIfPresent([AgentHistoryEventRecord].self, forKey: .events) ?? []
        source = try container.decodeIfPresent(AgentHistorySourceLocation.self, forKey: .source)
    }
}

public struct AgentHistorySessionResult: Codable, Equatable, Sendable {
    public var session: AgentHistorySessionSummary?
    public var events: [AgentHistoryEventRecord]
    public var source: AgentHistorySourceLocation?
    public var mode: String?
    public var format: String?

    public init(
        session: AgentHistorySessionSummary? = nil,
        events: [AgentHistoryEventRecord] = [],
        source: AgentHistorySourceLocation? = nil,
        mode: String? = nil,
        format: String? = nil
    ) {
        self.session = session
        self.events = events
        self.source = source
        self.mode = mode
        self.format = format
    }

    enum CodingKeys: String, CodingKey {
        case session
        case events
        case source
        case mode
        case format
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        session = try container.decodeIfPresent(AgentHistorySessionSummary.self, forKey: .session)
        events = try container.decodeIfPresent([AgentHistoryEventRecord].self, forKey: .events) ?? []
        source = try container.decodeIfPresent(AgentHistorySourceLocation.self, forKey: .source)
        mode = try container.decodeIfPresent(String.self, forKey: .mode)
        format = try container.decodeIfPresent(String.self, forKey: .format)
    }
}

public struct AgentHistoryLocationResult: Codable, Equatable, Sendable {
    public var ctxSessionId: String
    public var ctxEventId: String?
    public var provider: String
    public var providerSessionId: String?
    public var source: AgentHistorySourceLocation
    public var resume: AgentHistoryResumeLocation?

    public init(
        ctxSessionId: String,
        ctxEventId: String? = nil,
        provider: String,
        providerSessionId: String? = nil,
        source: AgentHistorySourceLocation,
        resume: AgentHistoryResumeLocation? = nil
    ) {
        self.ctxSessionId = ctxSessionId
        self.ctxEventId = ctxEventId
        self.provider = provider
        self.providerSessionId = providerSessionId
        self.source = source
        self.resume = resume
    }
}

public struct AgentHistoryEventRecord: Codable, Equatable, Sendable {
    public var ctxEventId: String?
    public var ctxSessionId: String?
    public var sequence: Int?
    public var eventType: String?
    public var role: String?
    public var occurredAt: String?
    public var source: String?
    public var cursor: String?
    public var text: String?
    public var preview: String?
    public var redactionState: String?
    public var citations: [AgentHistoryCitation]?

    public init(
        ctxEventId: String? = nil,
        ctxSessionId: String? = nil,
        sequence: Int? = nil,
        eventType: String? = nil,
        role: String? = nil,
        occurredAt: String? = nil,
        source: String? = nil,
        cursor: String? = nil,
        text: String? = nil,
        preview: String? = nil,
        redactionState: String? = nil,
        citations: [AgentHistoryCitation]? = nil
    ) {
        self.ctxEventId = ctxEventId
        self.ctxSessionId = ctxSessionId
        self.sequence = sequence
        self.eventType = eventType
        self.role = role
        self.occurredAt = occurredAt
        self.source = source
        self.cursor = cursor
        self.text = text
        self.preview = preview
        self.redactionState = redactionState
        self.citations = citations
    }
}

public struct AgentHistorySessionSummary: Codable, Equatable, Sendable {
    public var ctxSessionId: String?
    public var provider: String?
    public var providerSessionId: String?
    public var title: String?

    public init(ctxSessionId: String? = nil, provider: String? = nil, providerSessionId: String? = nil, title: String? = nil) {
        self.ctxSessionId = ctxSessionId
        self.provider = provider
        self.providerSessionId = providerSessionId
        self.title = title
    }
}

public struct AgentHistorySourceLocation: Codable, Equatable, Sendable {
    public var path: String?
    public var cursor: String?
    public var exists: Bool?
    public var sourceId: String?
    public var sourceFormat: String?

    public init(path: String? = nil, cursor: String? = nil, exists: Bool? = nil, sourceId: String? = nil, sourceFormat: String? = nil) {
        self.path = path
        self.cursor = cursor
        self.exists = exists
        self.sourceId = sourceId
        self.sourceFormat = sourceFormat
    }
}

public struct AgentHistoryResumeLocation: Codable, Equatable, Sendable {
    public var cursor: String?

    public init(cursor: String? = nil) {
        self.cursor = cursor
    }
}

public struct AgentHistoryFreshness: Codable, Equatable, Sendable {
    public var mode: String?
    public var status: String?
    public var sourceCount: Int?
    public var totals: AgentHistoryTotals?
    public var error: String?

    public init(mode: String? = nil, status: String? = nil, sourceCount: Int? = nil, totals: AgentHistoryTotals? = nil, error: String? = nil) {
        self.mode = mode
        self.status = status
        self.sourceCount = sourceCount
        self.totals = totals
        self.error = error
    }
}

public struct AgentHistoryCitation: Codable, Equatable, Sendable {
    public var itemId: String?
    public var itemType: String?
    public var ctxEventId: String?
    public var ctxSessionId: String?
    public var label: String?
    public var time: String?
    public var provider: String?
    public var sessionId: String?
    public var eventSeq: Int?
    public var sourcePath: String?
    public var sourceExists: Bool?
    public var cursor: String?

    public init(
        itemId: String? = nil,
        itemType: String? = nil,
        ctxEventId: String? = nil,
        ctxSessionId: String? = nil,
        label: String? = nil,
        time: String? = nil,
        provider: String? = nil,
        sessionId: String? = nil,
        eventSeq: Int? = nil,
        sourcePath: String? = nil,
        sourceExists: Bool? = nil,
        cursor: String? = nil
    ) {
        self.itemId = itemId
        self.itemType = itemType
        self.ctxEventId = ctxEventId
        self.ctxSessionId = ctxSessionId
        self.label = label
        self.time = time
        self.provider = provider
        self.sessionId = sessionId
        self.eventSeq = eventSeq
        self.sourcePath = sourcePath
        self.sourceExists = sourceExists
        self.cursor = cursor
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

public struct AgentHistoryPagination: Codable, Equatable, Sendable {
    public var limit: Int?

    public init(limit: Int? = nil) {
        self.limit = limit
    }
}

public struct AgentHistoryTruncation: Codable, Equatable, Sendable {
    public var truncated: Bool?

    public init(truncated: Bool? = nil) {
        self.truncated = truncated
    }
}
