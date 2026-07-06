import Foundation

public struct StatusResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var status: AgentHistoryStatus

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let status = envelope.status else {
            throw missingPayload("status", operation: envelope.operation)
        }
        self.envelope = envelope
        self.status = status
    }
}

public struct InitResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var status: AgentHistoryStatus

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let status = envelope.status else {
            throw missingPayload("status", operation: envelope.operation)
        }
        self.envelope = envelope
        self.status = status
    }
}

public struct SourcesResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var sources: [ProviderSource]

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let sources = envelope.sources else {
            throw missingPayload("sources", operation: envelope.operation)
        }
        self.envelope = envelope
        self.sources = sources
    }
}

public struct ImportResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var importResult: AgentHistoryImportResult

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let importResult = envelope.importResult else {
            throw missingPayload("import", operation: envelope.operation)
        }
        self.envelope = envelope
        self.importResult = importResult
    }
}

public struct SearchResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var search: AgentHistorySearchResult

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let search = envelope.search else {
            throw missingPayload("search", operation: envelope.operation)
        }
        self.envelope = envelope
        self.search = search
    }
}

public struct ShowEventResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var event: AgentHistoryEventResult

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let event = envelope.event else {
            throw missingPayload("event", operation: envelope.operation)
        }
        self.envelope = envelope
        self.event = event
    }
}

public struct ShowSessionResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var session: AgentHistorySessionResult

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let session = envelope.session else {
            throw missingPayload("session", operation: envelope.operation)
        }
        self.envelope = envelope
        self.session = session
    }
}

public struct LocateEventResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var location: AgentHistoryLocationResult

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let location = envelope.location else {
            throw missingPayload("location", operation: envelope.operation)
        }
        self.envelope = envelope
        self.location = location
    }
}

public struct LocateSessionResponse: Equatable, Sendable {
    public var envelope: AgentHistoryEnvelope
    public var location: AgentHistoryLocationResult

    public init(envelope: AgentHistoryEnvelope) throws {
        guard let location = envelope.location else {
            throw missingPayload("location", operation: envelope.operation)
        }
        self.envelope = envelope
        self.location = location
    }
}

private func missingPayload(_ payload: String, operation: AgentHistoryOperation) -> CtxAgentHistorySDKError {
    CtxAgentHistorySDKError(
        code: .decodeError,
        message: "agent-history-v1 \(operation.rawValue) response did not contain \(payload) payload"
    )
}
