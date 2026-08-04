#if os(macOS)
import Darwin
import Dispatch
import Foundation

private let commandPollIntervalMilliseconds: Int32 = 10
private let processGroupTerminationGraceNanoseconds: UInt64 = 100_000_000

struct DarwinCommandExecution {
    let maxRetainedStdoutBytes: Int
    let maxRetainedStderrBytes: Int

    func run(_ request: CommandRequest) throws -> CommandResult {
        let command = [request.command] + request.arguments
        let deadline = MonotonicDeadline(timeout: request.timeout)
        let environment = ProcessInfo.processInfo.environment.merging(request.env) { _, new in new }
        let launch = try spawn(request, environment: environment, command: command)
        var child = OwnedChildProcess(pid: launch.pid)
        var stdout = BoundedPipeDrain(
            fileDescriptor: launch.stdout,
            stream: "stdout",
            retentionLimit: maxRetainedStdoutBytes
        )
        var stderr = BoundedPipeDrain(
            fileDescriptor: launch.stderr,
            stream: "stderr",
            retentionLimit: maxRetainedStderrBytes
        )

        do {
            try child.resume()
            while true {
                if deadline.hasExpired {
                    throw timeoutError(command: command)
                }
                try stdout.drainAvailable()
                try stderr.drainAvailable()

                if deadline.hasExpired {
                    throw timeoutError(command: command)
                }
                if child.hasExited, stdout.reachedEOF, stderr.reachedEOF {
                    break
                }

                try waitForOutput(
                    stdout: stdout.fileDescriptor,
                    stderr: stderr.fileDescriptor,
                    timeoutMilliseconds: deadline.nextPollMilliseconds
                )
            }
        } catch let error as CtxAgentHistorySDKError {
            child.terminateProcessGroupAndReap()
            stdout.close()
            stderr.close()
            throw error
        } catch {
            child.terminateProcessGroupAndReap()
            stdout.close()
            stderr.close()
            throw CtxAgentHistorySDKError(
                code: .backendUnavailable,
                message: "failed to collect ctx CLI output",
                cause: String(describing: error),
                command: command,
                exitCode: -1
            )
        }

        let exitCode = child.terminateProcessGroupAndReap()
        if stdout.exceededRetentionLimit {
            throw retentionError(
                stream: "stdout",
                limit: maxRetainedStdoutBytes,
                command: command,
                exitCode: exitCode
            )
        }
        return CommandResult(
            stdout: stdout.retained,
            stderr: stderr.retained,
            exitCode: exitCode
        )
    }

    private func spawn(
        _ request: CommandRequest,
        environment: [String: String],
        command: [String]
    ) throws -> SpawnedCommand {
        do {
            return try spawnWithPipes(request, environment: environment)
        } catch let error as CtxAgentHistorySDKError {
            throw error
        } catch {
            throw CtxAgentHistorySDKError(
                code: .backendUnavailable,
                message: "failed to execute ctx CLI",
                details: .object(["command": .array(command.map { .string($0) })]),
                cause: String(describing: error),
                command: command,
                exitCode: -1
            )
        }
    }

    private func spawnWithPipes(
        _ request: CommandRequest,
        environment: [String: String]
    ) throws -> SpawnedCommand {
        var stdoutPipe = try PipeDescriptors()
        do {
            var stderrPipe = try PipeDescriptors()
            do {
                try stdoutPipe.makeReadEndNonblocking()
                try stderrPipe.makeReadEndNonblocking()
                let spawned = try spawn(
                    request,
                    environment: environment,
                    stdoutPipe: stdoutPipe,
                    stderrPipe: stderrPipe
                )
                stdoutPipe.closeWriteEnd()
                stderrPipe.closeWriteEnd()
                let result = SpawnedCommand(
                    pid: spawned,
                    stdout: stdoutPipe.takeReadEnd(),
                    stderr: stderrPipe.takeReadEnd()
                )
                stdoutPipe.close()
                stderrPipe.close()
                return result
            } catch {
                stderrPipe.close()
                throw error
            }
        } catch {
            stdoutPipe.close()
            throw error
        }
    }

