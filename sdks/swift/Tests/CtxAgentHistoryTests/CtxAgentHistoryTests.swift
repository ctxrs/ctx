import XCTest
@testable import CtxAgentHistory

final class CtxAgentHistoryTests: XCTestCase {
    func testWrapsCoreCLICommands() throws {
        let runner = CapturingRunner { request in
            CommandResult(stdout: #"{"schema_version":1,"initialized":true,"sources":[],"totals":{},"results":[]}"#)
        }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        _ = try client.status()
        _ = try client.initialize(InitOptions(catalogOnly: true))
        _ = try client.sources()
        _ = try client.importHistory(ImportOptions(provider: "codex", resume: true))
        _ = try client.sync(ImportOptions(all: true))

        XCTAssertEqual(
            runner.requests.map(\.arguments),
            [
                ["--data-root", "/tmp/ctx-sdk-test", "status", "--json"],
                ["--data-root", "/tmp/ctx-sdk-test", "setup", "--json", "--progress", "none", "--catalog-only"],
                ["--data-root", "/tmp/ctx-sdk-test", "sources", "--json"],
                ["--data-root", "/tmp/ctx-sdk-test", "import", "--json", "--progress", "none", "--provider", "codex", "--resume"],
                ["--data-root", "/tmp/ctx-sdk-test", "import", "--json", "--progress", "none", "--all"]
            ]
        )
    }

    func testBuildsSearchFlags() throws {
        let runner = CapturingRunner { _ in CommandResult(stdout: Self.searchJSON) }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        _ = try client.search(
            try SearchQueryV1(
                any: [.all("retry handling"), .semantic("timeout backoff behavior")],
                must: [.all("ctx")]
            ),
            options: SearchOptions(
                limit: 5,
                backend: "hybrid",
                provider: "custom",
                historySource: "dorkos/default",
                providerKey: "dorkos",
                sourceId: "default",
                sourceFormat: "dorkos-history-v1",
                workspace: "ctx",
                since: "30d",
                primaryOnly: true,
                eventType: "message",
                file: "crates/foo/src/lib.rs",
                session: "00000000-0000-0000-0000-000000000001",
                events: true,
                refresh: "off",
                includeCurrentSession: true
            )
        )

        XCTAssertEqual(
            runner.requests[0].arguments,
            [
                "--data-root", "/tmp/ctx-sdk-test",
                "search", "--query-json", #"{"any":[{"all":"retry handling"},{"semantic":"timeout backoff behavior"}],"must":[{"all":"ctx"}],"version":"ctx-search-v1"}"#,
                "--limit", "5",
                "--backend", "hybrid",
                "--provider", "custom",
                "--history-source", "dorkos/default",
                "--provider-key", "dorkos",
                "--source-id", "default",
                "--source-format", "dorkos-history-v1",
                "--workspace", "ctx",
                "--since", "30d",
                "--primary-only",
                "--event-type", "message",
                "--file", "crates/foo/src/lib.rs",
                "--session", "00000000-0000-0000-0000-000000000001",
                "--events",
                "--refresh", "off",
                "--include-current-session",
                "--json"
            ]
        )
    }

    func testCanonicalizesAndDeduplicatesSearchClausesBeforeBounds() throws {
        let query = try SearchQueryV1(
            any: [
                .all("  disk\t io\u{00A0}pressure "),
                .all("disk io pressure"),
                .literal("  logs_2.db  raw  ")
            ],
            mustNot: [.phrase(" postgres\n vacuum ")]
        )

        XCTAssertEqual(
            try query.jsonString(),
            #"{"any":[{"all":"disk io pressure"},{"literal":"logs_2.db  raw"}],"must_not":[{"phrase":"postgres vacuum"}],"version":"ctx-search-v1"}"#
        )

        let unicodeDuplicates = Array(
            repeating: SearchClause.all("  cafe\u{0301}\u{00A0}\u{4E16}\u{754C}  "),
            count: 33
        )
        let deduplicated = try SearchQueryV1(any: unicodeDuplicates)
        XCTAssertEqual(deduplicated.any, [.all("cafe\u{0301} \u{4E16}\u{754C}")])

        XCTAssertNoThrow(try SearchQueryV1(any: (0 ..< 32).map { .all("term\($0)") }))
        XCTAssertThrowsError(try SearchQueryV1(any: (0 ..< 33).map { .all("term\($0)") }))
        XCTAssertThrowsError(try SearchQueryV1(any: [.all("!!!")]))
        XCTAssertThrowsError(
            try SearchQueryV1(any: [.all((0 ..< 33).map { "term\($0)" }.joined(separator: " "))])
        )
    }

    func testValidatesExplicitSearchLimitBeforeTransport() throws {
        let runner = CapturingRunner { _ in CommandResult(stdout: Self.searchJSON) }
        let client = AgentHistoryClient(adapter: LocalCLIAdapter(runner: runner))
        let query = try SearchQueryV1.all("bounded limit")

        for limit in [-1, 0, 201] {
            XCTAssertThrowsError(try client.search(query, options: SearchOptions(limit: limit))) { error in
                XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .invalidRequest)
            }
        }
        XCTAssertTrue(runner.requests.isEmpty)

        _ = try client.search(query, options: SearchOptions(limit: 1))
        _ = try client.search(query, options: SearchOptions(limit: 200))
        XCTAssertTrue(runner.requests[0].arguments.contains("1"))
        XCTAssertTrue(runner.requests[1].arguments.contains("200"))
    }

