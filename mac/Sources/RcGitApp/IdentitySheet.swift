import AppKit
import RcGitKit

/// The sheet that asks for, edits, or clears a commit identity. Save stays disabled while
/// `CommitIdentity.parse` refuses the input, and the reason shows under the fields, so an
/// identity git cannot record is never handed back.
@MainActor
final class IdentitySheet: NSObject, NSTextFieldDelegate {
    enum Outcome {
        case save(CommitIdentity)
        case clear
    }

    private let alert = NSAlert()
    private let nameField = NSTextField(frame: NSRect(x: 0, y: 64, width: 300, height: 24))
    private let emailField = NSTextField(frame: NSRect(x: 0, y: 34, width: 300, height: 24))
    private let problemLabel = NSTextField(wrappingLabelWithString: "")

    /// Shows the sheet on `window`. `completion` runs only for Save with a valid identity, or
    /// for Clear when `allowsClear` offered it; Cancel calls nothing.
    static func present(
        on window: NSWindow, title: String, message: String, initial: CommitIdentity?,
        allowsClear: Bool, completion: @escaping (Outcome) -> Void
    ) {
        let sheet = IdentitySheet()
        sheet.configure(title: title, message: message, initial: initial, allowsClear: allowsClear)
        // The handler holds the sheet, which the fields only reference weakly as delegate.
        sheet.alert.beginSheetModal(for: window) { response in
            sheet.finish(response, completion: completion)
        }
        sheet.alert.window.makeFirstResponder(sheet.nameField)
    }

    private func configure(
        title: String, message: String, initial: CommitIdentity?, allowsClear: Bool
    ) {
        alert.messageText = title
        alert.informativeText = message
        nameField.placeholderString = "Name"
        nameField.stringValue = initial?.name ?? ""
        nameField.delegate = self
        emailField.placeholderString = "email@example.com"
        emailField.stringValue = initial?.email ?? ""
        emailField.delegate = self
        problemLabel.frame = NSRect(x: 0, y: 0, width: 300, height: 28)
        problemLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        problemLabel.textColor = .systemRed
        let box = NSView(frame: NSRect(x: 0, y: 0, width: 300, height: 88))
        box.addSubview(nameField)
        box.addSubview(emailField)
        box.addSubview(problemLabel)
        alert.accessoryView = box
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")
        if allowsClear {
            alert.addButton(withTitle: "Clear")
        }
        validate()
    }

    func controlTextDidChange(_ notification: Notification) {
        validate()
    }

    private var parsed: Result<CommitIdentity, IdentityProblem> {
        CommitIdentity.parse(name: nameField.stringValue, email: emailField.stringValue)
    }

    private func validate() {
        let untouched = nameField.stringValue.isEmpty && emailField.stringValue.isEmpty
        switch parsed {
        case .success:
            alert.buttons.first?.isEnabled = true
            problemLabel.stringValue = ""
        case let .failure(problem):
            alert.buttons.first?.isEnabled = false
            problemLabel.stringValue = untouched ? "" : problem.message
        }
    }

    private func finish(_ response: NSApplication.ModalResponse, completion: (Outcome) -> Void) {
        switch response {
        case .alertFirstButtonReturn:
            if case let .success(identity) = parsed { completion(.save(identity)) }
        case .alertThirdButtonReturn:
            completion(.clear)
        default:
            break
        }
    }
}
