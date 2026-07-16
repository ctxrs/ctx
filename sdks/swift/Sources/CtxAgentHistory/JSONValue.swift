import Foundation

public enum JSONValue: Codable, Equatable, Sendable, CustomStringConvertible {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Int.self) {
            self = .number(Double(value))
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try container.decode([String: JSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null:
            try container.encodeNil()
        case let .bool(value):
            try container.encode(value)
        case let .number(value):
            try container.encode(value)
        case let .string(value):
            try container.encode(value)
        case let .array(value):
            try container.encode(value)
        case let .object(value):
            try container.encode(value)
        }
    }

    public subscript(key: String) -> JSONValue? {
        guard case let .object(object) = self else {
            return nil
        }
        return object[key]
    }

    public var stringValue: String? {
        if case let .string(value) = self {
            return value
        }
        return nil
    }

    public var boolValue: Bool? {
        if case let .bool(value) = self {
            return value
        }
        return nil
    }

    public var intValue: Int? {
        if case let .number(value) = self {
            return Int(exactly: value)
        }
        return nil
    }

    public var arrayValue: [JSONValue]? {
        if case let .array(value) = self {
            return value
        }
        return nil
    }

    public var objectValue: [String: JSONValue]? {
        if case let .object(value) = self {
            return value
        }
        return nil
    }

    public var description: String {
        switch self {
        case .null:
            return "null"
        case let .bool(value):
            return String(value)
        case let .number(value):
            return String(value)
        case let .string(value):
            return value
        case let .array(value):
            return String(describing: value)
        case let .object(value):
            return String(describing: value)
        }
    }
}

extension JSONValue {
    static func from(_ value: Any) throws -> JSONValue {
        let data = try JSONSerialization.data(withJSONObject: value, options: [])
        return try JSONDecoder().decode(JSONValue.self, from: data)
    }

    func camelizedPublicJSON() -> JSONValue {
        switch self {
        case let .array(values):
            return .array(values.map { $0.camelizedPublicJSON() })
        case let .object(object):
            var result: [String: JSONValue] = [:]
            for (key, value) in object where !Self.omittedPublicKeys.contains(key) {
                let publicKey = Self.snakeToCamel(key)
                result[publicKey] = value.camelizedPublicJSON()
            }
            return .object(result)
        default:
            return self
        }
    }

    func droppingNulls() -> JSONValue {
        switch self {
        case let .array(values):
            return .array(values.map { $0.droppingNulls() })
        case let .object(object):
            var result: [String: JSONValue] = [:]
            for (key, value) in object {
                if case .null = value {
                    continue
                }
                result[key] = value.droppingNulls()
            }
            return .object(result)
        default:
            return self
        }
    }

    private static let omittedPublicKeys = Set([
        "schema_version",
        "target",
        "item_type",
        "itemType",
        "payload_type",
        "payloadType",
        "record_type",
        "recordType",
        "semantic_weight",
        "semanticWeight",
        "semantic_fallback_code",
        "semanticFallbackCode",
        "semantic_fallback",
        "semanticFallback"
    ])

    private static func snakeToCamel(_ value: String) -> String {
        let parts = value.split(separator: "_", omittingEmptySubsequences: false)
        guard let first = parts.first else {
            return value
        }
        return parts.dropFirst().reduce(String(first)) { partial, part in
            partial + part.prefix(1).uppercased() + part.dropFirst()
        }
    }
}
