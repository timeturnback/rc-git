/// A failure reported by the core. The message already names the operation and the object.
public struct RepositoryError: Error, Hashable, Sendable, CustomStringConvertible {
    public let message: String

    public init(_ message: String) {
        self.message = message
    }

    public var description: String { message }
}
