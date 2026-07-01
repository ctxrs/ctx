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
        let runner = CapturingRunner { _ in CommandResult(stdout: #"{"results":[]}"#) }
        let client = AgentHistoryClient(
            adapter: LocalCLIAdapter(dataRoot: "/tmp/ctx-sdk-test", runner: runner)
        )

        _ = try client.search(
            "retry handling",
            options: SearchOptions(
                terms: ["timeout", "backoff"],
                limit: 5,
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
                "--json"
            ]
        )
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
            case ["search", "local agent history"]:
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

        let search = try client.search("local agent history", options: SearchOptions(limit: 1, refresh: "off"))
        XCTAssertEqual(search.search.query, "local agent history")
        XCTAssertEqual(search.search.results.first?.resultScope, "event")
        XCTAssertEqual(search.search.results.first?.citations.first?.label, "codex event")

        let event = try client.showEvent("11111111-1111-4111-8111-111111111111")
        XCTAssertEqual(event.event.event?.redactionState, "redacted")
        XCTAssertEqual(event.event.source?.sourceFormat, "codex_session_jsonl")

        let session = try client.showSession("22222222-2222-4222-8222-222222222222")
        XCTAssertEqual(session.session.session?.providerSessionId, "codex-fixture-session")
        XCTAssertEqual(session.session.events.first?.text, "local agent history search result")

        let location = try client.locateEvent("11111111-1111-4111-8111-111111111111")
        XCTAssertEqual(location.location.provider, "codex")
        XCTAssertEqual(location.location.resume?.cursor, "line:2")
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
    private static let searchJSON = #"{"query":"local agent history","filters":{"provider":"codex"},"freshness":{"mode":"off","status":"skipped","source_count":0,"totals":{"imported_events":0}},"generated_at":"2026-07-01T12:00:00Z","results":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider_session_id":"codex-fixture-session","event_seq":1,"title":"Fixture session","snippet":"local agent history search result","rank":0.98,"result_scope":"event","provider":"codex","timestamp":"2026-07-01T12:00:00Z","cwd":"/workspace/ctx","source_path":"/tmp/ctx-sdk-fixture/session.jsonl","source_exists":true,"cursor":"line:2","why_matched":["text"],"citations":[{"item_type":"event","ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","label":"codex event","provider":"codex","source_path":"/tmp/ctx-sdk-fixture/session.jsonl","source_exists":true,"cursor":"line:2"}],"suggested_next_commands":["ctx show event 11111111-1111-4111-8111-111111111111 --format json"],"visibility":"private"}],"pagination":{"limit":20},"truncation":{"truncated":false}}"#
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
