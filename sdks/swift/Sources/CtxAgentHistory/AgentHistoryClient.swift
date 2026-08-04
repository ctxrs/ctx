import Foundation

public struct AgentHistoryClient: Sendable {
    private enum Backend: Sendable {
        case local(LocalCLIAdapter)
        case hosted(HostedConfig)
    }

    private var backend: Backend

    public init(adapter: LocalCLIAdapter = LocalCLIAdapter()) {
        backend = .local(adapter)
    }

    private init(backend: Backend) {
        self.backend = backend
    }

    public static func local(
        ctxPath: String = "ctx",
        dataRoot: String? = nil,
        cwd: String? = nil,
        env: [String: String] = [:],
        timeout: TimeInterval? = 60
    ) -> AgentHistoryClient {
        AgentHistoryClient(
            adapter: LocalCLIAdapter(
                ctxPath: ctxPath,
                dataRoot: dataRoot,
                cwd: cwd,
                env: env,
                timeout: timeout
            )
        )
    }

    public static func hosted(_ config: HostedConfig = HostedConfig()) -> AgentHistoryClient {
        AgentHistoryClient(backend: .hosted(config))
    }

    public func status() throws -> StatusResponse {
        try StatusResponse(envelope: localEnvelope(operation: .status, arguments: ["status", "--format=json"]))
    }

    public func initialize(_ options: InitOptions = InitOptions()) throws -> InitResponse {
        var arguments = ["setup", "--format=json"]
        appendOption(&arguments, "--progress", options.progress)
        return try InitResponse(envelope: localEnvelope(operation: .initialize, arguments: arguments))
    }

    public func sources() throws -> SourcesResponse {
        try SourcesResponse(envelope: localEnvelope(operation: .sources, arguments: ["sources", "--format=json"]))
    }

    public func importHistory(_ options: ImportOptions = ImportOptions()) throws -> ImportResponse {
        var arguments = ["import", "--format=json"]
        appendOption(&arguments, "--progress", options.progress)
        appendImportOptions(&arguments, options)
        return try ImportResponse(envelope: localEnvelope(operation: .importHistory, arguments: arguments))
    }

    public func sync(_ options: ImportOptions = ImportOptions()) throws -> ImportResponse {
        var arguments = ["import", "--format=json"]
        appendOption(&arguments, "--progress", options.progress)
        appendImportOptions(&arguments, options)
        return try ImportResponse(envelope: localEnvelope(operation: .sync, arguments: arguments))
    }

    public func search(_ query: String? = nil, options: SearchOptions = SearchOptions()) throws -> SearchResponse {
        try requireSearchIntent(query: query, options: options)
        try requireCompatibleSearchFilters(options)
        var arguments = ["search"]
        if let query {
            arguments.append(query)
        }
        for term in options.terms {
            arguments.append(contentsOf: ["--term", term])
        }
        if let limit = options.limit {
            arguments.append(contentsOf: ["--limit", String(limit)])
        }
        appendOption(&arguments, "--backend", options.backend)
        if let semanticWeight = options.semanticWeight {
            arguments.append(contentsOf: ["--semantic-weight", String(semanticWeight)])
        }
        appendOption(&arguments, "--provider", options.provider)
        appendOption(&arguments, "--workspace", options.workspace)
        appendOption(&arguments, "--since", options.since)
        if options.primaryOnly {
            arguments.append("--primary-only")
        }
        if options.includeSubagents {
            arguments.append("--include-subagents")
        }
        appendOption(&arguments, "--content-scope", options.contentScope?.rawValue)
        appendOption(&arguments, "--event-type", options.eventType)
        appendOption(&arguments, "--file", options.file)
        appendOption(&arguments, "--session", options.session)
        if options.events {
            arguments.append("--events")
        }
        appendOption(&arguments, "--refresh", options.refresh)
        if options.includeCurrentSession {
            arguments.append("--include-current-session")
        }
        arguments.append("--format=json")
        return try SearchResponse(envelope: localEnvelope(operation: .search, arguments: arguments))
    }

