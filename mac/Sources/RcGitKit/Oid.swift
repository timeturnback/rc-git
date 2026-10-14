import CRcGit

/// Identity of a git object. Content-addressed: equal ids mean equal content, so caches keyed
/// by `Oid` never go stale.
public struct Oid: Hashable, Sendable, CustomStringConvertible {
    /// The id bytes, zero-padded to RcGitOid's 32, held inline so an id costs no allocation.
    private var storage: (UInt64, UInt64, UInt64, UInt64) = (0, 0, 0, 0)
    /// Number of meaningful bytes in `storage`.
    private let count: UInt8

    /// Creates an id from its raw bytes. At most 32: the widest hash RcGitOid carries.
    public init(bytes: [UInt8]) {
        precondition(bytes.count <= 32, "Oid(bytes:) takes at most 32 bytes, got \(bytes.count)")
        count = UInt8(bytes.count)
        withUnsafeMutableBytes(of: &storage) { $0.copyBytes(from: bytes) }
    }

    /// Parses a full 40-digit SHA-1 hex id, lowercase or uppercase. `nil` for anything else.
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
        self.init(bytes: bytes)
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
        count = min(c.len, 32)
        let used = Int(count)
        withUnsafeMutableBytes(of: &storage) { target in
            withUnsafeBytes(of: c.bytes) {
                target.copyMemory(from: UnsafeRawBufferPointer(rebasing: $0.prefix(used)))
            }
        }
    }

    /// The meaningful bytes, as a fresh array.
    public var bytes: [UInt8] {
        withUnsafeBytes(of: storage) { Array($0.prefix(Int(count))) }
    }

    public static func == (lhs: Oid, rhs: Oid) -> Bool {
        lhs.count == rhs.count && lhs.storage == rhs.storage
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(count)
        hasher.combine(storage.0)
        hasher.combine(storage.1)
        hasher.combine(storage.2)
        hasher.combine(storage.3)
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
    public var isUnhashed: Bool { count == 0 }

    func withC<T>(_ body: (UnsafePointer<RcGitOid>) throws -> T) rethrows -> T {
        var c = RcGitOid()
        withUnsafeMutableBytes(of: &c.bytes) { target in
            withUnsafeBytes(of: storage) { target.copyMemory(from: $0) }
        }
        c.len = count
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
