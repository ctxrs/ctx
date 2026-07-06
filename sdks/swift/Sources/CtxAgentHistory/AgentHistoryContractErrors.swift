import Foundation

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
