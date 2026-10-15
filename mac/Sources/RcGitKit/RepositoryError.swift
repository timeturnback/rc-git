import CRcGit

/// A failure reported by the core. The message already names the operation and the object.
public struct RepositoryError: Error, Hashable, Sendable, CustomStringConvertible {
    public let message: String

    public init(_ message: String) {
        self.message = message
    }

    public var description: String { message }
}

extension RepositoryError {
    /// Takes ownership of a core error string, frees it, and wraps it; `fallback` stands in
    /// when the core set no message.
    static func take(_ error: UnsafeMutablePointer<CChar>?, fallback: String) -> RepositoryError {
        RepositoryError(takeString(error) ?? fallback)
    }

    /// Takes ownership of a string the core returned and frees it with `rcgit_string_free`.
    static func takeString(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { rcgit_string_free(pointer) }
        return String(cString: pointer)
    }
}

extension RepositoryError {
    /// `error` itself when it already is a `RepositoryError`, else its description.
    init(wrapping error: any Error) {
        self = (error as? RepositoryError) ?? RepositoryError(String(describing: error))
    }

    /// Runs a blocking repository call through `BlockingWork` and returns its outcome, mapping
    /// any error through `init(wrapping:)`. `body` must capture only Sendable locals, never a
    /// model.
    static func run<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async
        -> Result<T, RepositoryError>
    {
        await BlockingWork.run { () -> Result<T, RepositoryError> in
            do {
                return .success(try body())
            } catch {
                return .failure(RepositoryError(wrapping: error))
            }
        }
    }
}