    func testWrapsShowAndLocateCommands() throws {
        let runner = CapturingRunner { request in
            if request.arguments.contains("locate") {
                return CommandResult(stdout: Self.locationJSON)
            }
            return CommandResult(stdout: #"{"events":[]}"#)
        }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        _ = try client.showEvent("00000000-0000-0000-0000-000000000002", options: ShowEventOptions(window: 3))
        _ = try client.showSession("00000000-0000-0000-0000-000000000003", options: ShowSessionOptions(mode: "full"))
        _ = try client.showSession(ShowSessionOptions(provider: "codex", providerSession: "codex-session", mode: "log"))
        _ = try client.locateEvent("00000000-0000-0000-0000-000000000004")
        _ = try client.locateSession(LocateSessionOptions(provider: "codex", providerSession: "codex-session"))

        XCTAssertEqual(
            runner.requests.map { Array($0.arguments.dropFirst(2)) },
            [
                ["show", "event", "00000000-0000-0000-0000-000000000002", "--format", "json", "--window", "3"],
                ["show", "session", "00000000-0000-0000-0000-000000000003", "--mode", "full", "--format", "json"],
                ["show", "session", "--provider", "codex", "--provider-session", "codex-session", "--mode", "log", "--format", "json"],
                ["locate", "event", "00000000-0000-0000-0000-000000000004", "--format", "json"],
                ["locate", "session", "--provider", "codex", "--provider-session", "codex-session", "--format", "json"]
            ]
        )
    }

    func testReturnsTypedOperationPayloads() throws {
        let runner = CapturingRunner { request in
            switch Array(request.arguments.dropFirst(2).prefix(2)) {
            case ["status", "--json"]:
                return CommandResult(stdout: Self.statusJSON)
            case ["search", "--query-json"]:
                return CommandResult(stdout: Self.searchJSON)
            case ["show", "event"]:
                return CommandResult(stdout: Self.eventJSON)
            case ["show", "session"]:
                return CommandResult(stdout: Self.sessionJSON)
            case ["locate", "event"], ["locate", "session"]:
                return CommandResult(stdout: Self.locationJSON)
            default:
                return CommandResult(stdout: #"{"events":[]}"#)
            }
        }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        let status = try client.status()
        XCTAssertEqual(status.status.initialized, true)
        XCTAssertEqual(status.status.indexedItems, 3)

        let search = try client.search(try .all("local agent history"), options: SearchOptions(limit: 1, refresh: "off"))
        XCTAssertEqual(search.search.schemaVersion, 2)
        XCTAssertEqual(search.search.query?.any.first?.value, "local agent history")
        XCTAssertEqual(search.search.queryExecution.queryVersion, CTX_SEARCH_V1_VERSION)
        XCTAssertEqual(search.search.queryExecution.resolved.candidateRows, 16_384)
        XCTAssertEqual(search.search.queryExecution.semantic.effectiveBackend, "lexical")
        XCTAssertEqual(search.search.results.first?.resultType, "event")
        XCTAssertEqual(search.search.results.first?.resultScope, "event")
        XCTAssertEqual(search.search.results.first?.citations.first?.targetType, "event")
        XCTAssertEqual(search.search.results.first?.citations.first?.label, "codex event")
        let retrieval = try XCTUnwrap(search.search.retrieval?.objectValue)
        XCTAssertEqual(retrieval["requestedMode"], .string("hybrid"))
        XCTAssertNil(retrieval["semanticWeight"])
        XCTAssertNil(retrieval["semanticFallbackCode"])
        XCTAssertNil(retrieval["semanticFallback"])
        XCTAssertEqual(retrieval["coverage"]?["embeddedItems"], .number(4))

        let event = try client.showEvent("11111111-1111-4111-8111-111111111111")
        XCTAssertEqual(event.event.event?.text, "local agent history search result")
        XCTAssertEqual(event.event.source?.sourceFormat, "codex_session_jsonl")

        let session = try client.showSession("22222222-2222-4222-8222-222222222222")
        XCTAssertEqual(session.session.session?.providerSessionId, "codex-fixture-session")
        XCTAssertEqual(session.session.events.first?.text, "local agent history search result")

        let location = try client.locateEvent("11111111-1111-4111-8111-111111111111")
        XCTAssertEqual(location.location.provider, "codex")
        XCTAssertEqual(location.location.resume?.cursor, "line:2")
    }

    func testRejectsFractionalAndMissingSchemaV2Fields() throws {
        let base = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(Self.searchJSON.utf8)) as? [String: Any]
        )
        var payloads: [(String, [String: Any])] = []

        var fractionalVersion = base
        fractionalVersion["schema_version"] = 2.5
        payloads.append(("fractional schema version", fractionalVersion))

        for field in ["schema_version", "query", "query_execution", "results"] {
            var missing = base
            missing.removeValue(forKey: field)
            payloads.append(("missing \(field)", missing))
        }

        var fractionalExecution = base
        var execution = try XCTUnwrap(fractionalExecution["query_execution"] as? [String: Any])
        var resolved = try XCTUnwrap(execution["resolved"] as? [String: Any])
        resolved["query_bytes"] = 1.5
        execution["resolved"] = resolved
        fractionalExecution["query_execution"] = execution
        payloads.append(("fractional execution integer", fractionalExecution))

        var missingExecutionField = base
        execution = try XCTUnwrap(missingExecutionField["query_execution"] as? [String: Any])
        resolved = try XCTUnwrap(execution["resolved"] as? [String: Any])
        resolved.removeValue(forKey: "query_bytes")
        execution["resolved"] = resolved
        missingExecutionField["query_execution"] = execution
        payloads.append(("missing execution integer", missingExecutionField))

        var missingHitField = base
        var results = try XCTUnwrap(missingHitField["results"] as? [[String: Any]])
        results[0].removeValue(forKey: "result_scope")
        missingHitField["results"] = results
        payloads.append(("missing result scope", missingHitField))

        var fractionalHitField = base
        results = try XCTUnwrap(fractionalHitField["results"] as? [[String: Any]])
        results[0]["event_seq"] = 1.5
        fractionalHitField["results"] = results
        payloads.append(("fractional event sequence", fractionalHitField))

        for (name, payload) in payloads {
            let data = try JSONSerialization.data(withJSONObject: payload)
            let runner = CapturingRunner { _ in CommandResult(stdout: data) }
            let client = AgentHistoryClient(adapter: LocalCLIAdapter(runner: runner))
            XCTAssertThrowsError(try client.search(try .all("strict schema")), name) { error in
                XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .decodeError, name)
            }
        }
    }

    func testVersioningMetadata() throws {
        let runner = CapturingRunner { request in
            XCTAssertEqual(request.arguments, ["--version"])
            return CommandResult(stdout: "ctx 1.2.3\n")
        }
        let client = AgentHistoryClient(adapter: LocalCLIAdapter(runner: runner))

        let version = try client.version()

        XCTAssertEqual(version.schemaVersion, 1)
        XCTAssertEqual(version.apiVersion, AGENT_HISTORY_V1_VERSION)
        XCTAssertEqual(version.sdkVersion, CTX_AGENT_HISTORY_SWIFT_SDK_VERSION)
        XCTAssertEqual(version.adapter, "local-cli")
        XCTAssertEqual(version.ctxVersion, "1.2.3")
        XCTAssertEqual(try client.versioning()["api_version"]?.stringValue, AGENT_HISTORY_V1_VERSION)
    }

    func testStructuredErrors() throws {
        let cli = AgentHistoryClient(
            adapter: LocalCLIAdapter(runner: CapturingRunner { _ in
                CommandResult(stdout: "", stderr: "bad flag\n", exitCode: 2)
            })
        )
        XCTAssertThrowsError(try cli.status()) { error in
            let sdkError = error as? CtxAgentHistorySDKError
            XCTAssertEqual(sdkError?.code, .adapterError)
            XCTAssertEqual(sdkError?.exitCode, 2)
            XCTAssertEqual(sdkError?.stderr, "bad flag\n")
        }

        let parse = AgentHistoryClient(adapter: LocalCLIAdapter(runner: CapturingRunner { _ in CommandResult(stdout: "not json") }))
        XCTAssertThrowsError(try parse.status()) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .decodeError)
        }

        XCTAssertThrowsError(try parse.showEvent("")) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .invalidRequest)
        }
        XCTAssertThrowsError(try parse.showSession(ShowSessionOptions(provider: "codex"))) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .invalidRequest)
        }
        XCTAssertThrowsError(try parse.search(options: SearchOptions(refresh: "off"))) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .invalidRequest)
        }
        XCTAssertThrowsError(try parse.search(try SearchQueryV1(mustNot: [.all("negative only")]))) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .invalidRequest)
        }
        XCTAssertThrowsError(try parse.search(try SearchQueryV1(must: [.semantic("invalid placement")]))) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .invalidRequest)
        }
    }

    func testAllStructuredErrorCodesRoundTripThroughContractError() throws {
        let codes: [AgentHistoryErrorCode] = [
            .invalidRequest,
            .notFound,
            .notInitialized,
            .backendUnavailable,
            .timeout,
            .cancelled,
            .notSupported,
            .adapterError,
            .decodeError,
            .unknown
        ]
        let encoder = JSONEncoder()
        let decoder = JSONDecoder()

        for code in codes {
            let contractError = CtxAgentHistorySDKError(code: code, message: code.rawValue).contractError
            let decoded = try decoder.decode(AgentHistoryContractError.self, from: encoder.encode(contractError))
            XCTAssertEqual(decoded.code, code)
            XCTAssertEqual(decoded.message, code.rawValue)
        }
    }

    func testCamelizedPublicJSONOmitsRawMetadataKeys() throws {
        let raw = try JSONValue.from([
            "payload_type": "search_results",
            "payloadType": "search_results",
            "result_type": "event",
            "record_type": "event",
            "recordType": "event",
            "item_type": "event",
            "itemType": "event",
            "target_type": "event"
        ])
        let normalized = raw.camelizedPublicJSON().objectValue ?? [:]

        XCTAssertNil(normalized["payloadType"])
        XCTAssertNil(normalized["recordType"])
        XCTAssertNil(normalized["itemType"])
        XCTAssertEqual(normalized["resultType"], .string("event"))
        XCTAssertEqual(normalized["targetType"], .string("event"))
    }

    func testHostedClientIsExplicitPlaceholder() throws {
        let client = AgentHistoryClient.hosted(
            HostedConfig(baseURL: URL(string: "https://ctx.example.invalid"))
        )

        let version = try client.version()
        XCTAssertEqual(version.adapter, "hosted-placeholder")
        XCTAssertEqual(version.hosted, false)
        XCTAssertThrowsError(try client.status()) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .notSupported)
        }
    }

    func testDecodesBundledContractFixtures() throws {
        let decoder = JSONDecoder()
        let fixturesDirectory = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("contracts/agent-history-v1/fixtures", isDirectory: true)
        let fixtureURLs = try FileManager.default
            .contentsOfDirectory(at: fixturesDirectory, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
        XCTAssertFalse(fixtureURLs.isEmpty)

        for url in fixtureURLs {
            let envelope = try decoder.decode(AgentHistoryEnvelope.self, from: Data(contentsOf: url))
            XCTAssertEqual(envelope.contractVersion, AGENT_HISTORY_V1_VERSION, url.lastPathComponent)
            XCTAssertEqual(envelope.schemaVersion, 1, url.lastPathComponent)
            switch envelope.operation {
            case .status:
                XCTAssertEqual(envelope.status?.initialized, true, url.lastPathComponent)
            case .sources:
                XCTAssertEqual(envelope.sources?.first?.provider, "codex", url.lastPathComponent)
            case .importHistory:
                XCTAssertEqual(envelope.importResult?.totals.importedEvents, 2, url.lastPathComponent)
            case .search:
                XCTAssertNotNil(envelope.search?.results, url.lastPathComponent)
                if let first = envelope.search?.results.first {
                    XCTAssertEqual(first.resultScope, "event", url.lastPathComponent)
                }
            case .showEvent:
                XCTAssertEqual(envelope.event?.events.first?.ctxEventId, "11111111-1111-4111-8111-111111111111", url.lastPathComponent)
            case .showSession:
                XCTAssertEqual(envelope.session?.session?.title, "Fixture session", url.lastPathComponent)
            case .locateEvent:
                XCTAssertEqual(envelope.location?.source.cursor, "line:2", url.lastPathComponent)
            case .locateSession:
                XCTAssertEqual(envelope.location?.source.cursor, "session:codex-fixture-session", url.lastPathComponent)
            case .initialize, .sync, .error:
                break
            }
        }
    }

    private static let statusJSON = #"{"initialized":true,"local_only":true,"data_root":"/tmp/ctx-sdk-test","indexed_items":3,"indexed_sources":1,"cataloged_sessions":1}"#
    private static let searchJSON = """
    {"schema_version":2,"query":{"version":"ctx-search-v1","any":[{"all":"local agent history"}]},"query_execution":{"query_version":"ctx-search-v1","candidate_strategy":"bounded_fts","resolved":{"query_bytes":8192,"clauses":32,"analyzed_tokens_per_clause":32,"candidates_per_positive_seed":1024,"candidate_rows":16384,"retained_candidate_ids":8192,"residual_rows":8192,"verification_bytes":16777216,"verification_lookup_bytes":16384,"hydrated_rows":256,"hydration_input_bytes":8388608,"hydration_input_bytes_per_event":65536,"snippet_input_bytes":8388608,"returned_text_bytes":524288,"serialized_response_bytes":2097152,"results":200,"elapsed_ms":1000},"consumed":{"query_bytes":19,"clauses":1,"analyzed_tokens":3,"largest_analyzed_tokens_per_clause":3,"largest_positive_seed_candidates":1,"candidate_rows":1,"retained_candidate_ids":1,"residual_rows":1,"verification_bytes":16,"largest_verification_lookup_bytes":16,"hydrated_rows":1,"legacy_fallback_rows":0,"hydration_input_bytes":32,"largest_hydration_input_bytes":32,"snippet_input_bytes":32,"returned_results":1,"returned_text_bytes":32,"serialized_response_bytes":1000,"elapsed_ms":2},"semantic":{"attempted":false,"required":false,"readiness":"unavailable","effective_backend":"lexical","requested_candidates":0,"eligible_candidates":0,"candidates_supplied":0,"candidates_consumed":0,"candidates_used":0,"coverage":{},"completeness":"not_attempted","positive_text_rule_version":"ctx-search-positive-text-v1"},"rrf_k":60,"per_branch_candidate_rows":1024,"requested_result_limit":1,"result_limit":1,"max_result_limit":200,"clauses_executed":1,"verification_dropped":0,"filter_verification_dropped":0,"candidate_budget_exhausted":false,"timed_out":false,"truncated":false},"filters":{"provider":"codex"},"freshness":{"mode":"off","status":"skipped"},"retrieval":{"requested_mode":"hybrid","effective_mode":"lexical","semantic_weight":0.0,"semantic_fallback_code":"semantic_retrieval_failed","semantic_fallback":"semantic_retrieval_failed","coverage":{"embedded_items":4}},"results":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","result_type":"event","result_scope":"event","provider":"codex","snippet":"local agent history search result","citations":[{"target_type":"event","label":"codex event"}]}]}
    """
    private static let eventJSON = #"{"event":{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","sequence":1,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","source":"codex","cursor":"line:2","text":"local agent history search result","redaction_state":"redacted"},"events":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","sequence":1,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","source":"codex","cursor":"line:2","text":"local agent history search result","redaction_state":"redacted"}],"source":{"path":"/tmp/ctx-sdk-fixture/session.jsonl","cursor":"line:2","exists":true,"source_id":"33333333-3333-4333-8333-333333333333","source_format":"codex_session_jsonl"}}"#
    private static let sessionJSON = #"{"session":{"ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","title":"Fixture session"},"events":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","sequence":1,"event_type":"message","role":"assistant","text":"local agent history search result"}],"source":{"path":"/tmp/ctx-sdk-fixture/session.jsonl","exists":true,"source_format":"codex_session_jsonl"},"mode":"lite","format":"json"}"#
    private static let locationJSON = #"{"ctx_session_id":"22222222-2222-4222-8222-222222222222","ctx_event_id":"11111111-1111-4111-8111-111111111111","provider":"codex","provider_session_id":"codex-fixture-session","source":{"path":"/tmp/ctx-sdk-fixture/session.jsonl","cursor":"line:2","exists":true,"source_id":"33333333-3333-4333-8333-333333333333","source_format":"codex_session_jsonl"},"resume":{"cursor":"line:2"}}"#
}

private final class CapturingRunner: CommandRunner, @unchecked Sendable {
    private let handler: (CommandRequest) throws -> CommandResult
    private(set) var requests: [CommandRequest] = []

    init(handler: @escaping (CommandRequest) throws -> CommandResult) {
        self.handler = handler
    }

    func run(_ request: CommandRequest) throws -> CommandResult {
        requests.append(request)
        return try handler(request)
    }
}
