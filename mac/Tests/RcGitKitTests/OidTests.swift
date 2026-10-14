import Testing

@testable import RcGitKit

@Suite struct OidTests {
    @Test func hexRoundTrips() throws {
        let hex = "0123456789abcdef0123456789abcdef01234567"
        let oid = try #require(Oid(hex: hex))
        #expect(oid.hex == hex)
        #expect(oid.short == "0123456")
        #expect(oid.bytes.count == 20)
        #expect(Oid(hex: hex.uppercased()) == oid)
        #expect(oid.description == hex)
    }

    @Test func rejectsMalformedHex() {
        #expect(Oid(hex: "abc") == nil)
        #expect(Oid(hex: String(repeating: "g", count: 40)) == nil)
        #expect(Oid(hex: "") == nil)
        #expect(Oid(hex: String(repeating: "+f", count: 20)) == nil)
        #expect(Oid(hex: String(repeating: "-0", count: 20)) == nil)
    }

    @Test func cLayoutRoundTrips() {
        let oid = oid(0xab)
        let back = oid.withC { Oid(c: $0.pointee) }
        #expect(back == oid)
        #expect(oid.withC { Int($0.pointee.len) } == 20)
    }

    /// The id lives inline: no heap buffer per id, so a 200k-entry path index is 200k values,
    /// not 200k allocations.
    @Test func storesBytesInline() {
        #expect(MemoryLayout<Oid>.size == 33)
    }

    /// Only the meaningful bytes and their count decide identity: a zero-length id differs from
    /// an all-zero 20-byte one, and ids that differ in their last byte differ.
    @Test func equalityAndHashCoverCountAndEveryUsedByte() {
        let zeros = Oid(bytes: [UInt8](repeating: 0, count: 20))
        #expect(Oid.unhashed != zeros)
        #expect(Oid.unhashed.isUnhashed)
        #expect(!zeros.isUnhashed)
        var last = [UInt8](repeating: 0, count: 20)
        last[19] = 1
        #expect(Oid(bytes: last) != zeros)
        #expect(Oid(bytes: last).bytes == last)
        #expect(Set([zeros, Oid(bytes: last), Oid.unhashed, zeros]).count == 3)
    }
}