    public func showEvent(_ id: String, options: ShowEventOptions = ShowEventOptions()) throws -> ShowEventResponse {
        try requireID("event id", id)
        var arguments = ["show", "event", id, "--format", "json"]
        if let before = options.before {
            arguments.append(contentsOf: ["--before", String(before)])
        }
        if let after = options.after {
            arguments.append(contentsOf: ["--after", String(after)])
        }
        if let window = options.window {
            arguments.append(contentsOf: ["--window", String(window)])
        }
        return try ShowEventResponse(envelope: localEnvelope(operation: .showEvent, arguments: arguments))
    }

    public func showSession(_ id: String, options: ShowSessionOptions = ShowSessionOptions()) throws -> ShowSessionResponse {
        var merged = options
        merged.id = id
        return try showSession(merged)
    }

    public func showSession(_ options: ShowSessionOptions) throws -> ShowSessionResponse {
        var arguments = ["show", "session"]
        try appendSessionLookup(&arguments, id: options.id, provider: options.provider, providerSession: options.providerSession)
        arguments.append(contentsOf: ["--mode", options.mode ?? "lite", "--format", "json"])
        return try ShowSessionResponse(envelope: localEnvelope(operation: .showSession, arguments: arguments))
    }

    public func version() throws -> VersionInfo {
        switch backend {
        case let .local(adapter):
            let raw = try adapter.versionString()
            return VersionInfo(
                adapter: "local-cli",
                ctxVersion: parseCtxVersion(raw)
            )
        case .hosted:
            return VersionInfo(adapter: "hosted-placeholder", hosted: false)
        }
    }

    public func versioning() throws -> JSONValue {
        let data = try JSONEncoder().encode(try version())
        return try JSONDecoder().decode(JSONValue.self, from: data)
    }

    public func errorEnvelope(for error: CtxAgentHistorySDKError, operation: AgentHistoryOperation = .error) -> AgentHistoryEnvelope {
        let backendValue: AgentHistoryBackend?
        switch backend {
        case let .local(adapter):
            backendValue = adapter.backend
        case let .hosted(config):
            backendValue = AgentHistoryBackend(kind: "hosted", baseURL: config.baseURL?.absoluteString)
        }
        return AgentHistoryEnvelope(operation: operation, backend: backendValue, error: error.contractError)
    }

    private func localEnvelope(operation: AgentHistoryOperation, arguments: [String]) throws -> AgentHistoryEnvelope {
        switch backend {
        case let .local(adapter):
            let data = try adapter.execute(arguments)
            let raw = try decodeJSONObject(data)
            return try makeEnvelope(operation: operation, backend: adapter.backend, raw: raw)
        case .hosted:
            throw hostedUnsupported(operation: operation)
        }
    }

    private func decodeJSONObject(_ data: Data) throws -> JSONValue {
        do {
            var scanner = ExactJSONScanner(data)
            try scanner.validate()
            let value = try JSONDecoder().decode(JSONValue.self, from: data)
            guard case .object = value else {
                throw CtxAgentHistorySDKError(code: .decodeError, message: "ctx returned a non-object JSON value")
            }
            return value
        } catch let error as CtxAgentHistorySDKError {
            throw error
        } catch {
            throw CtxAgentHistorySDKError(
                code: .decodeError,
                message: "ctx returned invalid JSON",
                details: .object(["stdout": .string(String(data: data, encoding: .utf8) ?? "")]),
                cause: String(describing: error)
            )
        }
    }

