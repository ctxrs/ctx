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
        try StatusResponse(envelope: localEnvelope(operation: .status, arguments: ["status", "--json"]))
    }

    public func initialize(_ options: InitOptions = InitOptions()) throws -> InitResponse {
        var arguments = ["setup", "--json"]
        appendOption(&arguments, "--progress", options.progress)
        if options.catalogOnly {
            arguments.append("--catalog-only")
        }
        return try InitResponse(envelope: localEnvelope(operation: .initialize, arguments: arguments))
    }

    public func sources() throws -> SourcesResponse {
        try SourcesResponse(envelope: localEnvelope(operation: .sources, arguments: ["sources", "--json"]))
    }

    public func importHistory(_ options: ImportOptions = ImportOptions()) throws -> ImportResponse {
        var arguments = ["import", "--json"]
        appendOption(&arguments, "--progress", options.progress)
        appendImportOptions(&arguments, options)
        return try ImportResponse(envelope: localEnvelope(operation: .importHistory, arguments: arguments))
    }

    public func sync(_ options: ImportOptions = ImportOptions()) throws -> ImportResponse {
        var arguments = ["import", "--json"]
        appendOption(&arguments, "--progress", options.progress)
        appendImportOptions(&arguments, options)
        return try ImportResponse(envelope: localEnvelope(operation: .sync, arguments: arguments))
    }

    public func search(_ query: SearchQueryV1? = nil, options: SearchOptions = SearchOptions()) throws -> SearchResponse {
        try requireSearchIntent(query: query, options: options)
        var arguments = ["search"]
        if let query {
            arguments.append(contentsOf: ["--query-json", try query.jsonString()])
        }
        if let limit = options.limit {
            arguments.append(contentsOf: ["--limit", String(limit)])
        }
        appendOption(&arguments, "--backend", options.backend)
        appendOption(&arguments, "--provider", options.provider)
        appendOption(&arguments, "--history-source", options.historySource)
        appendOption(&arguments, "--provider-key", options.providerKey)
        appendOption(&arguments, "--source-id", options.sourceId)
        appendOption(&arguments, "--source-format", options.sourceFormat)
        appendOption(&arguments, "--workspace", options.workspace)
        appendOption(&arguments, "--since", options.since)
        if options.primaryOnly {
            arguments.append("--primary-only")
        }
        if options.includeSubagents {
            arguments.append("--include-subagents")
        }
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
        arguments.append("--json")
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

    public func locateEvent(_ id: String) throws -> LocateEventResponse {
        try requireID("event id", id)
        return try LocateEventResponse(envelope: localEnvelope(operation: .locateEvent, arguments: ["locate", "event", id, "--format", "json"]))
    }

    public func locateSession(_ id: String) throws -> LocateSessionResponse {
        try locateSession(LocateSessionOptions(id: id))
    }

    public func locateSession(_ options: LocateSessionOptions) throws -> LocateSessionResponse {
        var arguments = ["locate", "session"]
        try appendSessionLookup(&arguments, id: options.id, provider: options.provider, providerSession: options.providerSession)
        arguments.append(contentsOf: ["--format", "json"])
        return try LocateSessionResponse(envelope: localEnvelope(operation: .locateSession, arguments: arguments))
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
                status: try decodeTyped(normalizeStatus(raw), as: AgentHistoryStatus.self, context: "status")
            )
        case .initialize:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                status: try decodeTyped(normalizeStatus(raw), as: AgentHistoryStatus.self, context: "status")
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
                search: try decodeTyped(try normalizeSearch(raw), as: AgentHistorySearchResult.self, context: "search")
            )
        case .showEvent:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                event: try decodeTyped(normalizeEvent(raw), as: AgentHistoryEventResult.self, context: "event")
            )
        case .showSession:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                session: try decodeTyped(normalizeSession(raw), as: AgentHistorySessionResult.self, context: "session")
            )
        case .locateEvent, .locateSession:
            return AgentHistoryEnvelope(
                operation: operation,
                backend: backendWithRawDataRoot(backend, raw),
                location: try decodeTyped(raw.camelizedPublicJSON().droppingNulls(), as: AgentHistoryLocationResult.self, context: "location")
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

private func requireSearchIntent(query: SearchQueryV1?, options: SearchOptions) throws {
    if let limit = options.limit, !(1 ... CTX_SEARCH_MAX_RESULTS).contains(limit) {
        throw CtxAgentHistorySDKError(
            code: .invalidRequest,
            message: "search limit must be an integer between 1 and 200"
        )
    }
    if let query {
        try query.validate()
        return
    }
    if hasSearchText(options.file) {
        return
    }
    throw CtxAgentHistorySDKError(
        code: .invalidRequest,
        message: "search requires a ctx-search-v1 query or file option"
    )
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

private func normalizeStatus(_ raw: JSONValue) -> JSONValue {
    guard case let .object(object) = raw else {
        return .object(["initialized": .bool(false), "localOnly": .bool(true)])
    }
    let initialized: JSONValue
    if let explicit = object["initialized"] {
        initialized = explicit
    } else if let mode = object["mode"]?.stringValue {
        initialized = .bool(mode == "ready" || mode == "catalog_only")
    } else {
        initialized = .bool(false)
    }
    var status: [String: JSONValue] = [
        "initialized": initialized,
        "localOnly": object["local_only"] ?? object["localOnly"] ?? .bool(true)
    ]
    copyFirst(["data_root", "dataRoot"], from: object, to: &status, as: "dataRoot")
    copyFirst(["indexed_items", "indexedItems"], from: object, to: &status, as: "indexedItems", defaultValue: .number(0))
    copyFirst(["indexed_sources", "indexedSources"], from: object, to: &status, as: "indexedSources", defaultValue: .number(0))
    copyFirst(["cataloged_sessions", "catalogedSessions"], from: object, to: &status, as: "catalogedSessions", defaultValue: .number(0))
    copyFirst(["indexed_catalog_sessions", "indexedCatalogSessions"], from: object, to: &status, as: "indexedCatalogSessions")
    copyFirst(["pending_catalog_sessions", "pendingCatalogSessions"], from: object, to: &status, as: "pendingCatalogSessions", defaultValue: .number(0))
    copyFirst(["failed_catalog_sessions", "failedCatalogSessions"], from: object, to: &status, as: "failedCatalogSessions", defaultValue: .number(0))
    copyFirst(["stale_catalog_sessions", "staleCatalogSessions"], from: object, to: &status, as: "staleCatalogSessions", defaultValue: .number(0))
    if let freshness = object["freshness"] {
        status["freshness"] = freshness.camelizedPublicJSON().droppingNulls()
    }
    if let semantic = object["semantic"] {
        status["semantic"] = semantic.camelizedPublicJSON().droppingNulls()
    }
    if let daemon = object["daemon"] {
        status["daemon"] = daemon.camelizedPublicJSON().droppingNulls()
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

private func normalizeSearch(_ raw: JSONValue) throws -> JSONValue {
    guard case let .object(object) = raw else {
        throw CtxAgentHistorySDKError(code: .decodeError, message: "ctx search returned a non-object payload")
    }
    guard object["schema_version"]?.intValue == 2 else {
        throw CtxAgentHistorySDKError(code: .decodeError, message: "ctx search returned an unsupported schema version")
    }
    guard object.keys.contains("query") else {
        throw CtxAgentHistorySDKError(code: .decodeError, message: "ctx search response is missing canonical query")
    }
    guard object["query"] == .null || object["query"]?.objectValue != nil else {
        throw CtxAgentHistorySDKError(code: .decodeError, message: "ctx search response contains a non-object canonical query")
    }
    guard object["query_execution"]?.objectValue != nil else {
        throw CtxAgentHistorySDKError(code: .decodeError, message: "ctx search response is missing query execution diagnostics")
    }
    guard let results = object["results"]?.arrayValue else {
        throw CtxAgentHistorySDKError(code: .decodeError, message: "ctx search response is missing results")
    }
    var search = raw.camelizedPublicJSON().objectValue ?? [:]
    search.removeValue(forKey: "schemaVersion")
    search.removeValue(forKey: "queryExecution")
    search["schema_version"] = .number(2)
    search["query"] = object["query"] ?? search["query"] ?? .null
    search["query_execution"] = object["query_execution"]
    search["filters"] = (object["filters"] ?? .object([:])).camelizedPublicJSON()
    search["freshness"] = (object["freshness"] ?? .object([:])).camelizedPublicJSON()
    search["generatedAt"] = object["generated_at"] ?? object["generatedAt"] ?? search["generatedAt"] ?? .null
    search["results"] = .array(results.map { normalizeSearchHit($0) })
    search["pagination"] = (object["pagination"] ?? .object([:])).camelizedPublicJSON()
    search["truncation"] = (object["truncation"] ?? .object([:])).camelizedPublicJSON()
    return .object(search).droppingNulls()
}

private func normalizeSearchHit(_ raw: JSONValue) -> JSONValue {
    raw.camelizedPublicJSON()
}

private func normalizeEvent(_ raw: JSONValue) -> JSONValue {
    let event = raw["event"]?.camelizedPublicJSON().droppingNulls()
    let events = raw["events"]?.arrayValue?.map { $0.camelizedPublicJSON().droppingNulls() } ?? []
    let source = raw["source"]?.camelizedPublicJSON().droppingNulls()
    return .object([
        "event": event ?? .null,
        "events": .array(events),
        "source": source ?? .null
    ]).droppingNulls()
}

private func normalizeSession(_ raw: JSONValue) -> JSONValue {
    var session = raw["session"]?.camelizedPublicJSON().objectValue ?? [:]
    if session["ctxSessionId"] == nil, let ctxSessionID = raw["ctx_session_id"] ?? raw["ctxSessionId"] {
        session["ctxSessionId"] = ctxSessionID
    }
    if session["providerSessionId"] == nil, let providerSessionID = raw["provider_session_id"] ?? raw["providerSessionId"] {
        session["providerSessionId"] = providerSessionID
    }
    let events = raw["events"]?.arrayValue?.map { $0.camelizedPublicJSON().droppingNulls() } ?? []
    return .object([
        "session": .object(session),
        "events": .array(events),
        "source": raw["source"]?.camelizedPublicJSON().droppingNulls() ?? .null,
        "mode": raw["mode"] ?? .null,
        "format": raw["format"] ?? .null
    ]).droppingNulls()
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
