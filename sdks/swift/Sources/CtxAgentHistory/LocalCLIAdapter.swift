import Foundation
import Darwin

private let localStdoutCapBytes = 2 * 1024 * 1024
private let localStderrCapBytes = 256 * 1024
private let localReadBufferBytes = 64 * 1024
private let localDrainGrace: TimeInterval = 0.1
private let localTeardownLimit: TimeInterval = 1

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
        let processScope = OwnedProcessScope(process: process)
        defer { processScope.terminate() }

        let stdoutData = LockedCapture(stream: "stdout", capBytes: localStdoutCapBytes)
        let stderrData = LockedCapture(stream: "stderr", capBytes: localStderrCapBytes)
        let pipeReaders = DispatchGroup()
        pipeReaders.enter()
        DispatchQueue.global(qos: .utility).async {
            stdoutData.read(from: stdout.fileHandleForReading)
            pipeReaders.leave()
        }
        pipeReaders.enter()
        DispatchQueue.global(qos: .utility).async {
            stderrData.read(from: stderr.fileHandleForReading)
            pipeReaders.leave()
        }

        let deadline = request.timeout.map { Date().addingTimeInterval(max(0, $0)) }
        while process.isRunning {
            if let issue = stdoutData.issue() ?? stderrData.issue() {
                abort(processScope, stdout, stderr, pipeReaders)
                throw issue.sdkError(command: [request.command] + request.arguments)
            }
            if let deadline, Date() >= deadline {
                abort(processScope, stdout, stderr, pipeReaders)
                throw CtxAgentHistorySDKError(
                    code: .timeout,
                    message: "ctx CLI timed out",
                    retryable: true,
                    command: [request.command] + request.arguments,
                    exitCode: -1
                )
            }
            Thread.sleep(forTimeInterval: 0.005)
        }

        if pipeReaders.wait(timeout: .now() + localDrainGrace) == .timedOut {
            abort(processScope, stdout, stderr, pipeReaders)
            throw CaptureIssue.failure(stream: "pipe", cause: "a descendant retained a CLI output pipe")
                .sdkError(command: [request.command] + request.arguments)
        }
        if let issue = stdoutData.issue() ?? stderrData.issue() {
            throw issue.sdkError(command: [request.command] + request.arguments)
        }
        return CommandResult(
            stdout: stdoutData.data(),
            stderr: stderrData.data(),
            exitCode: process.terminationStatus
        )
    }

    private func abort(
        _ scope: OwnedProcessScope,
        _ stdout: Pipe,
        _ stderr: Pipe,
        _ readers: DispatchGroup
    ) {
        scope.terminate()
        try? stdout.fileHandleForReading.close()
        try? stderr.fileHandleForReading.close()
        _ = readers.wait(timeout: .now() + localTeardownLimit)
    }
}

private enum CaptureIssue {
    case limit(stream: String, capBytes: Int)
    case failure(stream: String, cause: String)

    func sdkError(command: [String]) -> CtxAgentHistorySDKError {
        switch self {
        case let .limit(stream, capBytes):
            return CtxAgentHistorySDKError(
                code: .captureLimit,
                message: "ctx CLI \(stream) exceeded its capture limit",
                details: .object([
                    "stream": .string(stream),
                    "capBytes": .number(Double(capBytes))
                ]),
                command: command,
                exitCode: -1
            )
        case let .failure(stream, cause):
            return CtxAgentHistorySDKError(
                code: .captureFailure,
                message: "ctx CLI output capture failed",
                details: .object(["stream": .string(stream)]),
                cause: cause,
                command: command,
                exitCode: -1
            )
        }
    }
}

private final class LockedCapture: @unchecked Sendable {
    private let lock = NSLock()
    private let stream: String
    private let capBytes: Int
    private var captured = Data()
    private var captureIssue: CaptureIssue?

    init(stream: String, capBytes: Int) {
        self.stream = stream
        self.capBytes = capBytes
    }

    func read(from handle: FileHandle) {
        do {
            while let chunk = try handle.read(upToCount: localReadBufferBytes), !chunk.isEmpty {
                lock.lock()
                let remaining = capBytes - captured.count
                if chunk.count > remaining {
                    if remaining > 0 {
                        captured.append(contentsOf: chunk.prefix(remaining))
                    }
                    captureIssue = .limit(stream: stream, capBytes: capBytes)
                    lock.unlock()
                    return
                }
                captured.append(chunk)
                lock.unlock()
            }
        } catch {
            lock.lock()
            if captureIssue == nil {
                captureIssue = .failure(stream: stream, cause: String(describing: error))
            }
            lock.unlock()
        }
    }

    func data() -> Data {
        lock.lock()
        let value = captured
        lock.unlock()
        return value
    }

    func issue() -> CaptureIssue? {
        lock.lock()
        let value = captureIssue
        lock.unlock()
        return value
    }
}

private final class OwnedProcessScope {
    private let process: Process
    private let processIdentifier: pid_t
    private let ownsProcessGroup: Bool
    private let lock = NSLock()
    private var terminated = false

    init(process: Process) {
        self.process = process
        processIdentifier = process.processIdentifier
        ownsProcessGroup = setpgid(processIdentifier, processIdentifier) == 0
    }

    func terminate() {
        lock.lock()
        if terminated {
            lock.unlock()
            return
        }
        terminated = true
        lock.unlock()

        let signaledGroup = ownsProcessGroup && kill(-processIdentifier, SIGTERM) == 0
        if process.isRunning {
            process.terminate()
        }
        if signaledGroup {
            Thread.sleep(forTimeInterval: localDrainGrace)
            _ = kill(-processIdentifier, SIGKILL)
        }
        if process.isRunning {
            _ = kill(processIdentifier, SIGKILL)
        }
        let deadline = Date().addingTimeInterval(localTeardownLimit)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.005)
        }
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
        let result = try validated(
            runner.run(
                CommandRequest(
                    command: ctxPath,
                    arguments: finalArguments,
                    cwd: cwd,
                    env: env,
                    timeout: timeout
                )
            ),
            arguments: finalArguments
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
        let result = try validated(
            runner.run(CommandRequest(command: ctxPath, arguments: ["--version"], cwd: cwd, env: env, timeout: timeout)),
            arguments: ["--version"]
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

    private func validated(_ result: CommandResult, arguments: [String]) throws -> CommandResult {
        for (stream, data, capBytes) in [
            ("stdout", result.stdout, localStdoutCapBytes),
            ("stderr", result.stderr, localStderrCapBytes)
        ] {
            if data.count > capBytes {
                throw CaptureIssue.limit(stream: stream, capBytes: capBytes)
                    .sdkError(command: [ctxPath] + arguments)
            }
            if String(data: data, encoding: .utf8) == nil {
                throw CaptureIssue.failure(stream: stream, cause: "output was not valid UTF-8")
                    .sdkError(command: [ctxPath] + arguments)
            }
        }
        return result
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