    private func makeEnvelope(operation: AgentHistoryOperation, backend: AgentHistoryBackend, raw: JSONValue) throws -> AgentHistoryEnvelope {
        switch operation {
        case .status:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                status: try decodeTyped(try normalizeStatus(raw), as: AgentHistoryStatus.self, context: "status")
            )
        case .initialize:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                status: try decodeTyped(try normalizeStatus(raw), as: AgentHistoryStatus.self, context: "status")
            )
        case .sources:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                sources: try decodeTyped(.array(normalizeSources(raw)), as: [ProviderSource].self, context: "sources")
            )
        case .importHistory, .sync:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                importResult: try decodeTyped(normalizeImport(raw), as: AgentHistoryImportResult.self, context: "import")
            )
        case .search:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                search: try decodeTyped(normalizeSearch(raw), as: AgentHistorySearchResult.self, context: "search")
            )
        case .showEvent:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                event: try decodeTyped(try normalizeEvent(raw), as: AgentHistoryEventResult.self, context: "event")
            )
        case .showSession:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                session: try decodeTyped(try normalizeSession(raw), as: AgentHistorySessionResult.self, context: "session")
            )
        case .error:
            throw CtxAgentHistorySDKError(code: .invalidRequest, message: "error is not a local CLI operation")
        }
    }
}

private func appendImportOptions(_ arguments: inout [String], _ options: ImportOptions) {
    appendOption(&arguments, "--provider", options.provider)
    appendOption(&arguments, "--path", options.path)
    if options.all {
        arguments.append("--all")
    }
    if options.resume {
        arguments.append("--resume")
    }
}

private func appendSessionLookup(_ arguments: inout [String], id: String?, provider: String?, providerSession: String?) throws {
    if let id, !id.isEmpty {
        arguments.append(id)
    }
    appendOption(&arguments, "--provider", provider)
    appendOption(&arguments, "--provider-session", providerSession)
    if (id?.isEmpty ?? true), (providerSession?.isEmpty ?? true) {
        throw CtxAgentHistorySDKError(
            code: .invalidRequest,
            message: "session lookup requires an id or provider session"
        )
    }
}

private func appendOption(_ arguments: inout [String], _ name: String, _ value: String?) {
    if let value, !value.isEmpty {
        arguments.append(contentsOf: [name, value])
    }
}

private func requireSearchIntent(query: String?, options: SearchOptions) throws {
    if hasSearchText(query) || hasSearchText(options.file) || options.terms.contains(where: { hasSearchText($0) }) {
        return
    }
    throw CtxAgentHistorySDKError(
        code: .invalidRequest,
        message: "search requires a query, term, or file option"
    )
}

private func requireCompatibleSearchFilters(_ options: SearchOptions) throws {
    if options.contentScope != nil, let eventType = options.eventType, !eventType.isEmpty {
        throw CtxAgentHistorySDKError(
            code: .invalidRequest,
            message: "search content scope and event type are mutually exclusive"
        )
    }
}

private func hasSearchText(_ value: String?) -> Bool {
    guard let value else {
        return false
    }
    return !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
}

private func requireID(_ name: String, _ id: String) throws {
    if id.isEmpty {
        throw CtxAgentHistorySDKError(code: .invalidRequest, message: "\(name) is required")
    }
}

private func hostedUnsupported(operation: AgentHistoryOperation) -> CtxAgentHistorySDKError {
    CtxAgentHistorySDKError(
        code: .notSupported,
        message: "hosted ctx agent history backend is not available in this in-repo SDK",
        details: .object(["backend": .string("hosted"), "operation": .string(operation.rawValue)])
    )
}

private func decodeTyped<T: Decodable>(_ value: JSONValue, as type: T.Type, context: String) throws -> T {
    do {
        let data = try JSONEncoder().encode(value)
        return try JSONDecoder().decode(type, from: data)
    } catch let error as CtxAgentHistorySDKError {
        throw error
    } catch {
        throw CtxAgentHistorySDKError(
            code: .decodeError,
            message: "ctx returned a \(context) payload that does not match agent-history-v1",
            details: .object(["payload": value]),
            cause: String(describing: error)
        )
    }
}

private struct ExactJSONScanner {
    private let bytes: [UInt8]
    private var index = 0

    init(_ data: Data) {
        bytes = Array(data)
    }

    mutating func validate() throws {
        try parseValue()
        skipWhitespace()
        guard index == bytes.count else {
            throw ScanError.malformed("trailing JSON data")
        }
    }