    private func spawn(
        _ request: CommandRequest,
        environment: [String: String],
        stdoutPipe: PipeDescriptors,
        stderrPipe: PipeDescriptors
    ) throws -> pid_t {
        let executable: String
        let arguments: [String]
        if request.command.contains("/") {
            executable = request.command
            arguments = [request.command] + request.arguments
        } else {
            executable = "/usr/bin/env"
            arguments = ["env", request.command] + request.arguments
        }
        let environmentEntries = environment.keys.sorted().map { "\($0)=\(environment[$0]!)" }
        try validateCStringValues([executable] + arguments + environmentEntries)

        var fileActions: posix_spawn_file_actions_t? = nil
        var attributes: posix_spawnattr_t? = nil
        var result = posix_spawn_file_actions_init(&fileActions)
        guard result == 0 else {
            throw POSIXFailure(operation: "posix_spawn_file_actions_init", code: result)
        }
        defer { posix_spawn_file_actions_destroy(&fileActions) }

        result = posix_spawnattr_init(&attributes)
        guard result == 0 else {
            throw POSIXFailure(operation: "posix_spawnattr_init", code: result)
        }
        defer { posix_spawnattr_destroy(&attributes) }

        for (source, destination) in [
            (stdoutPipe.writeEnd, STDOUT_FILENO),
            (stderrPipe.writeEnd, STDERR_FILENO)
        ] {
            result = posix_spawn_file_actions_adddup2(&fileActions, source, destination)
            guard result == 0 else {
                throw POSIXFailure(operation: "posix_spawn_file_actions_adddup2", code: result)
            }
        }
        for descriptor in [
            stdoutPipe.readEnd,
            stdoutPipe.writeEnd,
            stderrPipe.readEnd,
            stderrPipe.writeEnd
        ] {
            result = posix_spawn_file_actions_addclose(&fileActions, descriptor)
            guard result == 0 else {
                throw POSIXFailure(operation: "posix_spawn_file_actions_addclose", code: result)
            }
        }
        if let cwd = request.cwd {
            result = cwd.withCString {
                posix_spawn_file_actions_addchdir_np(&fileActions, $0)
            }
            guard result == 0 else {
                throw POSIXFailure(operation: "posix_spawn_file_actions_addchdir_np", code: result)
            }
        }

        var emptySignalMask = sigset_t()
        guard sigemptyset(&emptySignalMask) == 0 else {
            throw POSIXFailure(operation: "sigemptyset", code: errno)
        }
        result = posix_spawnattr_setsigmask(&attributes, &emptySignalMask)
        guard result == 0 else {
            throw POSIXFailure(operation: "posix_spawnattr_setsigmask", code: result)
        }
        // Suspend the new group until its exit source owns the leader PID.
        let flags =
            POSIX_SPAWN_SETPGROUP
            | POSIX_SPAWN_SETSIGMASK
            | POSIX_SPAWN_START_SUSPENDED
            | POSIX_SPAWN_CLOEXEC_DEFAULT
        result = posix_spawnattr_setflags(&attributes, Int16(flags))
        guard result == 0 else {
            throw POSIXFailure(operation: "posix_spawnattr_setflags", code: result)
        }
        result = posix_spawnattr_setpgroup(&attributes, 0)
        guard result == 0 else {
            throw POSIXFailure(operation: "posix_spawnattr_setpgroup", code: result)
        }

        var pid: pid_t = 0
        result = try withMutableCStringArray(arguments) { argv in
            try withMutableCStringArray(environmentEntries) { envp in
                executable.withCString {
                    posix_spawn(&pid, $0, &fileActions, &attributes, argv, envp)
                }
            }
        }
        guard result == 0 else {
            throw POSIXFailure(operation: "posix_spawn", code: result)
        }
        return pid
    }

    private func waitForOutput(
        stdout: Int32?,
        stderr: Int32?,
        timeoutMilliseconds: Int32
    ) throws {
        var descriptors = [stdout, stderr].compactMap { descriptor -> pollfd? in
            guard let descriptor else { return nil }
            return pollfd(fd: descriptor, events: Int16(POLLIN | POLLHUP | POLLERR), revents: 0)
        }
        let result = descriptors.withUnsafeMutableBufferPointer { buffer in
            Darwin.poll(buffer.baseAddress, nfds_t(buffer.count), timeoutMilliseconds)
        }
        if result < 0, errno != EINTR {
            throw POSIXFailure(operation: "poll", code: errno)
        }
    }

    private func timeoutError(command: [String]) -> CtxAgentHistorySDKError {
        CtxAgentHistorySDKError(
            code: .timeout,
            message: "ctx CLI timed out",
            retryable: true,
            command: command,
            exitCode: -1
        )
    }

    private func retentionError(
        stream: String,
        limit: Int,
        command: [String],
        exitCode: Int32
    ) -> CtxAgentHistorySDKError {
        CtxAgentHistorySDKError(
            code: .adapterError,
            message: "ctx CLI \(stream) exceeded its retention limit",
            details: .object([
                "stream": .string(stream),
                "limitBytes": .number(Decimal(limit))
            ]),
            command: command,
            exitCode: Int(exitCode)
        )
    }
}

