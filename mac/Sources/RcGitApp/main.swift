import AppKit
import RcGitKit

if AppBuild.current.variant == .dev {
    UserDefaults.standard.register(defaults: ["NSApplicationCrashOnExceptions": true])
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
