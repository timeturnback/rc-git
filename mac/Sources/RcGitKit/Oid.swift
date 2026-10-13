import CRcGit

/// Identity of a git object. Content-addressed: equal ids mean equal content, so caches keyed
/// by `Oid` never go stale.
public struct Oid: Hashable, Sendable, CustomStringConvertible {
    public let bytes: [UInt8]

    public init(bytes: [UInt8]) {
        self.bytes = bytes
    }

    /// Parses a full 40-digit lowercase or uppercase hex id. `nil` for anything else.
    public init?(hex: String) {
        let units = Array(hex.utf8)
        guard units.count == 40 else { return nil }
        var bytes: [UInt8] = []
        bytes.reserveCapacity(units.count / 2)
        for pair in stride(from: 0, to: units.count, by: 2) {
            guard let high = Self.nibble(units[pair]), let low = Self.nibble(units[pair + 1])
            else { return nil }
            bytes.append(high << 4 | low)
        }
        self.bytes = bytes
    }

    /// The value of one ASCII hex digit, or `nil` for any other code unit.
    private static func nibble(_ unit: UInt8) -> UInt8? {
        switch unit {
        case 0x30...0x39: return unit - 0x30
        case 0x41...0x46: return unit - 0x41 + 10
        case 0x61...0x66: return unit - 0x61 + 10
        default: return nil
        }
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