private struct SpawnedCommand {
    let pid: pid_t
    let stdout: Int32
    let stderr: Int32
}

private struct PipeDescriptors {
    private(set) var readEnd: Int32
    private(set) var writeEnd: Int32

    init() throws {
        var descriptors = [Int32](repeating: -1, count: 2)
        guard Darwin.pipe(&descriptors) == 0 else {
            throw POSIXFailure(operation: "pipe", code: errno)
        }
        guard descriptors.allSatisfy({ $0 > STDERR_FILENO }) else {
            for descriptor in descriptors where descriptor >= 0 {
                _ = Darwin.close(descriptor)
            }
            throw POSIXFailure(operation: "pipe descriptor allocation", code: EINVAL)
        }
        readEnd = descriptors[0]
        writeEnd = descriptors[1]
    }

    mutating func makeReadEndNonblocking() throws {
        let flags = fcntl(readEnd, F_GETFL)
        guard flags >= 0, fcntl(readEnd, F_SETFL, flags | O_NONBLOCK) == 0 else {
            throw POSIXFailure(operation: "fcntl", code: errno)
        }
    }

    mutating func takeReadEnd() -> Int32 {
        let descriptor = readEnd
        readEnd = -1
        return descriptor
    }

    mutating func closeWriteEnd() {
        closeDescriptor(&writeEnd)
    }

    mutating func close() {
        closeDescriptor(&readEnd)
        closeDescriptor(&writeEnd)
    }
}

private struct BoundedPipeDrain {
    private(set) var fileDescriptor: Int32?
    let stream: String
    let retentionLimit: Int
    private(set) var retained = Data()
    private(set) var exceededRetentionLimit = false

    var reachedEOF: Bool { fileDescriptor == nil }

    init(fileDescriptor: Int32, stream: String, retentionLimit: Int) {
        self.fileDescriptor = fileDescriptor
        self.stream = stream
        self.retentionLimit = retentionLimit
        retained.reserveCapacity(min(retentionLimit, 64 * 1024))
    }

    mutating func drainAvailable() throws {
        guard let descriptor = fileDescriptor else { return }
        var buffer = [UInt8](repeating: 0, count: 16 * 1024)
        // A hot writer must yield so the outer loop can enforce its deadline.
        for _ in 0..<16 {
            let count = buffer.withUnsafeMutableBytes { bytes in
                Darwin.read(descriptor, bytes.baseAddress, bytes.count)
            }
            if count > 0 {
                let remaining = max(0, retentionLimit - retained.count)
                if count > remaining {
                    if remaining > 0 {
                        retained.append(contentsOf: buffer.prefix(remaining))
                    }
                    exceededRetentionLimit = true
                } else {
                    retained.append(contentsOf: buffer.prefix(count))
                }
                continue
            }
            if count == 0 {
                close()
                return
            }
            if errno == EINTR {
                continue
            }
            if errno == EAGAIN || errno == EWOULDBLOCK {
                return
            }
            throw POSIXFailure(operation: "read \(stream)", code: errno)
        }
    }

    mutating func close() {
        guard var descriptor = fileDescriptor else { return }
        closeDescriptor(&descriptor)
        fileDescriptor = nil
    }
}

private struct OwnedChildProcess {
    let pid: pid_t
    private let exitObservation: ProcessExitObservation
    private let exitSource: DispatchSourceProcess
    private var hasResumed = false
    private(set) var waitStatus: Int32?

    init(pid: pid_t) {
        let exitObservation = ProcessExitObservation()
        let exitSource = DispatchSource.makeProcessSource(
            identifier: pid,
            eventMask: .exit,
            queue: .global(qos: .utility)
        )
        self.pid = pid
        self.exitObservation = exitObservation
        self.exitSource = exitSource
        exitSource.setEventHandler {
            exitObservation.observeExit()
        }
        exitSource.resume()
    }

    var hasExited: Bool { exitObservation.hasExited }

    mutating func resume() throws {
        guard !hasResumed else { return }
        guard Darwin.kill(-pid, SIGCONT) == 0 else {
            throw POSIXFailure(operation: "resume process group", code: errno)
        }
        hasResumed = true
    }

    @discardableResult
    mutating func terminateProcessGroupAndReap() -> Int32 {
        if let waitStatus {
            return decodeWaitStatus(waitStatus)
        }
        // The exit source observes without reaping, so the leader pins the PGID
        // until every remaining group member has been signalled.
        signalProcessGroup(SIGTERM)
        signalProcessGroup(SIGCONT)
        let graceDeadline = DispatchTime.now().uptimeNanoseconds
            .addingReportingOverflow(processGroupTerminationGraceNanoseconds)
        while !graceDeadline.overflow,
            DispatchTime.now().uptimeNanoseconds < graceDeadline.partialValue
        {
            if hasExited, !processGroupHasDescendants {
                break
            }
            Thread.sleep(forTimeInterval: 0.005)
        }
        signalProcessGroup(SIGKILL)
        reapBlocking()
        exitSource.cancel()
        return waitStatus.map(decodeWaitStatus) ?? -1
    }

