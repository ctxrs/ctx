import Foundation

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

public enum AgentHistoryErrorCode: String, Sendable {
    case invalidRequest = "invalid_request"
    case notFound = "not_found"
    case notInitialized = "not_initialized"
    case backendUnavailable = "backend_unavailable"
    case timeout
    case cancelled
    case notSupported = "not_supported"
    case adapterError = "adapter_error"
    case decodeError = "decode_error"
    case unknown
}

extension AgentHistoryErrorCode: Codable {
    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        self = AgentHistoryErrorCode(rawValue: try container.decode(String.self)) ?? .unknown
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

public struct AgentHistoryContractError: Codable, Equatable, Sendable {
    public var code: AgentHistoryErrorCode
    public var message: String
    public var retryable: Bool
    public var details: JSONValue?
    public var cause: String?

    public init(
        code: AgentHistoryErrorCode,
        message: String,
        retryable: Bool = false,
        details: JSONValue? = nil,
        cause: String? = nil
    ) {
        self.code = code
        self.message = message
        self.retryable = retryable
        self.details = details
        self.cause = cause
    }
}

public struct VersionInfo: Codable, Equatable, Sendable {
    public var schemaVersion: Int
    public var apiVersion: String
    public var sdkVersion: String
    public var adapter: String
    public var ctxVersion: String?
    public var hosted: Bool?

    public init(
        schemaVersion: Int = AGENT_HISTORY_V1_SCHEMA_VERSION,
        apiVersion: String = AGENT_HISTORY_V1_VERSION,
        sdkVersion: String = CTX_AGENT_HISTORY_SWIFT_SDK_VERSION,
        adapter: String,
        ctxVersion: String? = nil,
        hosted: Bool? = nil
    ) {
        self.schemaVersion = schemaVersion
        self.apiVersion = apiVersion
        self.sdkVersion = sdkVersion
        self.adapter = adapter
        self.ctxVersion = ctxVersion
        self.hosted = hosted
    }

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case apiVersion = "api_version"
        case sdkVersion = "sdk_version"
        case adapter
        case ctxVersion = "ctx_version"
        case hosted
    }
}