    private mutating func parseValue() throws {
        skipWhitespace()
        guard index < bytes.count else {
            throw ScanError.malformed("unexpected end of JSON")
        }
        switch bytes[index] {
        case UInt8(ascii: "{"):
            try parseObject()
        case UInt8(ascii: "["):
            try parseArray()
        case UInt8(ascii: "\""):
            _ = try parseString()
        default:
            let start = index
            while index < bytes.count,
                  !Self.isWhitespace(bytes[index]),
                  bytes[index] != UInt8(ascii: ","),
                  bytes[index] != UInt8(ascii: "}"),
                  bytes[index] != UInt8(ascii: "]")
            {
                index += 1
            }
            guard index > start else {
                throw ScanError.malformed("expected JSON value")
            }
        }
    }

    private mutating func parseObject() throws {
        index += 1
        var members = Set<String>()
        skipWhitespace()
        if consume(UInt8(ascii: "}")) {
            return
        }
        while index < bytes.count {
            skipWhitespace()
            let member = try parseString()
            guard members.insert(member).inserted else {
                throw ScanError.duplicate(member)
            }
            skipWhitespace()
            guard consume(UInt8(ascii: ":")) else {
                throw ScanError.malformed("expected colon")
            }
            try parseValue()
            skipWhitespace()
            if consume(UInt8(ascii: "}")) {
                return
            }
            guard consume(UInt8(ascii: ",")) else {
                throw ScanError.malformed("expected comma")
            }
        }
        throw ScanError.malformed("unterminated object")
    }

    private mutating func parseArray() throws {
        index += 1
        skipWhitespace()
        if consume(UInt8(ascii: "]")) {
            return
        }
        while index < bytes.count {
            try parseValue()
            skipWhitespace()
            if consume(UInt8(ascii: "]")) {
                return
            }
            guard consume(UInt8(ascii: ",")) else {
                throw ScanError.malformed("expected comma")
            }
        }
        throw ScanError.malformed("unterminated array")
    }

    private mutating func parseString() throws -> String {
        guard consume(UInt8(ascii: "\"")) else {
            throw ScanError.malformed("expected string")
        }
        let start = index - 1
        while index < bytes.count {
            let byte = bytes[index]
            index += 1
            if byte == UInt8(ascii: "\"") {
                let encoded = Data(bytes[start..<index])
                return try JSONDecoder().decode(String.self, from: encoded)
            }
            if byte == UInt8(ascii: "\\") {
                guard index < bytes.count else {
                    throw ScanError.malformed("unterminated escape")
                }
                index += 1
            }
        }
        throw ScanError.malformed("unterminated string")
    }

    private mutating func skipWhitespace() {
        while index < bytes.count, Self.isWhitespace(bytes[index]) {
            index += 1
        }
    }

    private mutating func consume(_ byte: UInt8) -> Bool {
        guard index < bytes.count, bytes[index] == byte else {
            return false
        }
        index += 1
        return true
    }

    private static func isWhitespace(_ byte: UInt8) -> Bool {
        byte == UInt8(ascii: " ") || byte == UInt8(ascii: "\n") ||
            byte == UInt8(ascii: "\r") || byte == UInt8(ascii: "\t")
    }

    private enum ScanError: Error, CustomStringConvertible {
        case duplicate(String)
        case malformed(String)

        var description: String {
            switch self {
            case let .duplicate(member):
                return "duplicate JSON object member \(member)"
            case let .malformed(message):
                return message
            }
        }
    }
}

private func parseCtxVersion(_ raw: String) -> String? {
    let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else {
        return nil
    }
    let parts = trimmed.split(separator: " ")
    if parts.count >= 2, parts[0] == "ctx" {
        return String(parts[1])
    }
    return trimmed
}

private func backendWithRawDataRoot(_ backend: AgentHistoryBackend, _ raw: JSONValue) -> AgentHistoryBackend {
    guard backend.dataRoot == nil else {
        return backend
    }
    let dataRoot = raw["data_root"]?.stringValue ?? raw["dataRoot"]?.stringValue
    return AgentHistoryBackend(kind: backend.kind, dataRoot: dataRoot, baseURL: backend.baseURL)
}

