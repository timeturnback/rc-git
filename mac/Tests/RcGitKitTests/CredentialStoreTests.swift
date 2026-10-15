import Foundation
import Security
import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct CredentialStoreTests {
    @Test(arguments: [
        (CredentialKind.httpsToken, kSecAttrProtocolHTTPS as String),
        (CredentialKind.sshPassphrase, kSecAttrProtocolSSH as String),
    ])
    func queryIsScopedToRcGitItems(kind: CredentialKind, expectedProtocol: String) throws {
        let query = KeychainCredentialStore.query(host: "example.com:8443", kind: kind)

        let domain = try #require(query[kSecAttrSecurityDomain as String] as? String)
        #expect(domain == "rc-git")
        let proto = try #require(query[kSecAttrProtocol as String] as? String)
        #expect(proto == expectedProtocol)
        let server = try #require(query[kSecAttrServer as String] as? String)
        #expect(server == "example.com:8443")
        let itemClass = try #require(query[kSecClass as String] as? String)
        #expect(itemClass == kSecClassInternetPassword as String)
    }
}
