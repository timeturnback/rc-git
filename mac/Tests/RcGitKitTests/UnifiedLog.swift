import Foundation
import OSLog

/// Reads this test process's own unified-log entries. A read scans the store for seconds, so
/// it runs on a detached task: on the main actor it would stall every main-actor suite running
/// beside it and break their timing.
enum UnifiedLog {
    /// Where to start reading: a second before now, so an entry written right after is in.
    static func mark() -> Date {
        Date().addingTimeInterval(-1)
    }

    /// The messages logged at `.error` under `category` since `date`.
    static func errors(category: String, since date: Date) async throws -> [String] {
        try await Task.detached(priority: .utility) {
            let store = try OSLogStore(scope: .currentProcessIdentifier)
            return try store.getEntries(at: store.position(date: date))
                .compactMap { $0 as? OSLogEntryLog }
                .filter { $0.category == category && $0.level == .error }
                .map(\.composedMessage)
        }.value
    }
}
