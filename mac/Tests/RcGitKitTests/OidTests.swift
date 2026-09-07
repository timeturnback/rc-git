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
    }

    @Test func cLayoutRoundTrips() {
        let oid = oid(0xab)
        let back = oid.withC { Oid(c: $0.pointee) }
        #expect(back == oid)
        #expect(oid.withC { Int($0.pointee.len) } == 20)
    }
}