    private mutating func reapBlocking() {
        guard waitStatus == nil else { return }
        while true {
            var status: Int32 = 0
            let result = Darwin.waitpid(pid, &status, 0)
            if result == pid {
                waitStatus = status
                return
            }
            if result < 0, errno == EINTR {
                continue
            }
            return
        }
    }

    private var processGroupHasDescendants: Bool {
        let requiredBytes = proc_listpids(
            UInt32(PROC_PGRP_ONLY),
            UInt32(bitPattern: pid),
            nil,
            0
        )
        guard requiredBytes > 0 else { return false }
        let stride = MemoryLayout<pid_t>.stride
        var processIdentifiers = [pid_t](
            repeating: 0,
            count: Int(requiredBytes) / stride + 8
        )
        let actualBytes = processIdentifiers.withUnsafeMutableBytes { buffer in
            proc_listpids(
                UInt32(PROC_PGRP_ONLY),
                UInt32(bitPattern: pid),
                buffer.baseAddress,
                Int32(buffer.count)
            )
        }
        guard actualBytes > 0 else { return false }
        return processIdentifiers.prefix(Int(actualBytes) / stride).contains { $0 > 0 && $0 != pid }
    }

    private func signalProcessGroup(_ signal: Int32) {
        _ = Darwin.kill(-pid, signal)
    }
}

private final class ProcessExitObservation: @unchecked Sendable {
    private let lock = NSLock()
    private var observed = false

    var hasExited: Bool {
        lock.lock()
        defer { lock.unlock() }
        return observed
    }

    func observeExit() {
        lock.lock()
        observed = true
        lock.unlock()
    }
}

private struct MonotonicDeadline {
    let uptimeNanoseconds: UInt64?

    init(timeout: TimeInterval?) {
        guard let timeout, timeout.isFinite else {
            uptimeNanoseconds = nil
            return
        }
        let nanoseconds = max(0, timeout) * 1_000_000_000
        let interval: UInt64
        if nanoseconds >= Double(UInt64.max) {
            interval = UInt64.max
        } else {
            interval = UInt64(nanoseconds)
        }
        let deadline = DispatchTime.now().uptimeNanoseconds.addingReportingOverflow(interval)
        uptimeNanoseconds = deadline.overflow ? UInt64.max : deadline.partialValue
    }

    var hasExpired: Bool {
        guard let uptimeNanoseconds else { return false }
        return DispatchTime.now().uptimeNanoseconds >= uptimeNanoseconds
    }

    var nextPollMilliseconds: Int32 {
        guard let uptimeNanoseconds else { return commandPollIntervalMilliseconds }
        let now = DispatchTime.now().uptimeNanoseconds
        guard uptimeNanoseconds > now else { return 0 }
        let remainingMilliseconds = (uptimeNanoseconds - now + 999_999) / 1_000_000
        return Int32(min(UInt64(commandPollIntervalMilliseconds), remainingMilliseconds))
    }
}

private struct POSIXFailure: Error, CustomStringConvertible {
    let operation: String
    let code: Int32

    var description: String {
        "\(operation) failed: \(String(cString: strerror(code))) (errno \(code))"
    }
}

private func validateCStringValues(_ values: [String]) throws {
    if values.contains(where: { $0.utf8.contains(0) }) {
        throw POSIXFailure(operation: "argument encoding", code: EINVAL)
    }
}

private func withMutableCStringArray<Result>(
    _ strings: [String],
    _ body: (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) throws -> Result
) throws -> Result {
    var allocated: [UnsafeMutablePointer<CChar>?] = try strings.map { value in
        guard let pointer = strdup(value) else {
            throw POSIXFailure(operation: "strdup", code: ENOMEM)
        }
        return pointer
    }
    defer {
        for pointer in allocated {
            free(pointer)
        }
    }
    allocated.append(nil)
    return try allocated.withUnsafeMutableBufferPointer { buffer in
        try body(buffer.baseAddress!)
    }
}

private func closeDescriptor(_ descriptor: inout Int32) {
    if descriptor >= 0 {
        _ = Darwin.close(descriptor)
        descriptor = -1
    }
}

private func decodeWaitStatus(_ status: Int32) -> Int32 {
    let signal = status & 0x7f
    return signal == 0 ? (status >> 8) & 0xff : signal
}
#endif
