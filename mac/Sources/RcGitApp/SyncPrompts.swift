import AppKit
import RcGitKit

/// The two questions a network step can raise, as sheets on whichever window drives the
/// step. The repository window and the update window both ask through here, so a remote
/// asks for its secret the same way wherever the pull started.
@MainActor
enum SyncPrompts {
    /// Asks for a username and token (HTTPS) or the passphrase of the key file at `key`
    /// (ssh) for `host`. The secret field is secure; what the server accepts is
    /// remembered in the login keychain by the model.
    static func credentials(
        on window: NSWindow?, host: String, kind: CredentialKind, key: String?,
        completion: @escaping (Credentials?) -> Void
    ) {
        guard let window else {
            completion(nil)
            return
        }
        let alert = NSAlert()
        let userField = NSTextField(frame: NSRect(x: 0, y: 30, width: 260, height: 24))
        userField.placeholderString = "Username"
        let secretField = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        switch kind {
        case .httpsToken:
            alert.messageText = "Sign in to \(host)"
            alert.informativeText =
                "rc-git talks to HTTPS remotes with a personal access token. The token is "
                + "remembered in your login keychain once the server accepts it."
            secretField.placeholderString = "Token"
            let box = NSView(frame: NSRect(x: 0, y: 0, width: 260, height: 56))
            box.addSubview(userField)
            box.addSubview(secretField)
            alert.accessoryView = box
        case .sshPassphrase:
            alert.messageText = "Unlock the ssh key for \(host)"
            alert.informativeText =
                "The ssh key \(key.map { "'\($0)'" } ?? "for this remote") is encrypted, "
                + "or the passphrase given was wrong. The passphrase is remembered in your "
                + "login keychain once the server accepts the key."
            secretField.placeholderString = "Passphrase"
            let box = NSView(frame: NSRect(x: 0, y: 0, width: 260, height: 26))
            box.addSubview(secretField)
            alert.accessoryView = box
        }
        alert.addButton(withTitle: kind == .httpsToken ? "Sign In" : "Unlock")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { response in
            let username = userField.stringValue.trimmingCharacters(in: .whitespaces)
            let secret = secretField.stringValue
            guard response == .alertFirstButtonReturn, !secret.isEmpty else {
                completion(nil)
                return
            }
            if kind == .httpsToken, username.isEmpty {
                completion(nil)
                return
            }
            completion(Credentials(username: username, token: secret))
        }
    }

    /// Shows an unknown ssh host key for the user to verify before trusting.
    static func hostKey(
        on window: NSWindow?, host: String, hostKey: String,
        completion: @escaping (Bool) -> Void
    ) {
        guard let window else {
            completion(false)
            return
        }
        let alert = NSAlert()
        alert.messageText = "Trust \(host)?"
        alert.informativeText =
            "This host is not in known_hosts. Its key is:\n\n\(hostKey)\n\n"
            + "Verify the fingerprint through a trusted channel before continuing. "
            + "rc-git remembers the approval for this host; known_hosts is never modified."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Trust and Continue")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { response in
            completion(response == .alertFirstButtonReturn)
        }
    }
}
