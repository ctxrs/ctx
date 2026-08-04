import Foundation

public enum AgentHistoryMCPResponseStatus: String, Codable, Equatable, Sendable {
    case succeeded
    case failed
    case cancelled
    case timedOut = "timed_out"
    case unknown
}

public enum AgentHistoryMCPFailureKind: String, Codable, Equatable, Sendable {
    case toolReported = "tool_reported"
    case invocation
    case unknown
}

public enum AgentHistoryMCPPayloadOmissionReason: String, Codable, Equatable, Sendable {
    case sizeLimit = "size_limit"
}

public enum AgentHistoryMCPJSONCaptureStatus: String, Codable, Equatable, Sendable {
    case present
    case absent
    case unavailable
    case omitted
}

public enum AgentHistoryMCPTextCaptureStatus: String, Codable, Equatable, Sendable {
    case normalizedBody = "normalized_body"
    case absent
    case unavailable
    case omitted
}

public enum AgentHistoryMCPJSONCapture: Codable, Equatable, Sendable {
    case present(value: JSONValue)
    case absent
    case unavailable
    case omitted(reason: AgentHistoryMCPPayloadOmissionReason, observedEncodedBytes: Int?)

    public var captureStatus: AgentHistoryMCPJSONCaptureStatus {
        switch self {
        case .present:
            return .present
        case .absent:
            return .absent
        case .unavailable:
            return .unavailable
        case .omitted:
            return .omitted
        }
    }

    public var value: JSONValue? {
        guard case let .present(value) = self else {
            return nil
        }
        return value
    }

    public var reason: AgentHistoryMCPPayloadOmissionReason? {
        guard case let .omitted(reason, _) = self else {
            return nil
        }
        return reason
    }

    public var observedEncodedBytes: Int? {
        guard case let .omitted(_, observedEncodedBytes) = self else {
            return nil
        }
        return observedEncodedBytes
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: MCPAnyCodingKey.self)
        let actual = Set(container.allKeys.map(\.stringValue))
        let statusKey = MCPAnyCodingKey("captureStatus")
        guard actual.contains(statusKey.stringValue) else {
            throw mcpDecodingError(decoder.codingPath, "MCP JSON capture requires captureStatus")
        }
        let status = try container.decode(AgentHistoryMCPJSONCaptureStatus.self, forKey: statusKey)
        switch status {
        case .present:
            try requireExactMCPMembers(actual, ["captureStatus", "value"], decoder.codingPath, "MCP JSON capture")
            self = .present(value: try container.decode(JSONValue.self, forKey: MCPAnyCodingKey("value")))
        case .absent:
            try requireExactMCPMembers(actual, ["captureStatus"], decoder.codingPath, "MCP JSON capture")
            self = .absent
        case .unavailable:
            try requireExactMCPMembers(actual, ["captureStatus"], decoder.codingPath, "MCP JSON capture")
            self = .unavailable
        case .omitted:
            let expected = actual.contains("observedEncodedBytes")
                ? Set(["captureStatus", "reason", "observedEncodedBytes"])
                : Set(["captureStatus", "reason"])
            try requireExactMCPMembers(actual, expected, decoder.codingPath, "MCP JSON capture")
            let reason = try container.decode(
                AgentHistoryMCPPayloadOmissionReason.self,
                forKey: MCPAnyCodingKey("reason")
            )
            let observedEncodedBytes = actual.contains("observedEncodedBytes")
                ? try decodeMCPSafeInteger(
                    from: container,
                    forKey: MCPAnyCodingKey("observedEncodedBytes"),
                    context: "MCP JSON capture observedEncodedBytes"
                )
                : nil
            self = .omitted(reason: reason, observedEncodedBytes: observedEncodedBytes)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: MCPAnyCodingKey.self)
        try container.encode(captureStatus, forKey: MCPAnyCodingKey("captureStatus"))
        switch self {
        case let .present(value):
            try container.encode(value, forKey: MCPAnyCodingKey("value"))
        case .absent, .unavailable:
            break
        case let .omitted(reason, observedEncodedBytes):
            try container.encode(reason, forKey: MCPAnyCodingKey("reason"))
            if let observedEncodedBytes {
                try validateMCPSafeIntegerForEncoding(
                    observedEncodedBytes,
                    codingPath: encoder.codingPath,
                    context: "MCP JSON capture observedEncodedBytes"
                )
                try container.encode(observedEncodedBytes, forKey: MCPAnyCodingKey("observedEncodedBytes"))
            }
        }
    }
}

public enum AgentHistoryMCPTextCapture: Codable, Equatable, Sendable {
    case normalizedBody
    case absent
    case unavailable
    case omitted(reason: AgentHistoryMCPPayloadOmissionReason, observedEncodedBytes: Int?)