private func normalizeStatus(_ raw: JSONValue) throws -> JSONValue {
    guard case let .object(current) = raw.camelizedPublicJSON().droppingNulls() else {
        return .object(["initialized": .bool(false), "localOnly": .bool(true)])
    }
    let initialized = current["initialized"]
        ?? .bool(current["lexical"]?["generationId"]?.stringValue != nil)
    var status: [String: JSONValue] = [
        "initialized": initialized,
        "localOnly": .bool(true)
    ]
    for key in [
        "dataRoot", "readOnly", "indexedItems", "indexedSessions", "indexedEvents",
        "indexedSources", "historyEpoch", "lexical", "refresh", "semantic", "daemon"
    ] {
        if let value = current[key] {
            if ["indexedItems", "indexedSessions", "indexedEvents", "indexedSources"].contains(key) {
                guard let counter = value.intValue,
                      counter >= 0,
                      counter <= AgentHistoryStatus.maximumExactCounter
                else {
                    throw CtxAgentHistorySDKError(
                        code: .decodeError,
                        message: "ctx status counter \(key) is outside the exact JSON integer domain",
                        details: .object([
                            "field": .string(key),
                            "maximum": .number(Decimal(AgentHistoryStatus.maximumExactCounter))
                        ])
                    )
                }
            }
            status[key] = value
        }
    }
    return .object(status).droppingNulls()
}

private func normalizeSources(_ raw: JSONValue) -> [JSONValue] {
    raw["sources"]?.arrayValue?.map { $0.camelizedPublicJSON().droppingNulls() } ?? []
}

private func normalizeImport(_ raw: JSONValue) -> JSONValue {
    guard case let .object(object) = raw else {
        return .object(["resume": .bool(false), "totals": .object([:]), "sources": .array([])])
    }
    return .object([
        "resume": object["resume"] ?? .bool(false),
        "resumeMode": object["resume_mode"] ?? object["resumeMode"] ?? .null,
        "totals": (object["totals"] ?? .object([:])).camelizedPublicJSON(),
        "sources": .array((object["sources"]?.arrayValue ?? []).map { $0.camelizedPublicJSON() })
    ]).droppingNulls()
}

private func normalizeSearch(_ raw: JSONValue) -> JSONValue {
    guard case let .object(object) = raw else {
        return .object(["query": .null, "results": .array([])]).droppingNulls()
    }
    var search = raw.camelizedPublicJSON().objectValue ?? [:]
    search["query"] = object["query"] ?? search["query"] ?? .null
    search["filters"] = (object["filters"] ?? .object([:])).camelizedPublicJSON()
    search["freshness"] = (object["freshness"] ?? .object([:])).camelizedPublicJSON()
    search["generatedAt"] = object["generated_at"] ?? object["generatedAt"] ?? search["generatedAt"] ?? .null
    search["results"] = .array((object["results"]?.arrayValue ?? []).map { normalizeSearchHit($0) })
    if let pagination = object["pagination"] {
        search["pagination"] = pagination.camelizedPublicJSON()
    } else if let resultWindow = search["resultWindow"]?.objectValue {
        var pagination: [String: JSONValue] = [:]
        pagination["limit"] = resultWindow["limit"]
        pagination["hasMore"] = resultWindow["moreAvailable"]
        search["pagination"] = .object(pagination)
    } else {
        search["pagination"] = .object([:])
    }
    search["truncation"] = (object["truncation"] ?? .object([:])).camelizedPublicJSON()
    return .object(search).droppingNulls()
}

private func normalizeSearchHit(_ raw: JSONValue) -> JSONValue {
    raw.camelizedPublicJSON()
}

private func normalizeEvent(_ raw: JSONValue) throws -> JSONValue {
    let event = try normalizeEventRecord(raw["event"])
    let events = try (raw["events"]?.arrayValue ?? [])
        .map { try normalizeEventRecord($0) }
        .compactMap { $0 }
    var result: [String: JSONValue] = ["events": .array(events)]
    if let event {
        result["event"] = event
    }
    return .object(result)
}

