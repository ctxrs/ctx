import Foundation

public struct CommandRequest: Equatable, Sendable {
    public var command: String
    public var arguments: [String]
    public var cwd: String?
    public var env: [String: String]
    public var timeout: TimeInterval?

    public init(
        command: String,
        arguments: [String],
        cwd: String? = nil,
        env: [String: String] = [:],
        timeout: TimeInterval? = nil
    ) {
        self.command = command
        self.arguments = arguments
        self.cwd = cwd
        self.env = env
        self.timeout = timeout
    }
}

public struct CommandResult: Equatable, Sendable {
    public var stdout: Data
    public var stderr: Data
    public var exitCode: Int32

    public init(stdout: Data = Data(), stderr: Data = Data(), exitCode: Int32 = 0) {
        self.stdout = stdout
        self.stderr = stderr
        self.exitCode = exitCode
    }

    public init(stdout: String, stderr: String = "", exitCode: Int32 = 0) {
        self.stdout = Data(stdout.utf8)
        self.stderr = Data(stderr.utf8)
        self.exitCode = exitCode
    }
}

public protocol CommandRunner: Sendable {
    func run(_ request: CommandRequest) throws -> CommandResult
}

public struct ProcessCommandRunner: CommandRunner {
    public init() {}

    public func run(_ request: CommandRequest) throws -> CommandResult {
        let process = Process()
        if request.command.contains("/") {
            process.executableURL = URL(fileURLWithPath: request.command)
            process.arguments = request.arguments
        } else {
            process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            process.arguments = [request.command] + request.arguments
        }
        if let cwd = request.cwd {
            process.currentDirectoryURL = URL(fileURLWithPath: cwd)
        }
        process.environment = ProcessInfo.processInfo.environment.merging(request.env) { _, new in new }

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        do {
            try process.run()
        } catch {
            throw CtxAgentHistorySDKError(
                code: .backendUnavailable,
                message: "failed to execute ctx CLI",
                details: .object(["command": .array(([request.command] + request.arguments).map { .string($0) })]),
                cause: String(describing: error),
                command: [request.command] + request.arguments,
                exitCode: -1
            )
        }

        let stdoutData = LockedData()
        let stderrData = LockedData()
        let pipeReaders = DispatchGroup()
        pipeReaders.enter()
        DispatchQueue.global(qos: .utility).async {
            stdoutData.store(stdout.fileHandleForReading.readDataToEndOfFile())
            pipeReaders.leave()
        }
        pipeReaders.enter()
        DispatchQueue.global(qos: .utility).async {
            stderrData.store(stderr.fileHandleForReading.readDataToEndOfFile())
            pipeReaders.leave()
        }

        if let timeout = request.timeout {
            let deadline = Date().addingTimeInterval(timeout)
            while process.isRunning && Date() < deadline {
                Thread.sleep(forTimeInterval: 0.01)
            }
            if process.isRunning {
                process.terminate()
                process.waitUntilExit()
                pipeReaders.wait()
                throw CtxAgentHistorySDKError(
                    code: .timeout,
                    message: "ctx CLI timed out",
                    retryable: true,
                    command: [request.command] + request.arguments,
                    exitCode: -1
                )
            }
        }

        process.waitUntilExit()
        pipeReaders.wait()
        return CommandResult(
            stdout: stdoutData.load(),
            stderr: stderrData.load(),
            exitCode: process.terminationStatus
        )
    }
}

private final class LockedData: @unchecked Sendable {
    private let lock = NSLock()
    private var data = Data()

    func store(_ newValue: Data) {
        lock.lock()
        data = newValue
        lock.unlock()
    }

    func load() -> Data {
        lock.lock()
        let value = data
        lock.unlock()
        return value
    }
}

public struct LocalCLIAdapter: Sendable {
    public var ctxPath: String
    public var dataRoot: String?
    public var cwd: String?
    public var env: [String: String]
    public var timeout: TimeInterval?
    public var runner: any CommandRunner

    public init(
        ctxPath: String = "ctx",
        dataRoot: String? = nil,
        cwd: String? = nil,
        env: [String: String] = [:],
        timeout: TimeInterval? = 60,
        runner: any CommandRunner = ProcessCommandRunner()
    ) {
        self.ctxPath = ctxPath
        self.dataRoot = dataRoot
        self.cwd = cwd
        self.env = env
        self.timeout = timeout
        self.runner = runner
    }

    public var backend: AgentHistoryBackend {
        AgentHistoryBackend(kind: "local", dataRoot: dataRoot)
    }

    public func execute(_ arguments: [String]) throws -> Data {
        guard !ctxPath.isEmpty else {
            throw CtxAgentHistorySDKError(code: .invalidRequest, message: "local ctx CLI path is empty")
        }
        let finalArguments = argv(arguments)
        let result = try runner.run(
            CommandRequest(
                command: ctxPath,
                arguments: finalArguments,
                cwd: cwd,
                env: env,
                timeout: timeout
            )
        )
        if result.exitCode != 0 {
            throw commandError(result: result, arguments: finalArguments)
        }
        let trimmed = result.stdout.trimmingASCIIWhitespace()
        guard !trimmed.isEmpty else {
            throw CtxAgentHistorySDKError(
                code: .decodeError,
                message: "ctx command returned empty stdout",
                details: .object(["command": .array(([ctxPath] + finalArguments).map { .string($0) })]),
                command: [ctxPath] + finalArguments,
                exitCode: Int(result.exitCode),
                stdout: String(data: result.stdout, encoding: .utf8),
                stderr: String(data: result.stderr, encoding: .utf8)
            )
        }
        return trimmed
    }

    public func versionString() throws -> String {
        let result = try runner.run(
            CommandRequest(command: ctxPath, arguments: ["--version"], cwd: cwd, env: env, timeout: timeout)
        )
        if result.exitCode != 0 {
            throw commandError(result: result, arguments: ["--version"])
        }
        return String(data: result.stdout, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }

    public func argv(_ arguments: [String]) -> [String] {
        var result: [String] = []
        if let dataRoot {
            result.append(contentsOf: ["--data-root", dataRoot])
        }
        result.append(contentsOf: arguments)
        return result
    }

    private func commandError(result: CommandResult, arguments: [String]) -> CtxAgentHistorySDKError {
        let stdout = String(data: result.stdout, encoding: .utf8) ?? ""
        let stderr = String(data: result.stderr, encoding: .utf8) ?? ""
        let firstStderrLine = stderr.split(whereSeparator: \.isNewline).first.map(String.init)
        return CtxAgentHistorySDKError(
            code: .adapterError,
            message: firstStderrLine.map { "ctx command failed: \($0)" } ?? "ctx command failed",
            details: .object([
                "command": .array(([ctxPath] + arguments).map { .string($0) }),
                "exitCode": .number(Double(result.exitCode)),
                "stdout": .string(stdout),
                "stderr": .string(stderr)
            ]),
            command: [ctxPath] + arguments,
            exitCode: Int(result.exitCode),
            stdout: stdout,
            stderr: stderr
        )
    }
}

private extension Data {
    func trimmingASCIIWhitespace() -> Data {
        var start = startIndex
        var end = endIndex
        while start < end, self[start].isASCIIWhitespace {
            formIndex(after: &start)
        }
        while end > start {
            let previous = index(before: end)
            if !self[previous].isASCIIWhitespace {
                break
            }
            end = previous
        }
        return self[start..<end]
    }
}

private extension UInt8 {
    var isASCIIWhitespace: Bool {
        self == 0x20 || self == 0x0a || self == 0x0d || self == 0x09
    }
}
