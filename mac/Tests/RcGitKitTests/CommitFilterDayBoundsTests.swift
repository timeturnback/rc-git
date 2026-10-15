import Foundation
import Testing

@testable import RcGitKit

/// How a Since/Until day pick becomes the filter's inclusive instants.
@Suite(.timeLimit(.minutes(1))) struct CommitFilterDayBoundsTests {
    private func calendar(_ zone: String) throws -> Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = try #require(TimeZone(identifier: zone))
        return calendar
    }

    private func instant(
        _ calendar: Calendar, _ year: Int, _ month: Int, _ day: Int, _ hour: Int,
        _ minute: Int = 0, _ second: Int = 0
    ) throws -> Date {
        let parts = DateComponents(
            year: year, month: month, day: day, hour: hour, minute: minute, second: second)
        return try #require(calendar.date(from: parts))
    }

    private func epoch(_ date: Date) -> Int64 {
        Int64(date.timeIntervalSince1970)
    }

    @Test func sinceMapsToLocalMidnight() throws {
        let calendar = try calendar("Asia/Ho_Chi_Minh")
        let since = try instant(calendar, 2026, 10, 13, 15, 30)
        let bounds = CommitFilter.dayBounds(since: since, until: nil, calendar: calendar)
        #expect(bounds.after == epoch(try instant(calendar, 2026, 10, 13, 0)))
        #expect(bounds.before == nil)
    }

    @Test func untilMapsToTheLastSecondOfTheDay() throws {
        let calendar = try calendar("Asia/Ho_Chi_Minh")
        let until = try instant(calendar, 2026, 10, 13, 9)
        let bounds = CommitFilter.dayBounds(since: nil, until: until, calendar: calendar)
        #expect(bounds.before == epoch(try instant(calendar, 2026, 10, 13, 23, 59, 59)))
        #expect(bounds.after == nil)
    }

    @Test func aLaterCommitTheSameDayIsInsideUntil() throws {
        let calendar = try calendar("Asia/Ho_Chi_Minh")
        let picked = try instant(calendar, 2026, 10, 13, 9)
        let commit = epoch(try instant(calendar, 2026, 10, 13, 10))
        let bounds = CommitFilter.dayBounds(since: picked, until: picked, calendar: calendar)
        let after = try #require(bounds.after)
        let before = try #require(bounds.before)
        #expect(after <= commit && commit <= before)
    }

    @Test func aSpringForwardDaySpansTwentyThreeHours() throws {
        let calendar = try calendar("America/New_York")
        let day = try instant(calendar, 2026, 3, 8, 12)
        let bounds = CommitFilter.dayBounds(since: day, until: day, calendar: calendar)
        let after = try #require(bounds.after)
        let before = try #require(bounds.before)
        #expect(before - after + 1 == 23 * 3600)
    }

    @Test func aFallBackDaySpansTwentyFiveHours() throws {
        let calendar = try calendar("America/New_York")
        let day = try instant(calendar, 2026, 11, 1, 12)
        let bounds = CommitFilter.dayBounds(since: day, until: day, calendar: calendar)
        let after = try #require(bounds.after)
        let before = try #require(bounds.before)
        #expect(before - after + 1 == 25 * 3600)
    }

    @Test func noDatesGiveNoBounds() {
        let bounds = CommitFilter.dayBounds(since: nil, until: nil, calendar: .current)
        #expect(bounds.after == nil && bounds.before == nil)
    }
}
