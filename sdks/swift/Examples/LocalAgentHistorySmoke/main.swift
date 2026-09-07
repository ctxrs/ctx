import CtxAgentHistory
import Foundation

@main
struct LocalAgentHistorySmoke {
    static func main() throws {
        let config = SmokeConfig(arguments: Array(CommandLine.arguments.dropFirst()))
        let client = config.makeClient()

        let status = try client.status()
        let initialized = try client.initialize(InitOptions())
        let imported = try client.importHistory(ImportOptions(provider: "codex", resume: true))
        let synced = try client.sync(ImportOptions(all: true))
        let search = try client.search(
            "local agent history",
            options: SearchOptions(limit: 1, provider: "codex", refresh: "off")
        )

        guard let hit = search.search.results.first,
              let eventID = hit.ctxEventId,
              let sessionID = hit.ctxSessionId
        else {
            throw SmokeError("search returned no event hit to show")
        }

        let event = try client.showEvent(eventID, options: ShowEventOptions(window: 1))
        let session = try client.showSession(sessionID)

        print("mode=\(config.useRealCtx ? "real" : "fake")")
        print("status.initialized=\(status.status.initialized)")
        print("init.initialized=\(initialized.status.initialized)")
        print("import.sessions=\(imported.importResult.totals.importedSessions ?? 0)")
        print("sync.events=\(synced.importResult.totals.importedEvents ?? 0)")
        print("search.results=\(search.search.results.count)")
        print("search.window=\(search.search.resultWindow?.returned ?? 0)/\(search.search.resultWindow?.limit ?? 0) more=\(search.search.resultWindow?.moreAvailable == true)")
        print("showEvent.event=\(event.event.event?.ctxEventId ?? "missing")")
        print("showSession.session=\(session.session.session?.ctxSessionId ?? "missing")")
        print("showEvent.providerSession=\(event.event.event?.providerSessionId ?? "missing")")
        print("showSession.providerSession=\(session.session.session?.providerSessionId ?? "missing")")
    }
}

private struct SmokeConfig {
    var arguments: [String]

    var useRealCtx: Bool {
        arguments.contains("--real") || ProcessInfo.processInfo.environment["CTX_AGENT_HISTORY_SMOKE_REAL"] == "1"
    }

    func makeClient() -> AgentHistoryClient {
        if useRealCtx {
            let env = ProcessInfo.processInfo.environment
            return .local(
                ctxPath: value(after: "--ctx-path") ?? env["CTX_PATH"] ?? "ctx",
                dataRoot: value(after: "--data-root") ?? env["CTX_DATA_ROOT"],
                cwd: value(after: "--cwd"),
                env: ["CTX_LOG": env["CTX_LOG"] ?? "warn"],
                timeout: 30
            )
        }

        return AgentHistoryClient(
            adapter: LocalCLIAdapter(
                dataRoot: "/tmp/ctx-swift-local-agent-history-smoke",
                runner: FakeSmokeRunner()
            )
        )
    }

    private func value(after flag: String) -> String? {
        guard let index = arguments.firstIndex(of: flag) else {
            return nil
        }
        let valueIndex = arguments.index(after: index)
        guard valueIndex < arguments.endIndex else {
            return nil
        }
        return arguments[valueIndex]
    }
}

private struct SmokeError: Error, CustomStringConvertible {
    var description: String

    init(_ description: String) {
        self.description = description
    }
}

private final class FakeSmokeRunner: CommandRunner, @unchecked Sendable {
    func run(_ request: CommandRequest) throws -> CommandResult {
        let arguments = request.arguments.filter { $0 != "--data-root" && $0 != "/tmp/ctx-swift-local-agent-history-smoke" }
        switch Array(arguments.prefix(2)) {
        case ["status", "--format=json"]:
            return CommandResult(stdout: #"{"initialized":true,"local_only":true,"data_root":"/tmp/ctx-swift-local-agent-history-smoke","indexed_items":3,"indexed_sessions":1,"indexed_events":2,"indexed_sources":1,"lexical":{"status":"ready","generation_id":"gen-3"},"refresh":{"status":"ready","generation_id":"gen-3"}}"#)
        case ["setup", "--format=json"]:
            return CommandResult(stdout: #"{"schema_version":1,"data_root":"/tmp/ctx-swift-local-agent-history-smoke","indexed_items":3,"lexical":{"status":"ready","generation_id":"gen-3"},"refresh":{"status":"ready","generation_id":"gen-3"},"network_required":false}"#)
        case ["import", "--format=json"]:
            return CommandResult(stdout: #"{"resume":true,"totals":{"imported_sources":1,"imported_sessions":1,"imported_events":1},"sources":[{"provider":"codex","path":"/tmp/ctx-sdk-fixture/session.jsonl","status":"imported","imported_sessions":1,"imported_events":1}]}"#)
        case ["search", "local agent history"]:
            return CommandResult(stdout: #"{"query":"local agent history","filters":{"provider":"codex"},"freshness":{"mode":"off","status":"skipped","source_count":0,"totals":{"imported_events":0}},"generated_at":"2026-07-01T12:00:00Z","results":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider_session_id":"codex-fixture-session","event_seq":1,"title":"Fixture session","snippet":"local agent history search result","rank":1,"retrieval_score":0.98,"result_type":"event","result_scope":"event","provider":"codex","timestamp":"2026-07-01T12:00:00Z","cwd":"/workspace/ctx","why_matched":["text"],"citations":[{"target_type":"event","ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","label":"codex event","provider":"codex"}],"suggested_next_commands":["ctx show event 11111111-1111-4111-8111-111111111111 --window 10","ctx search 'local agent history' --session 22222222-2222-4222-8222-222222222222","ctx show session 22222222-2222-4222-8222-222222222222"],"visibility":"local_only"}],"result_window":{"limit":1,"returned":1,"more_available":true},"truncation":{"truncated":false}}"#)
        case ["show", "event"]:
            return CommandResult(stdout: #"{"event":{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","source_format":"codex_session_jsonl","sequence":1,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","text":"local agent history search result"},"events":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","source_format":"codex_session_jsonl","sequence":1,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","text":"local agent history search result"}]}"#)
        case ["show", "session"]:
            return CommandResult(stdout: #"{"session":{"ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","title":"Fixture session"},"events":[{"ctx_event_id":"11111111-1111-4111-8111-111111111111","ctx_session_id":"22222222-2222-4222-8222-222222222222","provider":"codex","provider_session_id":"codex-fixture-session","sequence":1,"event_type":"message","role":"assistant","text":"local agent history search result"}],"mode":"lite","format":"json"}"#)
        default:
            return CommandResult(stdout: "", stderr: "unexpected fake ctx command: \(arguments.joined(separator: " "))\n", exitCode: 2)
        }
    }
}