private func normalizeEventRecord(_ raw: JSONValue?) throws -> JSONValue? {
    guard let raw else {
        return nil
    }
    guard case let .object(eventObject) = raw else {
        return raw.camelizedPublicJSON().droppingNulls()
    }

    let exactMCPWireKeys = Set(["mcp_tool_call", "mcpToolCall"])
    if eventObject.keys.contains(where: {
        !exactMCPWireKeys.contains($0) && JSONValue.camelizedPublicKey($0) == "mcpToolCall"
    }) {
        throw invalidMCPWire("transformed outer alias")
    }

    let exactMCPExchangeWireKeys = Set(["mcp_exchange", "mcpExchange"])
    if eventObject.keys.contains(where: {
        !exactMCPExchangeWireKeys.contains($0) && JSONValue.camelizedPublicKey($0) == "mcpExchange"
    }) {
        throw invalidMCPExchangeWire("transformed outer alias")
    }

    let hasSnake = eventObject["mcp_tool_call"] != nil
    let hasCamel = eventObject["mcpToolCall"] != nil
    if hasSnake && hasCamel {
        throw invalidMCPWire("duplicate outer wire aliases")
    }
    let exchangeWireKeys = eventObject.keys.filter { exactMCPExchangeWireKeys.contains($0) }
    if exchangeWireKeys.count > 1 {
        throw invalidMCPExchangeWire("duplicate outer wire aliases")
    }

    var outer = eventObject
    let call: JSONValue?
    if let snake = outer.removeValue(forKey: "mcp_tool_call") {
        call = snake
    } else {
        call = outer.removeValue(forKey: "mcpToolCall")
    }
    let exchange: JSONValue?
    if let snake = outer.removeValue(forKey: "mcp_exchange") {
        exchange = snake
    } else {
        exchange = outer.removeValue(forKey: "mcpExchange")
    }
    let normalized = JSONValue.object(outer)
        .camelizedPublicJSON()
        .droppingNulls()
    guard case let .object(normalizedObject) = normalized else {
        throw invalidMCPWire("event normalization did not produce an object")
    }
    var result = normalizedObject
    if result["mcpToolCall"] != nil {
        throw invalidMCPWire("outer member collides with canonical mcpToolCall")
    }
    if let call {
        result["mcpToolCall"] = call
    }
    if result["mcpExchange"] != nil {
        throw invalidMCPExchangeWire("outer member collides with canonical mcpExchange")
    }
    if let exchange {
        result["mcpExchange"] = try normalizeMCPExchangeWire(exchange)
    }
    return .object(result)
}

private func normalizeSession(_ raw: JSONValue) throws -> JSONValue {
    var session = raw["session"]?.camelizedPublicJSON().objectValue ?? [:]
    if session["ctxSessionId"] == nil, let ctxSessionID = raw["ctx_session_id"] ?? raw["ctxSessionId"] {
        session["ctxSessionId"] = ctxSessionID
    }
    if session["providerSessionId"] == nil, let providerSessionID = raw["provider_session_id"] ?? raw["providerSessionId"] {
        session["providerSessionId"] = providerSessionID
    }
    let events = try (raw["events"]?.arrayValue ?? [])
        .map { try normalizeEventRecord($0) }
        .compactMap { $0 }
    var result: [String: JSONValue] = [
        "session": .object(session),
        "events": .array(events)
    ]
    if let mode = raw["mode"], mode != .null {
        result["mode"] = mode
    }
    if let format = raw["format"], format != .null {
        result["format"] = format
    }
    return .object(result)
}

private func invalidMCPWire(_ message: String) -> CtxAgentHistorySDKError {
    CtxAgentHistorySDKError(
        code: .decodeError,
        message: "agent-history-v1 MCP tool call \(message)",
        details: .object(["field": .string("mcpToolCall")])
    )
}

private func invalidMCPExchangeWire(_ message: String) -> CtxAgentHistorySDKError {
    CtxAgentHistorySDKError(
        code: .decodeError,
        message: "agent-history-v1 MCP exchange \(message)",
        details: .object(["field": .string("mcpExchange")])
    )
}

