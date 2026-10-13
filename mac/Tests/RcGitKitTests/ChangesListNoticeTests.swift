import Testing

@testable import RcGitKit

/// What the change list says in place of rows, and whether its rows are current.
@Suite struct ChangesListNoticeTests {
    private let entry = StatusEntry(
        path: "a.txt", kind: .blob, status: .modified, oid: nil, size: 1)

    @Test func firstScanSaysScanningNotClean() {
        let notice = ChangesState.loading.notice(showingRows: false)
        #expect(notice.placeholder == ChangesState.scanningPlaceholder)
        #expect(!notice.isCurrent)
    }

    @Test func beforeAnyScanSaysScanning() {
        let notice = ChangesState.idle.notice(showingRows: false)
        #expect(notice.placeholder == ChangesState.scanningPlaceholder)
        #expect(!notice.isCurrent)
    }

    @Test func leftoverRowsStayButAreNotCurrent() {
        for state in [ChangesState.idle, .loading] {
            let notice = state.notice(showingRows: true)
            #expect(notice.placeholder == nil, "\(state)")
            #expect(!notice.isCurrent, "\(state)")
        }
    }

    @Test func onlyAnEmptyLoadedReportSaysClean() {
        let report = StatusReport(entries: [], scanned: 0, truncated: false)
        let notice = ChangesState.loaded(report).notice(showingRows: false)
        #expect(notice.placeholder == ChangesState.cleanPlaceholder)
        #expect(notice.isCurrent)
    }

    @Test func loadedReportWithEntriesHasNoPlaceholder() {
        let report = StatusReport(entries: [entry], scanned: 1, truncated: false)
        let notice = ChangesState.loaded(report).notice(showingRows: true)
        #expect(notice.placeholder == nil)
        #expect(notice.isCurrent)
    }

    @Test func failureShowsItsMessage() {
        let notice = ChangesState.failed("status failed").notice(showingRows: false)
        #expect(notice.placeholder == "status failed")
        #expect(notice.isCurrent)
    }
}