    public var captureStatus: AgentHistoryMCPTextCaptureStatus {
        switch self {
        case .normalizedBody:
            return .normalizedBody
        case .absent:
            return .absent
        case .unavailable:
            return .unavailable
        case .omitted:
            return .omitted
        }
    }

    public var reason: AgentHistoryMCPPayloadOmissionReason? {
        guard case let .omitted(reason, _) = self else {
            return nil
        }
        return reason
    }

    public var observedEncodedBytes: Int? {
        guard case let .omitted(_, observedEncodedBytes) = self else {
            return nil
        }
        return observedEncodedBytes
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: MCPAnyCodingKey.self)
        let actual = Set(container.allKeys.map(\.stringValue))
        let statusKey = MCPAnyCodingKey("captureStatus")
        guard actual.contains(statusKey.stringValue) else {
            throw mcpDecodingError(decoder.codingPath, "MCP text capture requires captureStatus")
        }
        let status = try container.decode(AgentHistoryMCPTextCaptureStatus.self, forKey: statusKey)
        switch status {
        case .normalizedBody:
            try requireExactMCPMembers(actual, ["captureStatus"], decoder.codingPath, "MCP text capture")
            self = .normalizedBody
        case .absent:
            try requireExactMCPMembers(actual, ["captureStatus"], decoder.codingPath, "MCP text capture")
            self = .absent
        case .unavailable:
            try requireExactMCPMembers(actual, ["captureStatus"], decoder.codingPath, "MCP text capture")
            self = .unavailable
        case .omitted:
            let expected = actual.contains("observedEncodedBytes")
                ? Set(["captureStatus", "reason", "observedEncodedBytes"])
                : Set(["captureStatus", "reason"])
            try requireExactMCPMembers(actual, expected, decoder.codingPath, "MCP text capture")
            let reason = try container.decode(
                AgentHistoryMCPPayloadOmissionReason.self,
                forKey: MCPAnyCodingKey("reason")
            )
            let observedEncodedBytes = actual.contains("observedEncodedBytes")
                ? try decodeMCPSafeInteger(
                    from: container,
                    forKey: MCPAnyCodingKey("observedEncodedBytes"),
                    context: "MCP text capture observedEncodedBytes"
                )
                : nil
            self = .omitted(reason: reason, observedEncodedBytes: observedEncodedBytes)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: MCPAnyCodingKey.self)
        try container.encode(captureStatus, forKey: MCPAnyCodingKey("captureStatus"))
        if case let .omitted(reason, observedEncodedBytes) = self {
            try container.encode(reason, forKey: MCPAnyCodingKey("reason"))
            if let observedEncodedBytes {
                try validateMCPSafeIntegerForEncoding(
                    observedEncodedBytes,
                    codingPath: encoder.codingPath,
                    context: "MCP text capture observedEncodedBytes"
                )
                try container.encode(observedEncodedBytes, forKey: MCPAnyCodingKey("observedEncodedBytes"))
            }
        }
    }
}

public struct AgentHistoryMCPInvocation: Codable, Equatable, Sendable {
    public var server: String
    public var tool: String
    public var arguments: AgentHistoryMCPJSONCapture

    public init(server: String, tool: String, arguments: AgentHistoryMCPJSONCapture) {
        self.server = server
        self.tool = tool
        self.arguments = arguments
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: MCPAnyCodingKey.self)
        try requireExactMCPMembers(
            Set(container.allKeys.map(\.stringValue)),
            ["server", "tool", "arguments"],
            decoder.codingPath,
            "MCP invocation"
        )
        server = try decodeMCPIdentity(from: container, forKey: MCPAnyCodingKey("server"), context: "MCP invocation server")
        tool = try decodeMCPIdentity(from: container, forKey: MCPAnyCodingKey("tool"), context: "MCP invocation tool")
        arguments = try container.decode(AgentHistoryMCPJSONCapture.self, forKey: MCPAnyCodingKey("arguments"))
        try validateArgumentsCapture(arguments, codingPath: decoder.codingPath)
    }

    public func encode(to encoder: Encoder) throws {
        try validateMCPIdentityForEncoding(server, codingPath: encoder.codingPath, context: "MCP invocation server")
        try validateMCPIdentityForEncoding(tool, codingPath: encoder.codingPath, context: "MCP invocation tool")
        try validateArgumentsCapture(arguments, codingPath: encoder.codingPath)
        var container = encoder.container(keyedBy: MCPAnyCodingKey.self)
        try container.encode(server, forKey: MCPAnyCodingKey("server"))
        try container.encode(tool, forKey: MCPAnyCodingKey("tool"))
        try container.encode(arguments, forKey: MCPAnyCodingKey("arguments"))
    }

    private func validateArgumentsCapture(
        _ capture: AgentHistoryMCPJSONCapture,
        codingPath: [CodingKey]
    ) throws {
        guard case let .present(value) = capture else {
            return
        }
        guard case .object = value else {
            throw mcpDecodingError(codingPath, "present MCP invocation arguments must be a JSON object")
        }
    }
}

