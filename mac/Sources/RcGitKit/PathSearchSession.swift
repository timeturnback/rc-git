/// One Go to File query session: runs each query, keeps only the newest one's result, and
/// holds a Return pressed while that result is still in flight until it lands, so a fast typist
/// opens the first match of what they typed rather than a row of the older list (or nothing).
@MainActor
public final class PathSearchSession {
    public typealias Provider = (String) async throws -> PathSearchResult

    /// What the newest query produced, for the view to render.
    public enum Update {
        case results(truncated: Bool)
        case failed(any Error)
    }

    public var provider: Provider?
    /// Fires on the main actor when the newest query lands; `results` is already updated.
    public var onUpdate: ((Update) -> Void)?
    public var onOpen: ((PathIndexEntry) -> Void)?

    /// The list on screen. It belongs to the newest query only while no search is in flight.
    public private(set) var results: [PathIndexEntry] = []
    private var searchGeneration = 0
    private var appliedGeneration = 0
    private var openWhenResultsLand = false

    public init() {}

    /// Empties the list and drops any pending open and any search still in flight.
    public func reset() {
        searchGeneration += 1
        results = []
        openWhenResultsLand = false
    }

    /// Drops a Return that is waiting for results, as when the panel closes.
    public func cancelPendingOpen() {
        openWhenResultsLand = false
    }

    /// Starts `query`; any older query still running is ignored when it lands.
    ///
    /// - Returns: The task applying the result, or nil when there is no provider.
    @discardableResult
    public func search(_ query: String) -> Task<Void, Never>? {
        guard let provider else { return nil }
        searchGeneration += 1
        let generation = searchGeneration
        return Task { @MainActor in
            do {
                let result = try await provider(query)
                guard generation == self.searchGeneration else { return }
                self.results = result.entries
                self.appliedGeneration = generation
                self.onUpdate?(.results(truncated: result.truncated))
                if self.openWhenResultsLand {
                    self.openWhenResultsLand = false
                    self.open(row: 0)
                }
            } catch is CancellationError {
            } catch {
                guard generation == self.searchGeneration else { return }
                self.results = []
                self.appliedGeneration = generation
                self.openWhenResultsLand = false
                self.onUpdate?(.failed(error))
            }
        }
    }

    /// Opens `row` of the list on screen, or, while the newest query is still in flight, its
    /// first match once it lands. A row outside the list opens nothing.
    public func open(row: Int) {
        guard appliedGeneration == searchGeneration else {
            openWhenResultsLand = true
            return
        }
        guard results.indices.contains(row) else { return }
        onOpen?(results[row])
    }
}
