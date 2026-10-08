import CRcGit

/// Identity of a git object. Content-addressed: equal ids mean equal content, so caches keyed
/// by `Oid` never go stale.
public struct Oid: Hashable, Sendable, CustomStringConvertible {
    public let bytes: [UInt8]

    public init(bytes: [UInt8]) {
        self.bytes = bytes
    }

    /// Parses a full lowercase or uppercase hex id. `nil` for anything else.
    public init?(hex: String) {
        guard hex.count % 2 == 0, hex.count == 40 || hex.count == 64 else { return nil }
        var bytes: [UInt8] = []
        bytes.reserveCapacity(hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            bytes.append(byte)
            index = next
        }
        self.bytes = bytes
    }

    init(c: RcGitOid) {
        let count = Int(c.len)
        self.bytes = withUnsafeBytes(of: c.bytes) { Array($0.prefix(count)) }
    }

    /// Lowercase hexadecimal form.
    public var hex: String {
        bytes.map { byte in
            let s = String(byte, radix: 16)
            return byte < 16 ? "0" + s : s
        }.joined()
    }

    /// The first 7 hex digits, the way `git log --oneline` shows them.
    public var short: String { String(hex.prefix(7)) }

    public var description: String { hex }

    /// Stand-in id for a working-tree file, whose bytes were never hashed. Every unhashed file
    /// shares it, so an `Oid`-keyed cache must skip any entry where `isUnhashed` is true.
    public static let unhashed = Oid(bytes: [])

    /// True for `Oid.unhashed`: the id names no object and cannot key a cache.
    public var isUnhashed: Bool { bytes.isEmpty }

    func withC<T>(_ body: (UnsafePointer<RcGitOid>) throws -> T) rethrows -> T {
        var c = RcGitOid()
        withUnsafeMutableBytes(of: &c.bytes) { buffer in
            for (offset, byte) in bytes.prefix(32).enumerated() {
                buffer[offset] = byte
            }
        }
        c.len = UInt8(min(bytes.count, 32))
        return try withUnsafePointer(to: &c, body)
    }
}

/// Runs `body` with `oid` as a C pointer, or with NULL when `oid` is `nil`.
func withOptionalOid<T>(
    _ oid: Oid?, _ body: (UnsafePointer<RcGitOid>?) throws -> T
) rethrows -> T {
    guard let oid else { return try body(nil) }
    return try oid.withC { try body($0) }
}