public struct AgentHistoryMCPResponse: Codable, Equatable, Sendable {
    public var status: AgentHistoryMCPResponseStatus
    public var failureKind: AgentHistoryMCPFailureKind?
    public var durationNs: Int?
    public var text: AgentHistoryMCPTextCapture
    public var payload: AgentHistoryMCPJSONCapture

    public init(
        status: AgentHistoryMCPResponseStatus,
        failureKind: AgentHistoryMCPFailureKind? = nil,
        durationNs: Int? = nil,
        text: AgentHistoryMCPTextCapture,
        payload: AgentHistoryMCPJSONCapture
    ) {
        self.status = status
        self.failureKind = failureKind
        self.durationNs = durationNs
        self.text = text
        self.payload = payload
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: MCPAnyCodingKey.self)
        let actual = Set(container.allKeys.map(\.stringValue))
        let allowed = Set(["status", "failureKind", "durationNs", "text", "payload"])
        let required = Set(["status", "text", "payload"])
        try requireAllowedMCPMembers(actual, allowed, required, decoder.codingPath, "MCP response")
        status = try container.decode(AgentHistoryMCPResponseStatus.self, forKey: MCPAnyCodingKey("status"))
        if status == .failed {
            guard actual.contains("failureKind") else {
                throw mcpDecodingError(decoder.codingPath, "failed MCP response requires failureKind")
            }
            failureKind = try container.decode(AgentHistoryMCPFailureKind.self, forKey: MCPAnyCodingKey("failureKind"))
        } else {
            guard !actual.contains("failureKind") else {
                throw mcpDecodingError(decoder.codingPath, "MCP failureKind is only valid for failed responses")
            }
            failureKind = nil
        }
        durationNs = actual.contains("durationNs")
            ? try decodeMCPSafeInteger(
                from: container,
                forKey: MCPAnyCodingKey("durationNs"),
                context: "MCP response durationNs"
            )
            : nil
        text = try container.decode(AgentHistoryMCPTextCapture.self, forKey: MCPAnyCodingKey("text"))
        payload = try container.decode(AgentHistoryMCPJSONCapture.self, forKey: MCPAnyCodingKey("payload"))
    }

    public func encode(to encoder: Encoder) throws {
        if status == .failed, failureKind == nil {
            throw mcpEncodingError(self, encoder.codingPath, "failed MCP response requires failureKind")
        }
        if status != .failed, failureKind != nil {
            throw mcpEncodingError(self, encoder.codingPath, "MCP failureKind is only valid for failed responses")
        }
        if let durationNs {
            try validateMCPSafeIntegerForEncoding(
                durationNs,
                codingPath: encoder.codingPath,
                context: "MCP response durationNs"
            )
        }
        var container = encoder.container(keyedBy: MCPAnyCodingKey.self)
        try container.encode(status, forKey: MCPAnyCodingKey("status"))
        try container.encodeIfPresent(failureKind, forKey: MCPAnyCodingKey("failureKind"))
        try container.encodeIfPresent(durationNs, forKey: MCPAnyCodingKey("durationNs"))
        try container.encode(text, forKey: MCPAnyCodingKey("text"))
        try container.encode(payload, forKey: MCPAnyCodingKey("payload"))
    }
}

public struct AgentHistoryMCPExchange: Codable, Equatable, Sendable {
    public static let maximumIdentityBytes = 64 * 1024
    public static let maximumExactInteger = 9_007_199_254_740_991

    public var providerCallId: String
    public var invocation: AgentHistoryMCPInvocation?
    public var response: AgentHistoryMCPResponse?

