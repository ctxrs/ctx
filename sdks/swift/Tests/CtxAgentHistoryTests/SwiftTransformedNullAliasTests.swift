import Foundation
import XCTest

@testable import CtxAgentHistory

final class SwiftTransformedNullAliasTests: XCTestCase {
    func testRejectsTransformedNullAliasesBeforeNullDropping() throws {
        for name in [
            "swift-transformed-null-mcp-alias-standalone.json",
            "swift-transformed-null-mcp-alias-with-canonical.json"
        ] {
            let data = try Data(contentsOf: fixtureDirectory.appendingPathComponent(name))
            let runner = SwiftAliasFixtureRunner(result: CommandResult(stdout: data))
            XCTAssertThrowsError(
                try AgentHistoryClient(adapter: LocalCLIAdapter(runner: runner))
                    .showEvent("event-1"),
                name
            ) { error in
                XCTAssertEqual((error as? CtxAgentHistorySDKError)?.code, .decodeError)
            }
        }
    }

    private var fixtureDirectory: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("Fixtures", isDirectory: true)
    }
}

private final class SwiftAliasFixtureRunner: CommandRunner, @unchecked Sendable {
    let result: CommandResult

    init(result: CommandResult) {
        self.result = result
    }

    func run(_ request: CommandRequest) throws -> CommandResult {
        _ = request
        return result
    }
}
