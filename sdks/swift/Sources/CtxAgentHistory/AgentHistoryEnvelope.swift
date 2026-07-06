import Foundation

public let AGENT_HISTORY_V1_VERSION = "agent-history-v1"
public let CTX_AGENT_HISTORY_SWIFT_SDK_VERSION = "0.0.0"
public let AGENT_HISTORY_V1_SCHEMA_VERSION = 1

public enum AgentHistoryOperation: String, Codable, Sendable {
    case status
    case initialize = "init"
    case sources
    case importHistory = "import"
    case sync
    case search
    case showEvent
    case showSession
    case locateEvent
    case locateSession
    case error
}

public enum AgentHistoryBackendKind: Equatable, Sendable, Codable, CustomStringConvertible {
    case local
    case hosted
    case other(String)

    public init(rawValue: String) {
        switch rawValue {
        case "local":
            self = .local
        case "hosted":
            self = .hosted
        default:
            self = .other(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .local:
            return "local"
        case .hosted:
            return "hosted"
        case let .other(value):
            return value
        }
    }

    public var description: String {
        rawValue
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        self.init(rawValue: try container.decode(String.self))
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

public struct AgentHistoryBackend: Codable, Equatable, Sendable {
    public var kind: AgentHistoryBackendKind
    public var dataRoot: String?
    public var baseURL: String?

    public init(kind: AgentHistoryBackendKind, dataRoot: String? = nil, baseURL: String? = nil) {
        self.kind = kind
        self.dataRoot = dataRoot
        self.baseURL = baseURL
    }

    public init(kind: String, dataRoot: String? = nil, baseURL: String? = nil) {
        self.init(kind: AgentHistoryBackendKind(rawValue: kind), dataRoot: dataRoot, baseURL: baseURL)
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case dataRoot
        case baseURL = "baseUrl"
    }
}

public struct AgentHistoryEnvelope: Codable, Equatable, Sendable {
    public var contractVersion: String
    public var schemaVersion: Int
    public var operation: AgentHistoryOperation
    public var backend: AgentHistoryBackend?
    public var status: AgentHistoryStatus?
    public var sources: [ProviderSource]?
    public var importResult: AgentHistoryImportResult?
    public var search: AgentHistorySearchResult?
    public var event: AgentHistoryEventResult?
    public var session: AgentHistorySessionResult?
    public var location: AgentHistoryLocationResult?
    public var error: AgentHistoryContractError?

    public init(
        contractVersion: String = AGENT_HISTORY_V1_VERSION,
        schemaVersion: Int = AGENT_HISTORY_V1_SCHEMA_VERSION,
        operation: AgentHistoryOperation,
        backend: AgentHistoryBackend? = nil,
        status: AgentHistoryStatus? = nil,
        sources: [ProviderSource]? = nil,
        importResult: AgentHistoryImportResult? = nil,
        search: AgentHistorySearchResult? = nil,
        event: AgentHistoryEventResult? = nil,
        session: AgentHistorySessionResult? = nil,
        location: AgentHistoryLocationResult? = nil,
        error: AgentHistoryContractError? = nil
    ) {
        self.contractVersion = contractVersion
        self.schemaVersion = schemaVersion
        self.operation = operation
        self.backend = backend
        self.status = status
        self.sources = sources
        self.importResult = importResult
        self.search = search
        self.event = event
        self.session = session
        self.location = location
        self.error = error
    }

    enum CodingKeys: String, CodingKey {
        case contractVersion
        case schemaVersion
        case operation
        case backend
        case status
        case sources
        case importResult = "import"
        case search
        case event
        case session
        case location
        case error
    }
}