private func normalizeMCPExchangeWire(_ raw: JSONValue) throws -> JSONValue {
    var exchange = try normalizeClosedMCPObject(
        raw,
        context: "exchange",
        aliases: [
            "provider_call_id": "providerCallId",
            "providerCallId": "providerCallId",
            "invocation": "invocation",
            "response": "response"
        ]
    )
    if let invocation = exchange["invocation"] {
        exchange["invocation"] = try normalizeMCPInvocationWire(invocation)
    }
    if let response = exchange["response"] {
        exchange["response"] = try normalizeMCPResponseWire(response)
    }
    return .object(exchange)
}

private func normalizeMCPInvocationWire(_ raw: JSONValue) throws -> JSONValue {
    var invocation = try normalizeClosedMCPObject(
        raw,
        context: "invocation",
        aliases: ["server": "server", "tool": "tool", "arguments": "arguments"]
    )
    if let arguments = invocation["arguments"] {
        invocation["arguments"] = try normalizeMCPCaptureWire(arguments, context: "invocation.arguments")
    }
    return .object(invocation)
}

private func normalizeMCPResponseWire(_ raw: JSONValue) throws -> JSONValue {
    var response = try normalizeClosedMCPObject(
        raw,
        context: "response",
        aliases: [
            "status": "status",
            "failure_kind": "failureKind",
            "failureKind": "failureKind",
            "duration_ns": "durationNs",
            "durationNs": "durationNs",
            "text": "text",
            "payload": "payload"
        ]
    )
    if let duration = response["durationNs"] {
        try validateMCPWireSafeInteger(duration, context: "response.durationNs")
    }
    if let text = response["text"] {
        response["text"] = try normalizeMCPCaptureWire(text, context: "response.text")
    }
    if let payload = response["payload"] {
        response["payload"] = try normalizeMCPCaptureWire(payload, context: "response.payload")
    }
    return .object(response)
}

private func normalizeMCPCaptureWire(_ raw: JSONValue, context: String) throws -> JSONValue {
    let capture = try normalizeClosedMCPObject(
        raw,
        context: context,
        aliases: [
            "capture_status": "captureStatus",
            "captureStatus": "captureStatus",
            "value": "value",
            "reason": "reason",
            "observed_encoded_bytes": "observedEncodedBytes",
            "observedEncodedBytes": "observedEncodedBytes"
        ]
    )
    if let observed = capture["observedEncodedBytes"] {
        try validateMCPWireSafeInteger(observed, context: "\(context).observedEncodedBytes")
    }
    return .object(capture)
}

private func normalizeClosedMCPObject(
    _ raw: JSONValue,
    context: String,
    aliases: [String: String]
) throws -> [String: JSONValue] {
    guard case let .object(object) = raw else {
        throw invalidMCPExchangeWire("\(context) must be an object")
    }
    var normalized: [String: JSONValue] = [:]
    for (key, value) in object {
        guard let canonical = aliases[key] else {
            throw invalidMCPExchangeWire("\(context) contains unknown member \(key)")
        }
        guard normalized[canonical] == nil else {
            throw invalidMCPExchangeWire("\(context) contains colliding aliases for \(canonical)")
        }
        normalized[canonical] = value
    }
    return normalized
}

private func validateMCPWireSafeInteger(_ value: JSONValue, context: String) throws {
    guard let integer = value.intValue,
          integer >= 0,
          integer <= AgentHistoryMCPExchange.maximumExactInteger
    else {
        throw invalidMCPExchangeWire("\(context) is outside the exact JSON integer domain")
    }
}

private func copyFirst(
    _ keys: [String],
    from source: [String: JSONValue],
    to target: inout [String: JSONValue],
    as targetKey: String,
    defaultValue: JSONValue? = nil
) {
    for key in keys {
        if let value = source[key] {
            target[targetKey] = value
            return
        }
    }
    if let defaultValue {
        target[targetKey] = defaultValue
    }
}
