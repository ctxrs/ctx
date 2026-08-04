import Dispatch
import Foundation
import XCTest

@testable import CtxAgentHistory

#if os(macOS)
import Darwin
#endif

final class SwiftLocalCLIProcessTests: XCTestCase {
    func testFailsClosedOffMacOS() throws {
        #if os(macOS)
        throw XCTSkip("the contained local adapter is supported on macOS")
        #else
        XCTAssertThrowsError(
            try ProcessCommandRunner().run(
                CommandRequest(command: "ctx-should-not-spawn", arguments: ["status"])
            )
        ) { error in
            let sdkError = error as? CtxAgentHistorySDKError
            XCTAssertEqual(sdkError?.code, .notSupported)
            XCTAssertEqual(sdkError?.command, ["ctx-should-not-spawn", "status"])
            XCTAssertEqual(sdkError?.exitCode, -1)
        }
        #endif
    }

    func testContinuesDrainingAfterRetentionBounds() throws {
        #if os(macOS)
        let directory = try makeProcessFixtureDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let runner = ProcessCommandRunner(
            maxRetainedStdoutBytes: 4 * 1024,
            maxRetainedStderrBytes: 4 * 1024
        )

        let stderrBounded = try runner.run(
            processFixtureRequest(
                mode: "large-stderr",
                directory: directory,
                timeout: 2
            ))
        XCTAssertEqual(stderrBounded.stdout, Data("ok\n".utf8))
        XCTAssertEqual(stderrBounded.stderr.count, 4 * 1024)

        XCTAssertThrowsError(
            try runner.run(
                processFixtureRequest(
                    mode: "large-output",
                    directory: directory,
                    timeout: 2
                ))
        ) { error in
            let sdkError = error as? CtxAgentHistorySDKError
            XCTAssertEqual(sdkError?.code, .adapterError)
            XCTAssertEqual(sdkError?.details?["stream"], .string("stdout"))
            XCTAssertEqual(sdkError?.details?["limitBytes"], .number(4 * 1024))
        }
        #else
        throw XCTSkip("Darwin process-group execution is macOS-only")
        #endif
    }

    func testPreservesMaximumMCPAttributionOutput() throws {
        #if os(macOS)
        let directory = try makeProcessFixtureDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let output = try ProcessCommandRunner().run(
            processFixtureRequest(
                mode: "max-mcp-json",
                directory: directory,
                timeout: 2
            ))
        let runner = SwiftFixtureResultRunner(result: output)
        let selected = try AgentHistoryClient(adapter: LocalCLIAdapter(runner: runner))
            .showEvent("event-1")
            .event
            .event
        let call = try XCTUnwrap(selected?.mcpToolCall)
        XCTAssertEqual(call.server.utf8.count, AgentHistoryMCPToolCall.maximumComponentBytes)
        XCTAssertEqual(call.tool.utf8.count, AgentHistoryMCPToolCall.maximumComponentBytes)
        #else
        throw XCTSkip("Darwin process-group execution is macOS-only")
        #endif
    }

    func testTimesOutPersistentPipeDescendantAndKillsGroup() throws {
        #if os(macOS)
        let directory = try makeProcessFixtureDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let started = DispatchTime.now().uptimeNanoseconds

        XCTAssertThrowsError(
            try ProcessCommandRunner().run(
                processFixtureRequest(
                    mode: "persistent-descendant",
                    directory: directory,
                    timeout: 0.25
                ))
        ) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .timeout)
        }
        let elapsed = elapsedSeconds(since: started)
        XCTAssertGreaterThanOrEqual(elapsed, 0.20)
        XCTAssertLessThan(elapsed, 2)
        try assertProcessIsGone(pidFile: directory.appendingPathComponent("child.pid"))
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: directory.appendingPathComponent("alive").path
            ))
        #else
        throw XCTSkip("Darwin process-group execution is macOS-only")
        #endif
    }

    func testForceKillsIgnoredTermTreeAndReapsLeader() throws {
        #if os(macOS)
        let directory = try makeProcessFixtureDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let started = DispatchTime.now().uptimeNanoseconds

        XCTAssertThrowsError(
            try ProcessCommandRunner().run(
                processFixtureRequest(
                    mode: "ignored-term-tree",
                    directory: directory,
                    timeout: 0.25
                ))
        ) { error in
            XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .timeout)
        }
        let elapsed = elapsedSeconds(since: started)
        XCTAssertGreaterThanOrEqual(elapsed, 0.20)
        XCTAssertLessThan(elapsed, 2)
        try assertProcessIsGone(pidFile: directory.appendingPathComponent("root.pid"))
        try assertProcessIsGone(pidFile: directory.appendingPathComponent("child.pid"))
        XCTAssertFalse(
            FileManager.default.fileExists(
                atPath: directory.appendingPathComponent("alive").path
            ))
        #else
        throw XCTSkip("Darwin process-group execution is macOS-only")
        #endif
    }

    #if os(macOS)
    private var fixtureDirectory: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("Fixtures", isDirectory: true)
    }

    private func makeProcessFixtureDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ctx-swift-process-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    private func processFixtureRequest(
        mode: String,
        directory: URL,
        timeout: TimeInterval
    ) -> CommandRequest {
        CommandRequest(
            command:
                fixtureDirectory
                .appendingPathComponent("swift-local-cli-process-tree-fixture.sh")
                .path,
            arguments: [
                mode,
                directory.appendingPathComponent("root.pid").path,
                directory.appendingPathComponent("child.pid").path,
                directory.appendingPathComponent("alive").path
            ],
            timeout: timeout
        )
    }

    private func assertProcessIsGone(pidFile: URL) throws {
        let raw = try String(contentsOf: pidFile, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let pid = try XCTUnwrap(pid_t(raw))
        let deadline = DispatchTime.now().uptimeNanoseconds + 1_000_000_000
        while Darwin.kill(pid, 0) == 0,
            DispatchTime.now().uptimeNanoseconds < deadline
        {
            Thread.sleep(forTimeInterval: 0.01)
        }
        let result = Darwin.kill(pid, 0)
        let processError = errno
        XCTAssertEqual(result, -1, "process \(pid) survived process-group cleanup")
        XCTAssertEqual(processError, ESRCH, "process \(pid) still exists or cannot be inspected")
    }

    private func elapsedSeconds(since started: UInt64) -> Double {
        Double(DispatchTime.now().uptimeNanoseconds - started) / 1_000_000_000
    }
    #endif
}

private final class SwiftFixtureResultRunner: CommandRunner, @unchecked Sendable {
    let result: CommandResult

    init(result: CommandResult) {
        self.result = result
    }

    func run(_ request: CommandRequest) throws -> CommandResult {
        _ = request
        return result
    }
}
