import XCTest
@testable import CtxAgentHistory

final class CtxAgentHistoryTests: XCTestCase {
    func testForcesAnalyticsOffAfterAmbientAndUserEnvironmentMerging() throws {
        let variable = "CTX_ANALYTICS_ENABLED"
        let original = ProcessInfo.processInfo.environment[variable]
        setenv(variable, "true", 1)
        defer {
            if let original {
                setenv(variable, original, 1)
            } else {
                unsetenv(variable)
            }
        }

        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ctx-sdk-privacy-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let script = directory.appendingPathComponent("ctx-fake")
        try Data(
            """
            #!/bin/sh
            set -eu
            printf '{"analyticsEnabled":"%s"}\\n' "$CTX_ANALYTICS_ENABLED"
            """.utf8
        ).write(to: script)
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o700)],
            ofItemAtPath: script.path
        )

        XCTAssertEqual(ProcessInfo.processInfo.environment[variable], "true")
        let adapter = LocalCLIAdapter(
            ctxPath: script.path,
            env: [variable: "true"]
        )
        let output = try adapter.execute(["status", "--format=json"])
        let raw = try XCTUnwrap(
            JSONSerialization.jsonObject(with: output) as? [String: String]
        )
        XCTAssertEqual(raw["analyticsEnabled"], "false")
    }

    func testStatusCountersUseTheExactCrossSDKIntegerDomain() throws {
        let decoder = JSONDecoder()
        let maximum = AgentHistoryStatus.maximumExactCounter
        let accepted = try decoder.decode(
            AgentHistoryStatus.self,
            from: Data(
                """
                {"initialized":true,"localOnly":true,"indexedItems":\(maximum),"indexedSessions":\(maximum),"indexedEvents":\(maximum),"indexedSources":\(maximum)}
                """.utf8
            )
        )
        XCTAssertEqual(accepted.indexedItems, maximum)
        XCTAssertEqual(accepted.indexedSessions, maximum)
        XCTAssertEqual(accepted.indexedEvents, maximum)
        XCTAssertEqual(accepted.indexedSources, maximum)

        for rejected in ["9007199254740993", "9223372036854775807"] {
            XCTAssertThrowsError(
                try decoder.decode(
                    AgentHistoryStatus.self,
                    from: Data(
                        "{\"initialized\":true,\"localOnly\":true,\"indexedItems\":\(rejected)}".utf8
                    )
                )
            )
        }

        let invalid = AgentHistoryStatus(
            initialized: true,
            localOnly: true,
            indexedItems: maximum + 2
        )
        XCTAssertThrowsError(try JSONEncoder().encode(invalid))
    }

    func testWrapsCoreCLICommands() throws {
        let runner = CapturingRunner { request in
            CommandResult(stdout: #"{"schema_version":1,"initialized":true,"sources":[],"totals":{},"results":[]}"#)
        }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        _ = try client.status()
        _ = try client.initialize(InitOptions())
        _ = try client.sources()
        _ = try client.importHistory(ImportOptions(provider: "codex", resume: true))
        _ = try client.sync(ImportOptions(all: true))

        XCTAssertEqual(
            runner.requests.map(\.arguments),
            [
                ["--data-root", "/tmp/ctx-sdk-test", "status", "--format=json"],
                ["--data-root", "/tmp/ctx-sdk-test", "setup", "--format=json", "--progress", "none"],
                ["--data-root", "/tmp/ctx-sdk-test", "sources", "--format=json"],
                ["--data-root", "/tmp/ctx-sdk-test", "import", "--format=json", "--progress", "none", "--provider", "codex", "--resume"],
                ["--data-root", "/tmp/ctx-sdk-test", "import", "--format=json", "--progress", "none", "--all"]
            ]
        )
    }

    func testBuildsSearchFlags() throws {
        let runner = CapturingRunner { _ in CommandResult(stdout: #"{"results":[]}"#) }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        _ = try client.search(
            "retry handling",
            options: SearchOptions(
                terms: ["timeout", "backoff"],
                limit: 5,
                backend: "hybrid",
                semanticWeight: 0.35,
                provider: "codex",
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
                "search", "retry handling",
                "--term", "timeout",
                "--term", "backoff",
                "--limit", "5",
                "--backend", "hybrid",
                "--semantic-weight", "0.35",
                "--provider", "codex",
                "--workspace", "ctx",
                "--since", "30d",
                "--primary-only",
                "--event-type", "message",
                "--file", "crates/foo/src/lib.rs",
                "--session", "00000000-0000-0000-0000-000000000001",
                "--events",
                "--refresh", "off",
                "--include-current-session",
                "--format=json"
            ]
        )
    }

    func testWrapsShowCommands() throws {
        let runner = CapturingRunner { _ in CommandResult(stdout: #"{"events":[]}"#) }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        _ = try client.showEvent("00000000-0000-0000-0000-000000000002", options: ShowEventOptions(window: 3))
        _ = try client.showSession("00000000-0000-0000-0000-000000000003", options: ShowSessionOptions(mode: "full"))
        _ = try client.showSession(ShowSessionOptions(provider: "codex", providerSession: "codex-session", mode: "log"))

        XCTAssertEqual(
            runner.requests.map { Array($0.arguments.dropFirst(2)) },
            [
                ["show", "event", "00000000-0000-0000-0000-000000000002", "--format", "json", "--window", "3"],
                ["show", "session", "00000000-0000-0000-0000-000000000003", "--mode", "full", "--format", "json"],
                ["show", "session", "--provider", "codex", "--provider-session", "codex-session", "--mode", "log", "--format", "json"]
            ]
        )
    }

    func testReturnsTypedOperationPayloads() throws {
        let runner = CapturingRunner { request in
            switch Array(request.arguments.dropFirst(2).prefix(2)) {
            case ["status", "--format=json"]:
                return CommandResult(stdout: Self.statusJSON)
            case ["search", "local agent history"]:
                return CommandResult(stdout: Self.searchJSON)
            case ["show", "event"]:
                return CommandResult(stdout: Self.eventJSON)
            case ["show", "session"]:
                return CommandResult(stdout: Self.sessionJSON)
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

        let search = try client.search("local agent history", options: SearchOptions(limit: 1, refresh: "off"))
        XCTAssertEqual(search.search.query, "local agent history")
        XCTAssertEqual(search.search.retrieval?["requestedMode"]?.stringValue, "hybrid")
        XCTAssertEqual(search.search.retrieval?["effectiveMode"]?.stringValue, "lexical")
        XCTAssertEqual(search.search.retrieval?["semanticWeight"], .number(0.0))
        XCTAssertEqual(search.search.retrieval?["semanticFallbackCode"]?.stringValue, "semantic_retrieval_failed")
        XCTAssertEqual(search.search.retrieval?["semanticFallback"]?.stringValue, "semantic_retrieval_failed")
        XCTAssertEqual(search.search.retrieval?["coverage"]?["embeddedItems"]?.intValue, 4)
        XCTAssertEqual(search.search.retrieval?["diagnostics"]?["queryEmbedMs"]?.intValue, 2)
        XCTAssertEqual(search.search.results.first?.rank, 1.0)
        XCTAssertEqual(search.search.results.first?.retrievalScore, 0.98)
        XCTAssertEqual(search.search.results.first?.resultType, "event")
        XCTAssertEqual(search.search.results.first?.resultScope, "event")
        XCTAssertEqual(search.search.results.first?.sourceFormat, "codex_session_jsonl")
        XCTAssertEqual(search.search.results.first?.citations.first?.targetType, "event")
        XCTAssertEqual(search.search.results.first?.citations.first?.label, "codex event")
        XCTAssertEqual(search.search.resultWindow?.limit, 1)
        XCTAssertEqual(search.search.resultWindow?.returned, 1)
        XCTAssertEqual(search.search.resultWindow?.moreAvailable, true)
        XCTAssertEqual(search.search.pagination?.limit, 1)
        XCTAssertEqual(search.search.pagination?.hasMore, true)

        let event = try client.showEvent("11111111-1111-4111-8111-111111111111")
        XCTAssertEqual(event.event.event?.text, "local agent history search result")
        XCTAssertEqual(event.event.event?.providerSessionId, "codex-fixture-session")
        XCTAssertEqual(event.event.event?.sourceFormat, "codex_session_jsonl")
        XCTAssertEqual(event.event.event?.content?.complete, true)
        XCTAssertEqual(event.event.event?.content?.policyStatus, .selected)
        XCTAssertEqual(event.event.event?.structuredContent?["kind"]?.stringValue, "toolResult")
        let structuredItems = event.event.event?.structuredContent?["payload"]?["items"]?.arrayValue
        let nestedStructuredValues = structuredItems?[2]["nested"]?.arrayValue
        XCTAssertEqual(nestedStructuredValues?[1], .bool(false))

        let session = try client.showSession("22222222-2222-4222-8222-222222222222")
        XCTAssertEqual(session.session.session?.providerSessionId, "codex-fixture-session")
        XCTAssertEqual(session.session.session?.sourceFormat, "codex_session_jsonl")
        XCTAssertEqual(session.session.events.first?.text, "local agent history search result")

    }

    func testVersioningMetadata() throws {
        let runner = CapturingRunner { request in
            XCTAssertEqual(request.arguments, ["--version"])
            XCTAssertEqual(request.env["CTX_ANALYTICS_ENABLED"], "false")
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
        XCTAssertThrowsError(try parse.search("   ")) { error in
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
            "target_type": "event",
            "acquisition": [
                "source": "local_scan",
                "cursor": "opaque-checkpoint"
            ]
        ])
        let normalized = raw.camelizedPublicJSON().objectValue ?? [:]

        XCTAssertNil(normalized["payloadType"])
        XCTAssertNil(normalized["recordType"])
        XCTAssertNil(normalized["itemType"])
        XCTAssertEqual(normalized["resultType"], .string("event"))
        XCTAssertEqual(normalized["targetType"], .string("event"))
        XCTAssertEqual(normalized["acquisition"]?["source"], .string("local_scan"))
        XCTAssertEqual(normalized["acquisition"]?["cursor"], .string("opaque-checkpoint"))
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
                XCTAssertEqual(
                    envelope.search?.resultWindow?.returned,
                    envelope.search?.results.count,
                    url.lastPathComponent
                )
                XCTAssertEqual(
                    envelope.search?.resultWindow?.limit,
                    envelope.search?.pagination?.limit,
                    url.lastPathComponent
                )
                XCTAssertEqual(
                    envelope.search?.resultWindow?.moreAvailable,
                    envelope.search?.pagination?.hasMore,
                    url.lastPathComponent
                )
                if let first = envelope.search?.results.first {
                    XCTAssertEqual(first.rank, 1.0, url.lastPathComponent)
                    XCTAssertEqual(first.retrievalScore, 0.98, url.lastPathComponent)
                    XCTAssertEqual(first.resultScope, "event", url.lastPathComponent)
                }
            case .showEvent:
                XCTAssertEqual(envelope.event?.events.first?.ctxEventId, "11111111-1111-4111-8111-111111111111", url.lastPathComponent)
                XCTAssertEqual(envelope.event?.events.first?.content?.complete, true, url.lastPathComponent)
                XCTAssertEqual(envelope.event?.events.first?.content?.policyStatus, .selected, url.lastPathComponent)
                XCTAssertEqual(
                    envelope.event?.event?.structuredContent?["kind"]?.stringValue,
                    "toolResult",
                    url.lastPathComponent
                )
            case .showSession:
                XCTAssertEqual(envelope.session?.session?.title, "Fixture session", url.lastPathComponent)
                XCTAssertEqual(envelope.session?.session?.providerSessionId, "codex-fixture-session", url.lastPathComponent)
                XCTAssertEqual(envelope.session?.session?.sourceFormat, "codex_session_jsonl", url.lastPathComponent)
                XCTAssertEqual(
                    envelope.session?.events.first?.structuredContent?.arrayValue?[1]["complete"],
                    .bool(true),
                    url.lastPathComponent
                )
            case .initialize, .sync, .error:
                break
            }
        }
    }

    private static let statusJSON = #"{"initialized":true,"local_only":true,"data_root":"/tmp/ctx-sdk-test","indexed_items":3,"indexed_sessions":1,"indexed_events":2,"indexed_sources":1,"lexical":{"status":"ready","generation_id":"gen-3"},"refresh":{"status":"ready","generation_id":"gen-3"}}"#
    private static let searchJSON = #"{"query":"local agent history","filters":{"provider":"codex"},"freshness":{"mode":"off","status":"skipped","source_count":0,"totals":{"imported_events":0}},"generated_at":"2026-07-01T12:00:00Z","retrieval":{"requested_mode":"hybrid","effective_mode":"lexical","semantic_weight":0.0,"semantic_fallback_code":"semantic_retrieval_failed","semantic_fallback":"semantic_retrieval_failed","coverage":{"embedded_items":4,"indexed_now":1},"diagnostics":{"query_embed_ms":2}},"results":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider_session_id":"codex-fixture-session","source_format":"codex_session_jsonl","event_seq":1,"title":"Fixture session","snippet":"local agent history search result","rank":1,"retrieval_score":0.98,"result_type":"event","result_scope":"event","provider":"codex","timestamp":"2026-07-01T12:00:00Z","cwd":"/workspace/ctx","why_matched":["text"],"citations":[{"target_type":"event","ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","label":"codex event","provider":"codex"}],"suggested_next_commands":["ctx show event 11111111-1111-4111-8111-111111111111 --window 10","ctx search 'local agent history' --session 22222222-2222-4222-8222-222222222222","ctx show session 22222222-2222-4222-8222-222222222222"],"visibility":"local_only"}],"result_window":{"limit":1,"returned":1,"more_available":true},"truncation":{"truncated":false}}"#
    private static let eventJSON = #"{"event":{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","source_format":"codex_session_jsonl","sequence":1,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","text":"local agent history search result","structured_content":{"kind":"toolResult","payload":{"items":["alpha",null,{"nested":[1,false]}]}},"content":{"complete":true,"policy_status":"selected"}},"events":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","source_format":"codex_session_jsonl","sequence":1,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","text":"local agent history search result","structured_content":null,"content":{"complete":true,"policy_status":"selected"}}]}"#
    private static let sessionJSON = #"{"session":{"ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","source_format":"codex_session_jsonl","title":"Fixture session"},"events":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","source_format":"codex_session_jsonl","sequence":1,"event_type":"message","role":"assistant","text":"local agent history search result","content":{"complete":true,"policy_status":"selected"}}],"mode":"lite","format":"json"}"#
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
