import Foundation

public struct InitOptions: Sendable {
    public var catalogOnly: Bool
    public var progress: String?

    public init(catalogOnly: Bool = false, progress: String? = "none") {
        self.catalogOnly = catalogOnly
        self.progress = progress
    }
}

public struct ImportOptions: Sendable {
    public var all: Bool
    public var provider: String?
    public var path: String?
    public var resume: Bool
    public var progress: String?

    public init(
        all: Bool = false,
        provider: String? = nil,
        path: String? = nil,
        resume: Bool = false,
        progress: String? = "none"
    ) {
        self.all = all
        self.provider = provider
        self.path = path
        self.resume = resume
        self.progress = progress
    }
}

public struct SearchOptions: Sendable {
    public var terms: [String]
    public var limit: Int?
    public var provider: String?
    public var workspace: String?
    public var since: String?
    public var primaryOnly: Bool
    public var includeSubagents: Bool
    public var eventType: String?
    public var file: String?
    public var session: String?
    public var events: Bool
    public var refresh: String?
    public var includeCurrentSession: Bool

    public init(
        terms: [String] = [],
        limit: Int? = nil,
        provider: String? = nil,
        workspace: String? = nil,
        since: String? = nil,
        primaryOnly: Bool = false,
        includeSubagents: Bool = false,
        eventType: String? = nil,
        file: String? = nil,
        session: String? = nil,
        events: Bool = false,
        refresh: String? = nil,
        includeCurrentSession: Bool = false
    ) {
        self.terms = terms
        self.limit = limit
        self.provider = provider
        self.workspace = workspace
        self.since = since
        self.primaryOnly = primaryOnly
        self.includeSubagents = includeSubagents
        self.eventType = eventType
        self.file = file
        self.session = session
        self.events = events
        self.refresh = refresh
        self.includeCurrentSession = includeCurrentSession
    }
}

public struct ShowEventOptions: Sendable {
    public var before: Int?
    public var after: Int?
    public var window: Int?

    public init(before: Int? = nil, after: Int? = nil, window: Int? = nil) {
        self.before = before
        self.after = after
        self.window = window
    }
}

public struct ShowSessionOptions: Sendable {
    public var id: String?
    public var provider: String?
    public var providerSession: String?
    public var mode: String?

    public init(id: String? = nil, provider: String? = nil, providerSession: String? = nil, mode: String? = nil) {
        self.id = id
        self.provider = provider
        self.providerSession = providerSession
        self.mode = mode
    }
}

public struct LocateSessionOptions: Sendable {
    public var id: String?
    public var provider: String?
    public var providerSession: String?

    public init(id: String? = nil, provider: String? = nil, providerSession: String? = nil) {
        self.id = id
        self.provider = provider
        self.providerSession = providerSession
    }
}

public struct HostedConfig: Sendable {
    public var baseURL: URL?
    public var apiKey: String?

    public init(baseURL: URL? = nil, apiKey: String? = nil) {
        self.baseURL = baseURL
        self.apiKey = apiKey
    }
}