    public init(
        providerCallId: String,
        invocation: AgentHistoryMCPInvocation? = nil,
        response: AgentHistoryMCPResponse? = nil
    ) {
        self.providerCallId = providerCallId
        self.invocation = invocation
        self.response = response
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: MCPAnyCodingKey.self)
        let actual = Set(container.allKeys.map(\.stringValue))
        try requireAllowedMCPMembers(
            actual,
            ["providerCallId", "invocation", "response"],
            ["providerCallId"],
            decoder.codingPath,
            "MCP exchange"
        )
        guard actual.contains("invocation") || actual.contains("response") else {
            throw mcpDecodingError(decoder.codingPath, "MCP exchange requires invocation, response, or both")
        }
        providerCallId = try decodeMCPIdentity(
            from: container,
            forKey: MCPAnyCodingKey("providerCallId"),
            context: "MCP exchange providerCallId"
        )
        invocation = actual.contains("invocation")
            ? try container.decode(AgentHistoryMCPInvocation.self, forKey: MCPAnyCodingKey("invocation"))
            : nil
        response = actual.contains("response")
            ? try container.decode(AgentHistoryMCPResponse.self, forKey: MCPAnyCodingKey("response"))
            : nil
    }

    public func encode(to encoder: Encoder) throws {
        try validateMCPIdentityForEncoding(
            providerCallId,
            codingPath: encoder.codingPath,
            context: "MCP exchange providerCallId"
        )
        guard invocation != nil || response != nil else {
            throw mcpEncodingError(self, encoder.codingPath, "MCP exchange requires invocation, response, or both")
        }
        var container = encoder.container(keyedBy: MCPAnyCodingKey.self)
        try container.encode(providerCallId, forKey: MCPAnyCodingKey("providerCallId"))
        try container.encodeIfPresent(invocation, forKey: MCPAnyCodingKey("invocation"))
        try container.encodeIfPresent(response, forKey: MCPAnyCodingKey("response"))
    }
}

private struct MCPAnyCodingKey: CodingKey {
    let stringValue: String
    let intValue: Int? = nil

    init(_ stringValue: String) {
        self.stringValue = stringValue
    }

    init?(stringValue: String) {
        self.init(stringValue)
    }

    init?(intValue: Int) {
        return nil
    }
}

private func requireExactMCPMembers(
    _ actual: Set<String>,
    _ expected: Set<String>,
    _ codingPath: [CodingKey],
    _ context: String
) throws {
    guard actual == expected else {
        throw mcpDecodingError(codingPath, "\(context) has invalid members")
    }
}

private func requireAllowedMCPMembers(
    _ actual: Set<String>,
    _ allowed: Set<String>,
    _ required: Set<String>,
    _ codingPath: [CodingKey],
    _ context: String
) throws {
    guard actual.isSubset(of: allowed), required.isSubset(of: actual) else {
        throw mcpDecodingError(codingPath, "\(context) has invalid members")
    }
}

private func decodeMCPIdentity(
    from container: KeyedDecodingContainer<MCPAnyCodingKey>,
    forKey key: MCPAnyCodingKey,
    context: String
) throws -> String {
    let value = try container.decode(String.self, forKey: key)
    guard !value.isEmpty, value.utf8.count <= AgentHistoryMCPExchange.maximumIdentityBytes else {
        throw DecodingError.dataCorruptedError(
            forKey: key,
            in: container,
            debugDescription: "\(context) must be nonempty and no more than \(AgentHistoryMCPExchange.maximumIdentityBytes) decoded UTF-8 bytes"
        )
    }
    return value
}

private func validateMCPIdentityForEncoding(
    _ value: String,
    codingPath: [CodingKey],
    context: String
) throws {
    guard !value.isEmpty, value.utf8.count <= AgentHistoryMCPExchange.maximumIdentityBytes else {
        throw mcpEncodingError(
            value,
            codingPath,
            "\(context) must be nonempty and no more than \(AgentHistoryMCPExchange.maximumIdentityBytes) decoded UTF-8 bytes"
        )
    }
}

private func decodeMCPSafeInteger(
    from container: KeyedDecodingContainer<MCPAnyCodingKey>,
    forKey key: MCPAnyCodingKey,
    context: String
) throws -> Int {
    let value = try container.decode(Int.self, forKey: key)
    guard value >= 0, value <= AgentHistoryMCPExchange.maximumExactInteger else {
        throw DecodingError.dataCorruptedError(
            forKey: key,
            in: container,
            debugDescription: "\(context) is outside the exact JSON integer domain"
        )
    }
    return value
}

private func validateMCPSafeIntegerForEncoding(
    _ value: Int,
    codingPath: [CodingKey],
    context: String
) throws {
    guard value >= 0, value <= AgentHistoryMCPExchange.maximumExactInteger else {
        throw mcpEncodingError(value, codingPath, "\(context) is outside the exact JSON integer domain")
    }
}

private func mcpDecodingError(_ codingPath: [CodingKey], _ description: String) -> DecodingError {
    .dataCorrupted(.init(codingPath: codingPath, debugDescription: description))
}

private func mcpEncodingError(_ value: Any, _ codingPath: [CodingKey], _ description: String) -> EncodingError {
    .invalidValue(value, .init(codingPath: codingPath, debugDescription: description))
}
